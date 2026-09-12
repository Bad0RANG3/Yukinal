/**
 * Contract tests for the IPC command schemas (one side of the pair; the other lives
 * in `crates/core/src/ipc.rs` and asserts the *same* fixtures against serde output).
 *
 * The fixtures in `packages/shared/fixtures/ipc/` are the single canonical JSON each
 * side must accept/emit; a parse here is a promise that the Rust contract test keeps.
 */

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import type { IpcCommandName } from "../ipc/index.js";
import { DockerRestartInputSchema, DockerRestartResultSchema } from "./docker.js";
import { FilesystemReadInputSchema, FilesystemWriteInputSchema } from "./file.js";
import {
  AgentStatusSchema,
  EMPTY_PAYLOAD,
  IpcServerIdSchema,
  IPC_SCHEMAS,
} from "./ipc.js";

const FIXTURE_DIR = new URL("../../fixtures/ipc/", import.meta.url);

function fixture(name: string): unknown {
  return JSON.parse(readFileSync(new URL(`${name}.json`, FIXTURE_DIR), "utf8")) as unknown;
}

test("every command in IpcCommandMap has a fixture and its response schema parses it", () => {
  const commands = Object.keys(IPC_SCHEMAS) as IpcCommandName[];
  assert.ok(commands.length >= 14, "the command map must not silently shrink");
  for (const command of commands) {
    const payload = fixture(command);
    const parsed = IPC_SCHEMAS[command].response.safeParse(payload);
    assert.equal(
      parsed.success,
      true,
      `${command} response schema must parse packages/shared/fixtures/ipc/${command}.json`,
    );
  }
});

test("the exited agent_status variant parses too", () => {
  const parsed = AgentStatusSchema.safeParse(fixture("agent_status_exited"));
  assert.equal(parsed.success, true);
});

test("a status carrying an automatic-restart record parses, and the field stays optional", () => {
  // Not covered by the per-command loop above: there is only one fixture per command
  // name, and `agent_status.json` is the shape an idle supervisor reports.
  const parsed = AgentStatusSchema.safeParse(fixture("agent_status_restarted"));
  assert.equal(parsed.success, true, "a restart record must parse");
  const status = parsed.data as { restart?: { attempt: number; exhausted: boolean } };
  assert.equal(status.restart?.attempt, 2);
  assert.equal(status.restart?.exhausted, false);
  // Optional, not nullable: an idle supervisor omits it entirely, and the fixture from
  // before automatic recovery existed must keep parsing.
  const idle = AgentStatusSchema.safeParse(fixture("agent_status"));
  assert.equal(idle.success, true);
  assert.equal((idle.data as { restart?: unknown }).restart, undefined);
});

test("a sync run.start response carries the run's result", () => {
  // The async fixture (`agent_run_start.json`) is the one the per-command loop above
  // checks, and it has no `result` — which is correct for `delivery: "async"`. This is
  // the other half of the pair: the shape a `delivery: "sync"` call answers with, and
  // the reason that mode is worth waiting for. It is not covered by the loop above
  // because there is only one fixture per command name.
  const parsed = IPC_SCHEMAS.agent_run_start.response.safeParse(fixture("agent_run_start_sync"));
  assert.equal(parsed.success, true, "the sync response must parse");
  const response = parsed.data as { started: boolean; result?: { state: string; steps: number } };
  assert.equal(response.started, true);
  assert.equal(response.result?.state, "completed");
  assert.equal(response.result?.steps, 3);
});

test("a result on a sync response is validated, not passed through", () => {
  // The result object reaches the UI through a response frame, so it does not pass the
  // event gate. If a sidecar ever sends a malformed one, this is where it has to fail.
  const malformed = {
    runId: "run_20260101",
    started: true,
    result: { runId: "run_20260101", state: "not-a-state", text: "", steps: 0, toolCalls: 0 },
  };
  assert.equal(IPC_SCHEMAS.agent_run_start.response.safeParse(malformed).success, false);
});

test("responses are strict: a serde drift (extra field) must fail, not be stripped", () => {
  const drifted = { ...(fixture("agent_status") as Record<string, unknown>), toolCount: 1, os: "klingon" };
  assert.equal(AgentStatusSchema.safeParse(drifted).success, false);
});

test("empty payloads reject unknown keys", () => {
  assert.equal(EMPTY_PAYLOAD.safeParse({}).success, true);
  assert.equal(EMPTY_PAYLOAD.safeParse({ anything: 1 }).success, false);
});

test("server ids on the wire must be opaque srv_ ids", () => {
  for (const name of [
    "server_connect",
    "server_disconnect",
    "server_snapshot",
    "server_host_key_status",
    "server_host_key_probe",
    "server_host_key_forget",
  ] as const) {
    assert.equal(
      IPC_SCHEMAS[name].params.safeParse({ serverId: "api.example.com:22" }).success,
      false,
      `${name} must reject a host-derived id`,
    );
    assert.equal(IPC_SCHEMAS[name].params.safeParse({ serverId: "srv_01abc" }).success, true);
  }
  // `server_host_key_trust` 还带一个指纹，所以单独来一遍（上面的循环里它会因为
  // 缺指纹而失败，那样这半条断言就变成了「缺字段也报错」，什么都没证明）。
  assert.equal(
    IPC_SCHEMAS.server_host_key_trust.params.safeParse({
      serverId: "api.example.com:22",
      fingerprint: "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU",
    }).success,
    false,
    "server_host_key_trust must reject a host-derived id",
  );
  const open = IPC_SCHEMAS.terminal_open.params.safeParse({
    serverId: "api.example.com:22",
    cols: 120,
    rows: 30,
  });
  assert.equal(open.success, false, "terminal_open must reject a host-derived id");
  assert.equal(IpcServerIdSchema.safeParse("srv_01abc").success, true);
  assert.equal(IpcServerIdSchema.safeParse("production").success, false);
});

/* ── 主机指纹（ADR 0012） ───────────────────────────────────────────────────── */

test("the unpinned status shape has no fingerprint field at all", () => {
  // 未核验不等于「指纹是空字符串」也不等于「指纹是 null」：Rust 会整个省掉这个字段，
  // 而界面要靠它的**缺席**显示「未核验」。这条钉住的是这个区别，不是 JSON 的美观。
  const unpinned = IPC_SCHEMAS.server_host_key_status.response.safeParse(
    fixture("server_host_key_status_unpinned"),
  );
  assert.equal(unpinned.success, true);
  assert.equal(
    Object.hasOwn(unpinned.data as object, "pinnedFingerprint"),
    false,
    "未核验时不该有 pinnedFingerprint",
  );

  // 反过来：显式给一个 null 是 drift，必须被拒。
  const nulled = { host: "api.example.com", port: 22, pinned: false, pinnedFingerprint: null };
  assert.equal(IPC_SCHEMAS.server_host_key_status.response.safeParse(nulled).success, false);
});

test("a probe answer is a comparison, and a mismatch carries both fingerprints", () => {
  // 主 fixture 就是 mismatch：契约里最要紧的那种形状（ADR 0012 第 3 条）必须是
  // 被逐字验证过的那个，而不是只存在于某个测试的局部对象里。
  const parsed = IPC_SCHEMAS.server_host_key_probe.response.safeParse(fixture("server_host_key_probe"));
  assert.equal(parsed.success, true);
  const probe = parsed.data as { comparison: string; pinnedFingerprint?: string; presentedFingerprint: string };
  assert.equal(probe.comparison, "mismatch");
  assert.notEqual(probe.pinnedFingerprint, probe.presentedFingerprint);

  const unpinned = IPC_SCHEMAS.server_host_key_probe.response.safeParse(
    fixture("server_host_key_probe_unpinned"),
  );
  assert.equal(unpinned.success, true);
  assert.equal((unpinned.data as { comparison: string }).comparison, "unpinned");

  // 只有三种比较结果，拼错的第四个词必须是 drift。
  const invented = { ...(fixture("server_host_key_probe") as Record<string, unknown>), comparison: "verified" };
  assert.equal(IPC_SCHEMAS.server_host_key_probe.response.safeParse(invented).success, false);
});

test("a fingerprint on the wire is a real SHA256 fingerprint, not a placeholder", () => {
  const params = IPC_SCHEMAS.server_host_key_trust.params;
  const real = "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU";
  assert.equal(params.safeParse({ serverId: "srv_01abc", fingerprint: real }).success, true);

  for (const bogus of [
    // 这正是被替换掉的那个占位字符串：它曾经被塞进一个叫 fingerprint 的字段。
    "not pinned (first connect must be explicitly trusted)",
    "SHA256:",
    "MD5:aa:bb:cc",
    // 带 padding 的 base64 不是 ssh-key 的写法（ADR 0012 第 6 条）。
    "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=",
    "",
  ]) {
    assert.equal(
      params.safeParse({ serverId: "srv_01abc", fingerprint: bogus }).success,
      false,
      `trust must refuse to pin ${JSON.stringify(bogus)}`,
    );
  }

  // 指纹是必填的：一个「钉住……什么？」的请求不存在。
  assert.equal(params.safeParse({ serverId: "srv_01abc" }).success, false);
});

test("terminal param shapes match the contract", () => {
  const open = IPC_SCHEMAS.terminal_open.params.safeParse({
    serverId: "srv_01abc",
    cols: 120,
    rows: 30,
  });
  assert.equal(open.success, true);

  const write = IPC_SCHEMAS.terminal_write.params.safeParse({
    terminalSessionId: "t_01",
    data: "ls\r",
  });
  assert.equal(write.success, true);
});

test("add-server params are validated by the shared input schema", () => {
  const parsed = IPC_SCHEMAS.server_add.params.safeParse({
    name: "db",
    host: "10.0.0.5",
    username: "root",
    environment: "staging",
    authentication: { method: "password", password: "hunter2" },
  });
  assert.equal(parsed.success, true);
});

test("Agent run params carry the explicit permission delegation", () => {
  const parsed = IPC_SCHEMAS.agent_run_start.params.safeParse({
    sessionId: "ses_1",
    prompt: "restart the service",
    permissionMode: "auto",
  });
  assert.equal(parsed.success, true);
  assert.equal(IPC_SCHEMAS.agent_run_start.params.safeParse({
    sessionId: "ses_1",
    prompt: "restart the service",
    permissionMode: "policy",
  }).success, false);
});

test("activity_list accepts an optional server filter and bounded limit", () => {
  const parsed = IPC_SCHEMAS.activity_list.params.safeParse({ serverId: "srv_01abc", limit: 25 });
  assert.equal(parsed.success, true);
  assert.equal(IPC_SCHEMAS.activity_list.params.safeParse({ serverId: "api.example.com:22" }).success, false);
  assert.equal(IPC_SCHEMAS.activity_list.params.safeParse({ limit: 0 }).success, false);
  assert.equal(IPC_SCHEMAS.activity_list.params.safeParse({ limit: 101 }).success, false);
});

test("tool_execution_list accepts trace/server filters and bounded limit", () => {
  assert.equal(
    IPC_SCHEMAS.tool_execution_list.params.safeParse({ traceId: "trc_1", limit: 25 }).success,
    true,
  );
  assert.equal(
    IPC_SCHEMAS.tool_execution_list.params.safeParse({ serverId: "srv_01abc", limit: 25 }).success,
    true,
  );
  assert.equal(
    IPC_SCHEMAS.tool_execution_list.params.safeParse({ serverId: "api.example.com:22" }).success,
    false,
  );
  assert.equal(IPC_SCHEMAS.tool_execution_list.params.safeParse({ limit: 0 }).success, false);
  assert.equal(IPC_SCHEMAS.tool_execution_list.params.safeParse({ limit: 101 }).success, false);
});

test("chat_session_list pages by a bounded offset and reports per-state counts", () => {
  const params = IPC_SCHEMAS.chat_session_list.params;
  assert.equal(params.safeParse({ query: "nginx", archived: false, offset: 50, limit: 50 }).success, true);
  assert.equal(params.safeParse({ offset: 0 }).success, true);
  // A negative or absurd offset would ask SQLite to walk the table for nothing.
  assert.equal(params.safeParse({ offset: -1 }).success, false);
  assert.equal(params.safeParse({ offset: 10_001 }).success, false);

  // `counts` is required, not optional: the filter tabs label themselves with these
  // numbers, and a response without them would leave the UI guessing page-local totals.
  const withoutCounts = IPC_SCHEMAS.chat_session_list.response.safeParse({ sessions: [] });
  assert.equal(withoutCounts.success, false);
  assert.equal(
    IPC_SCHEMAS.chat_session_list.response.safeParse({
      sessions: [],
      counts: { active: 3, archived: 1 },
    }).success,
    true,
  );
  assert.equal(
    IPC_SCHEMAS.chat_session_list.response.safeParse({ sessions: [], counts: { active: -1, archived: 0 } })
      .success,
    false,
  );
});

test("chat_session_rename carries a bounded title and answers with the stored row", () => {
  const params = IPC_SCHEMAS.chat_session_rename.params;
  assert.equal(params.safeParse({ sessionId: "ses_01hqx9", title: "排查 staging 的 nginx 502" }).success, true);
  // Same bound as create: a renamed title is the same field, so it cannot be longer.
  assert.equal(params.safeParse({ sessionId: "ses_01hqx9", title: "" }).success, false);
  assert.equal(params.safeParse({ sessionId: "ses_01hqx9", title: "x".repeat(201) }).success, false);
  assert.equal(IPC_SCHEMAS.chat_session_rename.response.safeParse({ session: {} }).success, false);
});

test("filesystem tool schemas bound paths, reads and writes", () => {
  assert.equal(FilesystemReadInputSchema.safeParse({ path: "/etc/app.env", maxBytes: 4096 }).success, true);
  assert.equal(FilesystemReadInputSchema.safeParse({ path: "relative/path" }).success, false);
  assert.equal(FilesystemReadInputSchema.safeParse({ path: "/tmp/file", maxBytes: 0 }).success, false);
  assert.equal(FilesystemWriteInputSchema.safeParse({ path: "/etc/app.env", content: "PORT=8080" }).success, true);
  assert.equal(FilesystemWriteInputSchema.safeParse({ path: "/etc/app.env", content: "x", extra: true }).success, false);
});

test("docker restart schema keeps the mutating input bounded", () => {
  assert.equal(DockerRestartInputSchema.safeParse({ container: "api_1", timeoutSeconds: 30 }).success, true);
  assert.equal(DockerRestartInputSchema.safeParse({ container: "api;rm -rf /" }).success, false);
  assert.equal(DockerRestartInputSchema.safeParse({ container: "api_1", timeoutSeconds: 0 }).success, false);
  assert.equal(DockerRestartResultSchema.safeParse({ container: "api_1", restarted: true }).success, true);
});

/* ── kind 轴跨过 IPC 的那一段（ADR 0011） ──────────────────────────────────── */

test("provider_save is the one save command, and it carries the kind", () => {
  // 命令名曾经是 `provider_save_openai` —— 一个把「只有一种 kind」写进名字里的名字。
  // 这条钉住的是：保存路径不再对 kind 有隐含取值，缺了它就是一个非法请求。
  assert.equal(
    IPC_SCHEMAS.provider_save.params.safeParse({
      baseUrl: "https://api.example.com/v1",
      model: "model-id",
    }).success,
    false,
    "the save must not default the kind",
  );
  assert.equal(
    IPC_SCHEMAS.provider_save.params.safeParse({
      kind: "anthropic",
      baseUrl: "https://api.anthropic.com",
      model: "claude-sonnet-4-5",
    }).success,
    true,
  );
  // 未知 kind 在这里失败，而不是被当成 openai-compatible 落库。
  assert.equal(
    IPC_SCHEMAS.provider_save.params.safeParse({
      kind: "cohere",
      baseUrl: "https://api.example.com/v1",
      model: "model-id",
    }).success,
    false,
  );
});

test("a saved provider can be any of the three kinds, and native ones carry no wireApi", () => {
  // 每个 kind 一份 fixture：响应 schema 必须能解析全部三种，否则界面上根本显示不出来。
  for (const [fixtureName, kind] of [
    ["provider_save", "openai-compatible"],
    ["provider_save_anthropic", "anthropic"],
    ["provider_save_gemini", "gemini"],
  ] as const) {
    const parsed = IPC_SCHEMAS.provider_save.response.safeParse(fixture(fixtureName));
    assert.equal(parsed.success, true, `${fixtureName} must parse`);
    assert.equal((parsed.data as { provider: { kind: string } }).provider.kind, kind);
  }

  // 反过来：原生 kind 带着 wireApi 是 drift，不是可以忽略的字段。
  const drifted = { provider: { ...(fixture("provider_save_gemini") as { provider: object }).provider, wireApi: "responses" } };
  assert.equal(IPC_SCHEMAS.provider_save.response.safeParse(drifted).success, false);
});

test("provider_list can carry all three kinds at once", () => {
  // 读路径（数据库 → Rust → IPC → 界面）曾经把每一行都当成 openai-compatible 解码，
  // 所以这份 fixture 里三种 kind 同时存在，而不是只有被硬编码的那一种。
  const parsed = IPC_SCHEMAS.provider_list.response.safeParse(fixture("provider_list"));
  assert.equal(parsed.success, true);
  const kinds = (parsed.data as { providers: Array<{ kind: string; wireApi?: string }> }).providers;
  assert.deepEqual(kinds.map((provider) => provider.kind), ["openai-compatible", "anthropic", "gemini"]);
  assert.deepEqual(kinds.map((provider) => provider.wireApi), ["chat", undefined, undefined]);
});

test("provider_delete answers whether the credential went with the row", () => {
  // 删除会连带回收 keychain 条目 —— 但只在没有别的 Provider 引用同一份引用时。界面上的
  // 二次确认正是拿这句话在问用户，所以「密钥还在不在」必须由响应回答，而不是留给界面猜。
  const parsed = IPC_SCHEMAS.provider_delete.response.safeParse(fixture("provider_delete"));
  assert.equal(parsed.success, true);
  assert.equal((parsed.data as { credentialReclaimed: unknown }).credentialReclaimed, true);

  const kept = IPC_SCHEMAS.provider_delete.response.safeParse({ deleted: true, credentialReclaimed: false });
  assert.equal(kept.success, true);
  // 两个字段都是必填的：少一个字段不是「没删」，而是这一侧没说清。
  assert.equal(IPC_SCHEMAS.provider_delete.response.safeParse({ deleted: true }).success, false);
  // 参数是一个 id，不是下标或名称 —— 名称在真实数据里是会重复的。
  assert.equal(IPC_SCHEMAS.provider_delete.params.safeParse({ providerId: "prv_1a08a8e7bd95" }).success, true);
  assert.equal(IPC_SCHEMAS.provider_delete.params.safeParse({ label: "deepseek" }).success, false);
});
