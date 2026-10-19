import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";
import { IPC_SCHEMAS, AgentStreamEventSchema, type AgentTeamTask } from "@yukinal/shared";
import { reconcileTeamTask, coordinatorConfig, coordinatorScope, coordinatorEntries, useCoordinatorStore } from "../src/features/agent/coordinator-state.js";
import { CoordinatorStatus } from "../src/features/agent/CoordinatorTasks.js";
import { defaultAgentRoles } from "../src/features/agent-nist/agent-nist.js";
import { useTeamRuntimeStore } from "../src/features/agent-nist/team-runtime-store.js";
import { teamBoardCards } from "../src/features/agent-nist/team-board.js";
import { useWorkspaceStore } from "../src/stores/workspace-store.js";
import { previewResponse } from "../src/lib/preview-fixtures.js";
import { usesNativePreviewChat } from "../src/lib/preview-chat.js";

const job: AgentTeamTask = { id: "team_one", runId: "run_one", parentRunId: "run_parent", sessionId: "session_one", roleId: "blue-guardian", team: "blue", title: "检查服务和日志", serverId: "srv_alpha", access: "read", state: "running", revision: 1, createdAt: "2026-10-17T12:00:00Z", updatedAt: "2026-10-17T12:00:00Z" };

test("late status cannot move a result into another session, change targets, or revive finished work", () => {
  const done = { ...job, state: "completed" as const, revision: 5, result: "已返回证据" };
  assert.equal(reconcileTeamTask(done, { ...job, revision: 2 }), done);
  assert.equal(reconcileTeamTask(done, { ...job, revision: 7 }), done);
  assert.equal(reconcileTeamTask(job, { ...job, sessionId: "different", revision: 2 }), job);
  assert.equal(reconcileTeamTask(job, { ...job, serverId: "srv_beta", revision: 2 }), job);
  assert.equal(reconcileTeamTask(job, done), done);
});

test("worker status updates the board without setting the foreground chat busy", () => {
  useWorkspaceStore.setState({ agentBusy: false });
  useTeamRuntimeStore.setState({ runs: [], cues: [] });
  useCoordinatorStore.setState({ tasks: [] });
  useCoordinatorStore.getState().receive(job);
  useCoordinatorStore.getState().receive({ ...job, revision: 2, usage: { inputTokens: 20, outputTokens: 10 } });
  useCoordinatorStore.getState().receive({ ...job, revision: 1 });
  assert.equal(useWorkspaceStore.getState().agentBusy, false);
  assert.equal(useCoordinatorStore.getState().tasks.length, 1);
  const runtime = useTeamRuntimeStore.getState();
  assert.equal(runtime.runs.length, 1);
  assert.equal(runtime.runs[0]?.sessionId, "session_one");
  runtime.begin({ id: "lead", team: "white", source: "coordinator", serverId: null, goal: "分派任务" });
  assert.equal(teamBoardCards([], useTeamRuntimeStore.getState().runs).length, 1);
  runtime.pulse(Date.now());
  assert.equal(useTeamRuntimeStore.getState().totals.inputTokens, 20);
});

test("the coordinator remains the only conversation voice while multiple teams work", () => {
  const html = renderToStaticMarkup(<CoordinatorStatus tasks={[job, { ...job, id: "team_two", runId: "run_two", team: "yellow" }, { ...job, id: "other", sessionId: "elsewhere" }]} sessionId="session_one" />);
  assert.match(html, /协调助手/);
  assert.match(html, /aria-label="协调助手：2 项后台任务，可继续对话"/);
  assert.match(html, />2 项后台任务</);
  assert.doesNotMatch(html, /蓝队|黄队/);
});

test("idle and preview chats have no standalone coordinator status", () => {
  assert.deepEqual(previewResponse("agent_team_tasks", {}), { tasks: [] });
  assert.equal(usesNativePreviewChat("agent_team_tasks", true), false);
  const html = renderToStaticMarkup(<CoordinatorStatus tasks={[]} sessionId={null} preview />);
  assert.equal(html, "");
  assert.equal(renderToStaticMarkup(<CoordinatorStatus tasks={[job]} sessionId="session_one" preview />), "");
  assert.equal(renderToStaticMarkup(<CoordinatorStatus tasks={[job]} sessionId="elsewhere" />), "");
  assert.equal(renderToStaticMarkup(<CoordinatorStatus tasks={[{ ...job, state: "completed" }]} sessionId="session_one" />), "");
});

test("internal dispatch receipts do not crowd chat while ordinary tools and failures remain visible", () => {
  const entries = coordinatorEntries([
    { kind: "user", text: "检查服务器" },
    { kind: "tool", toolName: "teams.dispatch", callId: "dispatch", target: "local", riskLevel: "read", decision: "auto" },
    { kind: "tool", toolName: "server.logs", callId: "logs", target: "srv_alpha", riskLevel: "read", decision: "auto" },
    { kind: "error", text: "本次运行失败" },
  ]);
  assert.deepEqual(entries.map((entry) => entry.kind), ["user", "tool", "error"]);
  assert.equal(entries[1]?.kind === "tool" && entries[1].toolName, "server.logs");
});

test("missing and partially reported provider usage stays unknown across heartbeat totals", () => {
  useTeamRuntimeStore.setState({ runs: [], cues: [] });
  const runtime = useTeamRuntimeStore.getState();
  runtime.begin({ id: "missing", team: "blue", serverId: null, goal: "检查" });
  runtime.usage("missing", { inputTokens: 0, outputTokens: 0, reportedRequests: 0, unreportedRequests: 1 });
  runtime.pulse(30_000);
  assert.deepEqual(useTeamRuntimeStore.getState().totals, { inputTokens: 0, outputTokens: 0, reportedRuns: 0, unreportedRuns: 1 });
  runtime.usage("missing", { inputTokens: 20, outputTokens: 10, reportedRequests: 1, unreportedRequests: 1 });
  runtime.pulse(60_000);
  assert.deepEqual(useTeamRuntimeStore.getState().totals, { inputTokens: 20, outputTokens: 10, reportedRuns: 1, unreportedRuns: 1 });
});

test("unbound roles inherit the foreground model, broken configured bindings are not silently replaced", () => {
  const roles = defaultAgentRoles();
  const config = coordinatorConfig(roles, [], [], null);
  assert.equal(config.roles.length, roles.length);
  assert.equal(config.roles[0]?.providerId, undefined);
  const broken = coordinatorConfig([{ ...roles[0]!, providerId: "missing", model: "missing" }, roles[1]!], [], [], roles[0]!.id);
  assert.deepEqual(broken.roles.map((role) => role.id), [roles[1]!.id]);
  assert.equal(broken.preferredRoleId, undefined);
});

test("a multi-server request stays global so neither server is silently excluded", () => {
  const servers = [{ id: "srv_alpha", label: "Alpha" }, { id: "srv_beta", label: "Beta" }];
  assert.equal(coordinatorScope("检查 @Alpha 和 @Beta", servers, "srv_alpha"), null);
  assert.equal(coordinatorScope("检查 @Alpha", servers, null), "srv_alpha");
  assert.equal(coordinatorScope("继续上个任务", servers, "srv_beta"), "srv_beta");
});

test("coordinator config and durable job snapshots cross the actual strict IPC/event gates", () => {
  assert.equal(IPC_SCHEMAS.agent_run_start.params.safeParse({ runId: "lead", sessionId: "session_one", prompt: "检查", coordination: coordinatorConfig(defaultAgentRoles(), [], [], null) }).success, true);
  assert.equal(IPC_SCHEMAS.agent_team_tasks.response.safeParse({ tasks: [job] }).success, true);
  assert.equal(AgentStreamEventSchema.safeParse({ type: "agent.team_task", runId: job.runId, task: job, at: job.updatedAt }).success, true);
  assert.equal(AgentStreamEventSchema.safeParse({ type: "agent.team_task", runId: job.runId, task: { ...job, fingerprint: "private" }, at: job.updatedAt }).success, false);
});
