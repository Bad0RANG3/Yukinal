import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { AGENT_PERMISSION_MODES, AGENT_RUN_MODES, IPC_COMMANDS, isReadOnlyRunMode } from "@yukinal/shared";
import { callDesktop } from "../src/lib/ipc.js";
import { useWorkspaceStore } from "../src/stores/workspace-store.js";
import { buildServerInput, type ServerFormValues } from "../src/features/servers/server-form.js";
import { RunLifecycle } from "../src/features/agent/run-lifecycle.js";
import {
  appendAssistantDelta,
  appendEntries,
  appendToolCall,
  entriesFromMessages,
  MAX_TRANSCRIPT_ENTRIES,
  sessionTitleFromPrompt,
  settleAssistantText,
  settleToolResult,
  targetLabel,
  toolResultSummary,
  type Entry,
  type ToolCallFacts,
} from "../src/features/agent/transcript.js";

Object.defineProperty(globalThis, "window", { value: {}, configurable: true });
afterEach(() => {
  clearMocks();
  useWorkspaceStore.setState(useWorkspaceStore.getInitialState(), true);
});

test("server selection opens its workspace without resetting the current server's tab", () => {
  const workspace = useWorkspaceStore.getState();
  workspace.selectServer("srv_first");
  workspace.setServerPage("files");
  workspace.selectServer("srv_first");
  assert.equal(useWorkspaceStore.getState().serverPage, "files");
  workspace.setPrimary("settings");
  workspace.selectServer("srv_second");
  assert.equal(useWorkspaceStore.getState().primary, "servers");
  assert.equal(useWorkspaceStore.getState().serverPage, "overview");
});

test("server form commands match the Rust input argument; other commands stay flat", async () => {
  const calls: unknown[] = [];
  const server = {
    id: "srv_test",
    name: "Test",
    connection: { host: "example.test", port: 22, username: "deploy" },
    capabilities: {},
    status: "disconnected",
    metadata: { environment: "staging" },
    createdAt: "2026-01-01T00:00:00Z",
    updatedAt: "2026-01-01T00:00:00Z",
  };
  mockIPC((command, args) => {
    calls.push({ command, args });
    if (command === "server_add" || command === "server_update") return { server };
    if (command === "server_connect") return { status: "connected" };
    return {};
  });
  const input = {
    name: "Test", host: "example.test", port: 22, username: "deploy",
    environment: "staging" as const,
    authentication: { method: "password" as const, password: "test-only" },
  };
  await callDesktop(IPC_COMMANDS.serverAdd, input);
  await callDesktop(IPC_COMMANDS.serverUpdate, { ...input, serverId: "srv_test" });
  await callDesktop(IPC_COMMANDS.serverConnect, { serverId: "srv_test" });
  assert.deepEqual(calls, [
    { command: "server_add", args: { input } },
    { command: "server_update", args: { input: { ...input, serverId: "srv_test" } } },
    { command: "server_connect", args: { serverId: "srv_test" } },
  ]);
});

test("native string errors become visible Error messages", async () => {
  mockIPC(() => Promise.reject("SSH authentication failed"));
  await assert.rejects(callDesktop(IPC_COMMANDS.serverConnect, { serverId: "srv_test" }), {
    name: "Error", message: "SSH authentication failed",
  });
});

const formValues: ServerFormValues = {
  name: " staging ", host: "example.test", port: "2222", username: "deploy", environment: "staging",
  authMethod: "password", password: "secret", privateKeyPem: "",
};

test("server form normalizes values and validates port and first-time credentials", () => {
  assert.deepEqual(buildServerInput(formValues), {
    name: "staging", host: "example.test", port: 2222, username: "deploy", environment: "staging",
    authentication: { method: "password", password: "secret" },
  });
  assert.deepEqual(buildServerInput({ ...formValues, password: "" }, "srv_existing"), {
    name: "staging", host: "example.test", port: 2222, username: "deploy", environment: "staging", serverId: "srv_existing",
  });
  assert.throws(() => buildServerInput({ ...formValues, port: "0" }), /端口/);
  assert.throws(() => buildServerInput({ ...formValues, password: "" }), /SSH 密码/);
});

test("server form validation uses the UTF-8 Chinese messages shown in the desktop form", () => {
  assert.throws(() => buildServerInput({ ...formValues, port: "0" }), {
    message: "端口必须是 1 到 65535 之间的整数。",
  });
  assert.throws(() => buildServerInput({ ...formValues, name: "" }), {
    message: "请填写名称、主机和用户名。",
  });
  assert.throws(() => buildServerInput({ ...formValues, password: "" }), {
    message: "请填写 SSH 密码。",
  });
  assert.throws(() => buildServerInput({ ...formValues, authMethod: "privateKey", password: "", privateKeyPem: "" }), {
    message: "请填写 SSH 私钥。",
  });
});

test("run lifecycle accepts one start and ignores duplicate or stale terminal events", () => {
  const lifecycle = new RunLifecycle();
  assert.equal(lifecycle.begin("run_a"), true);
  assert.equal(lifecycle.begin(), false);
  assert.equal(lifecycle.started("run_other"), false);
  assert.equal(lifecycle.started("run_a"), true);
  assert.equal(lifecycle.started("run_b"), false);
  assert.equal(lifecycle.isActive("run_b"), false);
  assert.equal(lifecycle.finish("run_b"), false);
  assert.equal(lifecycle.finish("run_a"), true);
  assert.equal(lifecycle.finish("run_a"), false);
});

test("transcript caps history without dropping the newest entries", () => {
  const full: Entry[] = Array.from({ length: MAX_TRANSCRIPT_ENTRIES }, (_value, index) => ({ kind: "user", text: `u${index}` }));
  const next = appendEntries(full, [{ kind: "assistant", text: "latest" }]);
  assert.equal(next.length, MAX_TRANSCRIPT_ENTRIES);
  assert.deepEqual(next.at(-1), { kind: "assistant", text: "latest" });
  assert.deepEqual(next[0], { kind: "user", text: "u1" });
});

test("streaming deltas accumulate into the trailing assistant line", () => {
  const first = appendAssistantDelta([{ kind: "user", text: "hi" }], "你");
  const second = appendAssistantDelta(first, "好");
  assert.deepEqual(second, [{ kind: "user", text: "hi" }, { kind: "assistant", text: "你好" }]);

  // An empty delta must not open a spurious empty assistant row.
  assert.equal(appendAssistantDelta(second, ""), second);
});

test("a delta arriving after a tool card opens a fresh assistant line", () => {
  const entries: Entry[] = [
    { kind: "assistant", text: "先看一下" },
    { kind: "tool", callId: "c1", toolName: "docker.ps", target: "local", riskLevel: "read", decision: "auto" },
  ];
  assert.deepEqual(appendAssistantDelta(entries, "继续"), [
    ...entries,
    { kind: "assistant", text: "继续" },
  ]);
});

import {
  AGENT_COMMANDS,
  commandAvailable,
  commandHelpText,
  findActiveTrigger,
  matchCommands,
  matchMentions,
  replaceTrigger,
  resolveMentionedServer,
  resolveSubmission,
  unavailableReason,
  type CommandContext,
  type MentionCandidate,
} from "../src/features/agent/composer-triggers.js";

const idleDesktop: CommandContext = { running: false, archived: false, desktop: true };
const runningDesktop: CommandContext = { running: true, archived: false, desktop: true };

test("a trigger is only live while the caret sits inside an unterminated token", () => {
  assert.deepEqual(findActiveTrigger("/ne", 3), { kind: "command", start: 0, end: 3, query: "ne" });
  assert.deepEqual(findActiveTrigger("看看 @pro", 7), { kind: "mention", start: 3, end: 7, query: "pro" });
  // A space closes the token: the suggestion list must not linger.
  assert.equal(findActiveTrigger("/new ", 5), null);
  assert.equal(findActiveTrigger("@prod 后面", 8), null);
  // A command acts on the whole line, so it cannot appear mid-sentence.
  assert.equal(findActiveTrigger("然后 /new", 7), null);
  // An email-ish "@" glued to a word is not a mention.
  assert.equal(findActiveTrigger("a@b", 3), null);
  assert.equal(findActiveTrigger("plain text", 10), null);
});

test("accepting a suggestion replaces only the trigger and leaves the rest alone", () => {
  const trigger = findActiveTrigger("看看 @pro 的磁盘", 7);
  assert.ok(trigger);
  const result = replaceTrigger("看看 @pro 的磁盘", trigger, "@prod-1");
  assert.deepEqual(result, { text: "看看 @prod-1 的磁盘", caret: 10 });
});

test("accepting a suggestion adds a trailing space only when one is missing", () => {
  const open = findActiveTrigger("/ne", 3);
  assert.ok(open);
  // Nothing follows: insert a space so the user can keep typing, caret at the end.
  assert.deepEqual(replaceTrigger("/ne", open, "/new"), { text: "/new ", caret: 5 });

  // A space already follows: do not double it, and land the caret right after
  // the inserted command (index 4), before the existing space.
  const closed = findActiveTrigger("/ne", 3);
  assert.ok(closed);
  assert.deepEqual(replaceTrigger("/ne 再说", closed, "/new"), { text: "/new 再说", caret: 4 });
});

test("command availability is driven by declared requirements, not by guessing", () => {
  const find = (name: string) => AGENT_COMMANDS.find((c) => c.name === name)!;
  assert.equal(commandAvailable(find("stop"), runningDesktop), true);
  assert.equal(commandAvailable(find("stop"), idleDesktop), false);
  assert.equal(unavailableReason(find("stop"), idleDesktop), "当前没有运行");

  assert.equal(commandAvailable(find("new"), runningDesktop), false);
  assert.equal(unavailableReason(find("new"), runningDesktop), "运行中不可用");
  assert.equal(commandAvailable(find("new"), { ...idleDesktop, desktop: false }), true);
  assert.equal(unavailableReason(find("new"), { running: false, archived: true, desktop: true }), "对话已归档");

  assert.equal(unavailableReason(find("history"), { running: false, archived: false, desktop: false }), "仅桌面应用可用");
  assert.equal(unavailableReason(find("help"), runningDesktop), null);
});

test("unavailable commands stay listed with a reason instead of vanishing", () => {
  const matches = matchCommands("", idleDesktop);
  assert.equal(matches.length, AGENT_COMMANDS.length);
  const stop = matches.find((m) => m.spec.name === "stop");
  assert.equal(stop?.reason, "当前没有运行");
  assert.equal(matches.find((m) => m.spec.name === "new")?.reason, null);
});

test("the help text is generated from the command table so it cannot drift", () => {
  const text = commandHelpText(idleDesktop);
  for (const spec of AGENT_COMMANDS) assert.match(text, new RegExp(`/${spec.name} —`));
  assert.match(text, /\/stop — .*（当前没有运行）/);
});

test("submitting a known command never becomes a prompt", () => {
  assert.deepEqual(resolveSubmission("/new"), { kind: "command", spec: AGENT_COMMANDS[0], args: "" });
  const stopped = resolveSubmission("  /stop  ");
  assert.equal(stopped.kind, "command");

  // An unknown slash word is reported, never silently sent as a question —
  // a typo must not turn into a real remote operation.
  assert.deepEqual(resolveSubmission("/deploy now"), { kind: "unknown", name: "deploy" });

  assert.deepEqual(resolveSubmission("重启 nginx"), { kind: "prompt", text: "重启 nginx" });
  // A bare slash is not a command either.
  assert.deepEqual(resolveSubmission("/"), { kind: "unknown", name: "" });
});

const servers: MentionCandidate[] = [
  { id: "srv_prod", label: "prod-1" },
  { id: "srv_stage", label: "staging" },
  { id: "srv_stageprod", label: "staging-prod" },
];

test("mentions rank prefix matches above substring matches", () => {
  assert.deepEqual(matchMentions(servers, "prod").map((c) => c.label), ["prod-1", "staging-prod"]);
  assert.deepEqual(matchMentions(servers, "stag").map((c) => c.label), ["staging", "staging-prod"]);
  // An empty query lists everything, so typing @ is a usable entry point.
  assert.equal(matchMentions(servers, "").length, 3);
  assert.deepEqual(matchMentions(servers, "zzz"), []);
  assert.equal(matchMentions(servers, "", 2).length, 2);
});

test("a mention resolves only on a whole-label boundary", () => {
  // `@prod-1` must not be matched as `@prod`, and short names must not
  // swallow longer ones that contain them.
  assert.equal(resolveMentionedServer("@prod-1 看看", servers)?.id, "srv_prod");
  assert.equal(resolveMentionedServer("@staging-prod 看看", servers)?.id, "srv_stageprod");
  assert.equal(resolveMentionedServer("@staging 看看", servers)?.id, "srv_stage");
  // Unknown names and partial tokens resolve to nothing rather than a wrong server.
  assert.equal(resolveMentionedServer("@prod 看看", servers), null);
  assert.equal(resolveMentionedServer("没有提及", servers), null);
  // The earliest mention wins when several are present.
  assert.equal(resolveMentionedServer("@staging 然后 @prod-1", servers)?.id, "srv_stage");
});

test("anything starting with a slash is never resolved as a plain prompt", () => {
  // The invariant that keeps a typo from becoming a real remote operation:
  // slash-prefixed input always resolves to a command or an explicit unknown,
  // so the caller can never fall through to "send it to the model anyway".
  const inputs = ["/", "/x", "/new", "/NEW", "/stop now", "/deploy prod", "/ help", "/unknown-command"];
  for (const input of inputs) {
    assert.notEqual(resolveSubmission(input).kind, "prompt", `"${input}" must not become a prompt`);
  }
  // Case is folded, and leading whitespace is trimmed.
  assert.equal(resolveSubmission("/NEW").kind, "command");
  assert.equal(resolveSubmission("  /stop  ").kind, "command");
  // A slash that is not at the start of the trimmed input is ordinary text.
  assert.deepEqual(resolveSubmission("看看 /var/log"), { kind: "prompt", text: "看看 /var/log" });
});

import {
  APPROVAL_ORDER,
  RUN_MODE_ORDER,
  RUN_MODE_SPECS,
  approvalOptionSpec,
  runModeSpec,
} from "../src/features/agent/run-mode.js";

// --- run modes and approval options -----------------------------------------


test("every run mode and approval option has reachable display copy", () => {
  // A mode with no spec would render an undefined label — the Record type
  // forces exhaustiveness, and this keeps the order arrays in step with it.
  for (const mode of AGENT_RUN_MODES) {
    const spec = runModeSpec(mode);
    assert.equal(spec.mode, mode);
    assert.ok(spec.label.length > 0, `${mode} needs a label`);
    assert.ok(spec.summary.length > 0, `${mode} needs a summary`);
    assert.ok(RUN_MODE_ORDER.includes(mode), `${mode} is missing from the menu order`);
  }
  assert.equal(RUN_MODE_ORDER.length, AGENT_RUN_MODES.length);

  for (const mode of AGENT_PERMISSION_MODES) {
    const spec = approvalOptionSpec(mode);
    assert.equal(spec.value, mode);
    assert.ok(spec.label.length > 0);
    assert.ok(APPROVAL_ORDER.includes(mode));
  }
  assert.equal(APPROVAL_ORDER.length, AGENT_PERMISSION_MODES.length);
});

test("the UI marks exactly the modes the backend enforces as read-only", () => {
  // The badge is a promise about enforcement. It must be true of the same
  // modes the permission engine actually denies, not a hand-kept list.
  for (const mode of AGENT_RUN_MODES) {
    assert.equal(
      runModeSpec(mode).readOnly,
      isReadOnlyRunMode(mode),
      `${mode}: UI badge disagrees with the enforced rule`,
    );
  }
  assert.equal(RUN_MODE_SPECS.goal.readOnly, false);
  assert.equal(RUN_MODE_SPECS.plan.readOnly, true);
  assert.equal(RUN_MODE_SPECS.readonly.readOnly, true);
});

test("the two mode axes stay independent", () => {
  // "plan mode + auto approve" is a legitimate combination: the run mode
  // bounds scope, the approval mode decides who signs off. Neither setting
  // may silently reinterpret the other.
  assert.equal(approvalOptionSpec("auto").value, "auto");
  assert.equal(approvalOptionSpec("ask").value, "ask");
  assert.notEqual(runModeSpec("plan").mode, "goal");
  assert.equal(AGENT_RUN_MODES.includes("ask" as never), false);
  assert.equal(AGENT_PERMISSION_MODES.includes("plan" as never), false);
});

const dockerCall: ToolCallFacts = {  callId: "call_1", toolName: "docker.ps", target: "local · 本地环境", riskLevel: "read", decision: "auto",
};

test("a tool call opens a pending card and its result fills in the same card", () => {
  const pending = appendToolCall([{ kind: "user", text: "看看容器" }], dockerCall);
  assert.deepEqual(pending.at(-1), { kind: "tool", ...dockerCall });

  const settled = settleToolResult(pending, dockerCall, { status: "success", durationMs: 120, summary: "3 个容器" });
  // One row, not two: the call and its result stay a single card.
  assert.equal(settled.length, pending.length);
  assert.deepEqual(settled.at(-1), {
    kind: "tool", ...dockerCall, result: { status: "success", durationMs: 120, summary: "3 个容器" },
  });
});

test("tool results pair by callId, not by position", () => {
  const a = { ...dockerCall, callId: "call_a", toolName: "docker.ps" };
  const b = { ...dockerCall, callId: "call_b", toolName: "filesystem.read" };
  // Two calls are in flight; the FIRST one's result arrives first.
  let entries = appendToolCall(appendToolCall([], a), b);
  entries = settleToolResult(entries, a, { status: "success", durationMs: 10, summary: "A" });
  assert.deepEqual(entries[0], { kind: "tool", ...a, result: { status: "success", durationMs: 10, summary: "A" } });
  // B must still be pending — a positional match would have wrongly settled it.
  assert.equal((entries[1] as Extract<Entry, { kind: "tool" }>).result, undefined);
});

test("a result without its call is never dropped", () => {
  const settled = settleToolResult([], dockerCall, { status: "failed", durationMs: 30, summary: "daemon 未运行" });
  assert.deepEqual(settled, [
    { kind: "tool", ...dockerCall, result: { status: "failed", durationMs: 30, summary: "daemon 未运行" } },
  ]);
});

test("the final assistant text replaces the streamed line instead of duplicating it", () => {
  const streamed: Entry[] = [{ kind: "user", text: "hi" }, { kind: "assistant", text: "部分" }];
  assert.deepEqual(settleAssistantText(streamed, "完整回答"), [
    { kind: "user", text: "hi" },
    { kind: "assistant", text: "完整回答" },
  ]);

  // With no trailing assistant line, the final text is appended once.
  assert.deepEqual(settleAssistantText([{ kind: "user", text: "hi" }], "完整回答"), [
    { kind: "user", text: "hi" },
    { kind: "assistant", text: "完整回答" },
  ]);
});

test("restoring a stored conversation keeps only the user and assistant turns", () => {
  const entries = entriesFromMessages([
    { id: "m1", sessionId: "s1", role: "user", content: "部署一下", createdAt: "2026-01-01T00:00:00Z" },
    { id: "m2", sessionId: "s1", role: "assistant", content: "好的", createdAt: "2026-01-01T00:00:01Z" },
    { id: "m3", sessionId: "s1", role: "tool", content: "{\"hidden\":true}", traceId: "tr_1", createdAt: "2026-01-01T00:00:02Z" },
    { id: "m4", sessionId: "s1", role: "system", content: "internal", createdAt: "2026-01-01T00:00:03Z" },
  ]);
  assert.deepEqual(entries, [
    { kind: "user", text: "部署一下" },
    { kind: "assistant", text: "好的" },
  ]);
});

test("session titles come from the prompt with leading markdown headings removed", () => {
  assert.equal(sessionTitleFromPrompt("# 紧急排查\n重启 staging 的 nginx"), "重启 staging 的 nginx");
  assert.equal(sessionTitleFromPrompt("   多个    空格   "), "多个 空格");
  assert.equal(sessionTitleFromPrompt("   "), "未命名任务");
  assert.equal(sessionTitleFromPrompt("x".repeat(80)).length, 48);
});

test("file bodies never reach the transcript, and other tool output is bounded", () => {
  assert.equal(
    toolResultSummary("filesystem.read", "SECRET=1"),
    "文件内容已返回给 Agent（正文不在动态中保存）",
  );
  assert.equal(toolResultSummary("docker.ps", "a".repeat(400)).length, 240);
});

test("tool targets prefer the server id and fall back to the host scope", () => {
  assert.equal(targetLabel({ host: "local", environment: "local" }), "local · 本地环境");
  assert.equal(targetLabel({ host: "remote", serverId: "srv_a", environment: "production" }), "srv_a · 生产环境");
});
