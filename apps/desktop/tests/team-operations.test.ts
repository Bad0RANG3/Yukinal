import assert from "node:assert/strict";
import { test, afterEach } from "node:test";
import { attentionReference, heartbeatSlot, heartbeatMultiple, nextHeartbeatAt, nextWatchSlot, isWatchRequest } from "../src/features/agent-nist/team-operations.js";
import { useTeamRuntimeStore } from "../src/features/agent-nist/team-runtime-store.js";
afterEach(() => useTeamRuntimeStore.setState(useTeamRuntimeStore.getInitialState(), true));
test("one epoch heartbeat coalesces missed slots and custom intervals never run faster than requested", () => {
  assert.equal(heartbeatSlot(89_999), 2);
  assert.equal(nextHeartbeatAt(90_000), 120_000);
  assert.equal(heartbeatMultiple(61), 3);
  assert.equal(nextWatchSlot(9_000_000, 300), 310);
  assert.equal(nextWatchSlot(9_999_999, 300) > heartbeatSlot(9_999_999), true);
});
test("usage snapshots are counted once per run, including late and repeated events", () => {
  const store = useTeamRuntimeStore.getState();
  store.begin({ id: "run_a", team: "blue", serverId: "srv_a", goal: "检查" });
  store.begin({ id: "run_b", team: "yellow", serverId: "srv_b", goal: "部署" });
  store.usage("run_a", { inputTokens: 120, outputTokens: 30 });
  store.usage("run_a", { inputTokens: 120, outputTokens: 30 });
  store.usage("run_a", { inputTokens: 100, outputTokens: 20 });
  store.usage("unknown", { inputTokens: 500, outputTokens: 500 });
  store.pulse(120_000);
  assert.deepEqual(useTeamRuntimeStore.getState().totals, { inputTokens: 120, outputTokens: 30, reportedRuns: 1, unreportedRuns: 1 });
  store.usage("run_b", { inputTokens: 90, outputTokens: 10 });
  store.begin({ id: "run_a", team: "blue", serverId: "srv_a", goal: "相同受理重试" });
  store.pulse(150_000);
  assert.equal(useTeamRuntimeStore.getState().totals.inputTokens, 210);
});

test("invalid usage is unknown rather than a reported zero", () => {
  const store = useTeamRuntimeStore.getState();
  store.begin({ id: "run", team: "blue", serverId: null, goal: "检查" });
  store.usage("run", { inputTokens: -1, outputTokens: 2 });
  store.pulse(30_000);
  assert.equal(useTeamRuntimeStore.getState().totals.reportedRuns, 0);
  assert.equal(useTeamRuntimeStore.getState().totals.unreportedRuns, 1);
});
test("attention uses a strict semantic allowlist and never converts the visible workpage into a tool target", () => {
  const reference = attentionReference({ section: "servers", serverId: "srv_a", workpage: { kind: "logs", serverId: "srv_b" }, action: "tool:logs" });
  assert.match(reference, /不构成操作授权/);
  assert.match(reference, /srv_a/);
  assert.match(reference, /srv_b/);
  assert.doesNotMatch(attentionReference({ section: "password=secret", serverId: "credential\nsecret", action: "textarea:draft=secret" }), /secret/);
});
test("watch suggestions do not turn stop or pause requests into a new loop", () => {
  assert.equal(isWatchRequest("每5分钟帮我盯着服务"), true);
  assert.equal(isWatchRequest("暂停值守"), false);
  assert.equal(isWatchRequest("不要再持续检查"), false);
  assert.equal(isWatchRequest("检查一下服务器"), false);
});
test("a fast result has a visible trail and an old timer cannot clear another operation", () => {
  const store = useTeamRuntimeStore.getState();
  store.begin({ id: "run", team: "blue", serverId: "srv_a", goal: "日志" });
  store.tool("run", "server.logs", "srv_a", true, "call-1");
  const cue = useTeamRuntimeStore.getState().cues[0]!;
  assert.equal(cue.finished, true);
  assert.equal(cue.until > Date.now(), true);
  store.tool("run", "filesystem.read", "srv_a", false, "call-2");
  store.expireCues(cue.until - 1);
  assert.equal(useTeamRuntimeStore.getState().cues.length, 2);
  store.expireCues(cue.until);
  assert.deepEqual(useTeamRuntimeStore.getState().cues.map((item) => item.callId), ["call-2"]);
});

test("parallel runs and calls retain independent targets, completion and cancellation", () => {
  const store = useTeamRuntimeStore.getState();
  store.begin({ id: "a", team: "yellow", serverId: "srv_a", goal: "部署" });
  store.begin({ id: "b", team: "blue", serverId: "srv_b", goal: "检查" });
  store.tool("a", "package.install", "srv_a", false, "same-call-id");
  store.tool("b", "server.logs", "srv_b", false, "same-call-id");
  store.tool("a", "filesystem.read", null, false, "host-call");
  assert.equal(useTeamRuntimeStore.getState().cues.length, 3);
  assert.equal(useTeamRuntimeStore.getState().cues.find((cue) => cue.callId === "host-call")?.serverId, null);
  store.tool("a", "package.install", "srv_a", true, "same-call-id");
  store.stage("a", "cancelled");
  const cues = useTeamRuntimeStore.getState().cues;
  assert.equal(cues.find((cue) => cue.runId === "b")?.finished, false);
  assert.equal(cues.find((cue) => cue.runId === "a")?.finished, true);
  assert.equal(cues.some((cue) => cue.callId === "host-call"), false);
  store.stage("b", "waiting_approval");
  assert.equal(useTeamRuntimeStore.getState().cues.some((cue) => cue.runId === "b"), false);
  store.tool("b", "server.logs", "srv_b", false, "late-call");
  store.tool("a", "server.logs", "srv_a", false, "late-call");
  store.tool("unknown", "server.logs", "srv_a", false, "late-call");
  assert.equal(useTeamRuntimeStore.getState().cues.length, 1);
});

test("duplicate call events cannot restart the completed cue or prolong its trail", () => {
  const store = useTeamRuntimeStore.getState();
  store.begin({ id: "a", team: "blue", serverId: "srv_a", goal: "检查" });
  store.tool("a", "server.logs", "srv_a", true, "call");
  const cue = useTeamRuntimeStore.getState().cues[0];
  store.tool("a", "server.logs", "srv_a", false, "call");
  store.tool("a", "server.logs", "srv_a", true, "call");
  assert.equal(useTeamRuntimeStore.getState().cues[0], cue);
});

test("run bindings cannot follow a different visible conversation", () => {
  const store = useTeamRuntimeStore.getState();
  store.begin({ id: "a", team: "blue", serverId: "srv_a", goal: "检查" });
  store.bind("a", "session-a", "task-a");
  store.bind("a", "session-b", "task-b");
  assert.equal(useTeamRuntimeStore.getState().runs[0]?.sessionId, "session-a");
  assert.equal(useTeamRuntimeStore.getState().runs[0]?.taskId, "task-a");
});
