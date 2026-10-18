import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { renderToStaticMarkup } from "react-dom/server";
import { AGENT_EVENT_MEMBER_SCHEMAS, IPC_SCHEMAS, type ApprovalRequest, type IpcCommandMap } from "@yukinal/shared";
import { NativeTaskRunRuntime, useNativeTaskRunStore, visibleNativeTaskRuns, type NativeTaskRunApi, type NativeTaskSnapshot } from "../src/features/agent/native-task-runs.js";
import { NativeTaskApprovalCards } from "../src/features/agent/NativeTaskApprovals.js";
import { RunLifecycle } from "../src/features/agent/run-lifecycle.js";
import { previewResponse } from "../src/lib/preview-fixtures.js";
import type { DesktopEventPayload } from "../src/lib/ipc.js";

type Detail = IpcCommandMap["investigation_task_get"]["response"];
type Event = DesktopEventPayload<"agent.started" | "agent.waiting_approval" | "agent.approval_expired" | "agent.completed" | "agent.failed">;
const now = Date.parse("2026-10-18T00:00:00Z");
const at = "2026-10-18T00:00:00Z";
const empty: NativeTaskSnapshot = { runs: [], error: null };
afterEach(() => useNativeTaskRunStore.setState(empty));
function deferred<T>() { let resolve!: (value: T) => void; let reject!: (error: Error) => void; const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; }
const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
function detail(taskId = "task_fixture", runId = "run_fixture"): Detail {
  const base = IPC_SCHEMAS.investigation_task_get.response.parse(previewResponse("investigation_task_get", {}));
  return IPC_SCHEMAS.investigation_task_get.response.parse({ ...base,
    task: { ...base.task, id: taskId, serverId: "srv_fixture", scope: { host: "remote", serverId: "srv_fixture", environment: "staging" },
      objective: "Only the inert local approval fixture", status: "waiting_user", activeRunId: runId },
    evidence: [], findings: [], artifacts: [], steps: [], plan: undefined, decisionBrief: undefined,
    runs: [{ id: runId, taskId, sessionId: "session_actual_from_host", messageId: "message_fixture", attempt: 1,
      phase: "investigating", status: "waiting_user", startedAt: at, updatedAt: at }],
  });
}
function approval(runId = "run_fixture", overrides: Partial<ApprovalRequest> = {}): ApprovalRequest {
  return { approvalId: "apr_fixture", runId, toolName: "server.exec", traceId: "trace_fixture", callId: "call_fixture",
    inputFingerprint: "a".repeat(64), planId: "plan_fixture", planStepId: "step_fixture", evidenceIds: [],
    input: { command: "id", purpose: "inert fixture", timeoutMs: 10000, maxOutputBytes: 4096 },
    target: { host: "remote", serverId: "srv_fixture", environment: "staging" }, reason: "Review this exact call", factsSummary: ["The tool has not run yet."],
    expiresAt: "2026-10-18T00:05:00Z", sessionGrantable: false, ...overrides };
}
function waiting(ticket = approval(), taskId = "task_fixture"): Event { return { type: "agent.waiting_approval", runId: ticket.runId, taskId, approval: ticket, at }; }
function completed(runId = "run_fixture", taskId = "task_fixture"): Event { return { type: "agent.completed", runId, taskId, at, result: { runId, state: "completed", text: "inert fixture done", steps: 0, toolCalls: 0 } }; }
function harness() {
  let snapshot: NativeTaskSnapshot = empty;
  let current = detail();
  let ready: Promise<void> = Promise.resolve();
  let starts = 0;
  let stops = 0;
  let unlistens = 0;
  const decisions: IpcCommandMap["agent_approval_respond"]["params"][] = [];
  const listeners = new Map<string, (event: Event) => void>();
  let onStart: (id: string) => Promise<{ runId: string; started: boolean }> = async () => ({ runId: current.runs[0]!.id, started: true });
  let onDetail: (id: string) => Promise<Detail> = async () => current;
  let onRespond: () => Promise<{ accepted: boolean }> = async () => ({ accepted: true });
  const api: NativeTaskRunApi = {
    subscribe: (name, handler) => { listeners.set(name, handler as (event: Event) => void); return { ready, stop: () => { unlistens += 1; listeners.delete(name); } }; },
    start: async (id) => { starts += 1; return onStart(id); }, detail: (id) => onDetail(id),
    stop: async () => { stops += 1; const task = { ...current.task, status: "stopped" as const }; delete task.activeRunId; current = { ...current, task }; return { task }; },
    respond: async (params) => { decisions.push(params); return onRespond(); },
  };
  const runtime = new NativeTaskRunRuntime((value) => { snapshot = value; useNativeTaskRunStore.setState(value); }, api, () => now);
  return { runtime, get snapshot() { return snapshot; }, get starts() { return starts; }, get stops() { return stops; }, get unlistens() { return unlistens; }, decisions,
    setReady: (value: Promise<void>) => { ready = value; }, setDetail: (value: Detail) => { current = value; },
    onStart: (value: typeof onStart) => { onStart = value; }, onDetail: (value: typeof onDetail) => { onDetail = value; }, onRespond: (value: typeof onRespond) => { onRespond = value; },
    emit: (event: Event) => { const parsed = AGENT_EVENT_MEMBER_SCHEMAS[event.type].parse(event); listeners.get(event.type)?.(parsed); },
  };
}
async function startedWithApproval(h: ReturnType<typeof harness>, ticket = approval()) {
  h.onStart(async () => {
    h.emit({ type: "agent.started", runId: ticket.runId, taskId: "task_fixture", at });
    h.emit(waiting(ticket));
    return { runId: ticket.runId, started: true };
  });
  await h.runtime.start("task_fixture");
  return h.snapshot.runs[0]!.approval!;
}

test("waits for subscription readiness and accepts started/approval before task_start resolves", async () => {
  const h = harness();
  const ready = deferred<void>(); h.setReady(ready.promise);
  const foreground = new RunLifecycle(); foreground.begin("ordinary_chat"); foreground.started("ordinary_chat");
  const pending = startedWithApproval(h);
  await settle(); assert.equal(h.starts, 0);
  ready.resolve(); const actual = await pending;
  assert.equal(h.starts, 1);
  assert.equal(h.snapshot.runs[0]?.state, "waiting_approval");
  assert.equal(actual.approvalId, "apr_fixture");
  assert.equal(h.snapshot.runs[0]?.sessionId, "session_actual_from_host");
  assert.equal(foreground.isActive("ordinary_chat"), true);
  assert.deepEqual(actual.input, approval().input);
  h.runtime.disconnect(); assert.equal(h.unlistens, 5);
});

test("only the response's real run is adopted; foreign window/session events do not claim chat", async () => {
  const h = harness();
  h.onStart(async () => {
    h.emit(waiting(approval("other_run"), "task_fixture"));
    h.emit(waiting(approval("foreign_run"), "foreign_task"));
    h.emit(waiting()); return { runId: "run_fixture", started: true };
  });
  await h.runtime.start("task_fixture");
  assert.deepEqual(h.snapshot.runs.map((run) => run.runId), ["run_fixture"]);
  assert.equal(visibleNativeTaskRuns(h.snapshot.runs, { sessionId: "unrelated_chat", serverId: "srv_fixture" }).length, 0);
  assert.equal(visibleNativeTaskRuns(h.snapshot.runs, { sessionId: "session_actual_from_host", serverId: "srv_other" }).length, 0);
  assert.equal(visibleNativeTaskRuns(h.snapshot.runs, { taskId: "foreign_task" }).length, 0);
  h.runtime.disconnect();
});

test("task and actual session expose the same pending ticket across switching, with exact inputs", async () => {
  const h = harness(); const ticket = await startedWithApproval(h);
  const taskHtml = renderToStaticMarkup(<NativeTaskApprovalCards snapshot={h.snapshot} scope={{ taskId: "task_fixture" }} />);
  const chatHtml = renderToStaticMarkup(<NativeTaskApprovalCards snapshot={h.snapshot} scope={{ sessionId: "session_actual_from_host", serverId: "srv_fixture" }} />);
  assert.match(taskHtml, /批准一次/); assert.match(chatHtml, /拒绝/);
  assert.match(chatHtml, /4096/); assert.match(chatHtml, /10000/); assert.match(chatHtml, /id/);
  assert.doesNotMatch(chatHtml, /本次运行批准/);
  assert.equal(renderToStaticMarkup(<NativeTaskApprovalCards snapshot={h.snapshot} scope={{ sessionId: "other_session", serverId: "srv_fixture" }} />), "");
  assert.equal(visibleNativeTaskRuns(h.snapshot.runs, { taskId: "task_fixture" })[0]?.approval, ticket);
  assert.equal(visibleNativeTaskRuns(h.snapshot.runs, { sessionId: "session_actual_from_host", serverId: "srv_fixture" })[0]?.approval, ticket);
  h.runtime.disconnect();
});

test("two views share one immediate submission lock; accepted/duplicate events cannot re-enable it", async () => {
  const h = harness(); const ticket = await startedWithApproval(h); const response = deferred<{ accepted: boolean }>();
  h.onRespond(() => response.promise);
  const pending = h.runtime.respond(ticket, "approve_once");
  await h.runtime.respond(ticket, "reject");
  assert.equal(h.decisions.length, 1); assert.equal(h.snapshot.runs[0]?.pending, true);
  const html = renderToStaticMarkup(<NativeTaskApprovalCards snapshot={h.snapshot} scope={{ taskId: "task_fixture" }} />);
  assert.match(html, /disabled/);
  h.emit(waiting()); assert.equal(h.snapshot.runs[0]?.pending, true);
  response.resolve({ accepted: true }); await pending;
  assert.equal(h.snapshot.runs[0]?.decision, "已批准一次");
  h.emit(waiting()); await h.runtime.respond(ticket, "approve_once");
  assert.equal(h.decisions.length, 1);
  assert.doesNotMatch(renderToStaticMarkup(<NativeTaskApprovalCards snapshot={h.snapshot} scope={{ taskId: "task_fixture" }} />), /class="approval-actions"/);
  h.runtime.disconnect();
});

test("real sessionGrantable controls both the visible decision and submission; per-call tickets reject it", async () => {
  const h = harness(); const perCall = await startedWithApproval(h);
  await h.runtime.respond(perCall, "approve_session"); assert.equal(h.decisions.length, 0);
  const allowed = approval("run_fixture", { approvalId: "apr_session", toolName: "filesystem.write", sessionGrantable: true });
  h.emit(waiting(allowed)); const actual = h.snapshot.runs[0]!.approval!;
  assert.match(renderToStaticMarkup(<NativeTaskApprovalCards snapshot={h.snapshot} scope={{ taskId: "task_fixture" }} />), /本次运行批准/);
  await h.runtime.respond(actual, "approve_session");
  assert.equal(h.decisions[0]?.decision, "approve_session"); assert.equal(h.snapshot.runs[0]?.decision, "已批准本次运行");
  h.runtime.disconnect();
});

test("expired and stopped tickets cannot be submitted, and stop clears pending payloads", async () => {
  const h = harness(); const ticket = await startedWithApproval(h, approval("run_fixture", { expiresAt: at }));
  await h.runtime.respond(ticket, "approve_once"); assert.equal(h.decisions.length, 0);
  assert.equal(h.snapshot.runs[0]?.decision, "审批已过期");
  await h.runtime.stop("task_fixture"); assert.equal(h.snapshot.runs.length, 0);
  h.emit(waiting()); await h.runtime.respond(ticket, "approve_once");
  assert.equal(h.snapshot.runs.length, 0); assert.equal(h.decisions.length, 0); assert.equal(h.starts, 1); assert.equal(h.stops, 1);
  h.runtime.disconnect();
});

test("failed task_get cannot accept buffered tickets; the real task can still be stopped without restart", async () => {
  const h = harness(); h.onStart(async () => { h.emit(waiting()); return { runId: "run_fixture", started: true }; });
  h.onDetail(async () => { throw new Error("inert metadata read failed"); });
  await assert.rejects(h.runtime.start("task_fixture"), /metadata read failed/);
  assert.equal(h.snapshot.runs.length, 0);
  await h.runtime.stop("task_fixture");
  assert.equal(h.starts, 1); assert.equal(h.stops, 1); assert.equal(h.decisions.length, 0);
  h.runtime.disconnect();
});

test("mismatched active run, oversized cache, and terminal results never synthesize approval", async () => {
  for (const variant of ["mismatch", "oversize", "terminal"] as const) {
    const h = harness(); const value = detail();
    if (variant === "mismatch") value.task.activeRunId = "other_run";
    if (variant === "terminal") { value.runs[0]!.status = "completed"; value.task.activeRunId = undefined; }
    h.setDetail(value);
    h.onStart(async () => { h.emit(waiting(approval("run_fixture", variant === "oversize" ? { input: "x".repeat(300000) } : {}))); return { runId: "run_fixture", started: true }; });
    if (variant === "terminal") { await h.runtime.start("task_fixture"); assert.equal(h.snapshot.runs[0]?.approval, undefined); }
    else { await assert.rejects(h.runtime.start("task_fixture"), /未能核验/); assert.equal(h.snapshot.runs.length, 0); }
    assert.equal(h.decisions.length, 0); h.runtime.disconnect();
  }
});

test("old completion cannot end a new run and a late waiting event cannot revive a terminal one", async () => {
  const h = harness(); await startedWithApproval(h); h.emit(completed()); h.emit(waiting());
  assert.equal(h.snapshot.runs[0]?.state, "completed"); assert.equal(h.snapshot.runs[0]?.decision, "本次运行已结束");
  h.setDetail(detail("task_fixture", "run_next"));
  h.onStart(async () => { h.emit(waiting(approval("run_next", { approvalId: "apr_next" }))); return { runId: "run_next", started: true }; });
  await h.runtime.start("task_fixture"); h.emit(completed());
  assert.equal(h.snapshot.runs.find((run) => run.runId === "run_next")?.state, "waiting_approval");
  h.runtime.interrupt();
  assert.equal(h.snapshot.runs.find((run) => run.runId === "run_next")?.approval, undefined);
  h.runtime.disconnect();
});

test("disconnect during admission releases all listeners and does not start or adopt after close", async () => {
  const h = harness(); const ready = deferred<void>(); h.setReady(ready.promise);
  const pending = h.runtime.start("task_fixture"); h.runtime.disconnect(); ready.resolve();
  await assert.rejects(pending, /已关闭/);
  assert.equal(h.starts, 0); assert.equal(h.unlistens, 5); assert.equal(h.snapshot.runs.length, 0);
});

test("cache admission reserves pending slots so parallel starts cannot exceed 64 records", async () => {
  const h = harness();
  h.onStart(async (id) => ({ runId: `run_${id}`, started: true }));
  h.onDetail(async (id) => detail(id, `run_${id}`));
  for (let i = 0; i < 60; i += 1) await h.runtime.start(`task_${i}`);
  const gate = deferred<void>(); h.onStart(async (id) => { await gate.promise; return { runId: `run_${id}`, started: true }; });
  const requests = Array.from({ length: 8 }, (_value, i) => h.runtime.start(`task_parallel_${i}`));
  const collected = Promise.allSettled(requests); await settle(); gate.resolve(); const results = await collected;
  assert.equal(results.filter((result) => result.status === "fulfilled").length, 4);
  assert.equal(h.snapshot.runs.length, 64); assert.equal(h.starts, 64);
  h.runtime.disconnect();
});
