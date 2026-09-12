/**
 * The trace ledger's own contract.
 *
 * These are unit tests of the object the agent loop now depends on for the ids every
 * tool event carries, so what they pin is exactly what a refactor of the loop could
 * break silently: the deferred start event, the status mapping from a tool result, and
 * the fact that a finished trace reports its terminal status once.
 */

import assert from "node:assert/strict";
import test from "node:test";

import type { PermissionDecision, ToolCallResult } from "@yukinal/shared";

import { TraceRecorder, type TraceEvent } from "./trace-recorder.js";

function decision(overrides: Partial<PermissionDecision> = {}): PermissionDecision {
  return {
    outcome: "ask",
    intrinsicRisk: "high",
    finalRisk: "high",
    tier: "dangerous",
    facts: [],
    policyId: "policy.production",
    toolName: "docker.restart",
    reason: "dangerous or critical action cannot be auto-approved",
    target: { host: "remote", serverId: "srv_prd01", environment: "production" },
    requestedAt: "2026-01-01T00:00:00Z",
    ...overrides,
  };
}

function result(overrides: Partial<ToolCallResult> = {}): ToolCallResult {
  return {
    callId: "call_1",
    toolName: "system.echo",
    status: "success",
    traceId: "trc_test",
    startedAt: "2026-01-01T00:00:00Z",
    endedAt: "2026-01-01T00:00:01Z",
    durationMs: 1_000,
    outputSummary: "hello",
    ...overrides,
  };
}

test("a listener attached right after construction still sees trace.started", async () => {
  const seen: TraceEvent[] = [];
  const trace = new TraceRecorder("run_1", "check disk usage");
  trace.subscribe((event) => seen.push(event));

  // The start event is deferred one microtask so this ordering works in the loop,
  // where the recorder is built and subscribed before anything else happens.
  assert.equal(seen.length, 0);
  await Promise.resolve();

  assert.equal(seen.length, 1);
  const started = seen[0];
  assert.equal(started?.type, "trace.started");
  assert.equal(started?.type === "trace.started" ? started.title : undefined, "check disk usage");
});

test("an opened step carries the tool, the input, the intent and a monotonic seq", () => {
  const trace = new TraceRecorder("run_1", "t");
  const first = trace.startToolStep({ title: "docker ps", toolName: "docker.ps", callInput: { all: true } });
  const second = trace.startToolStep({
    title: "filesystem read",
    toolName: "filesystem.read",
    callInput: { path: "/etc/hosts" },
    intent: "read the host table",
  });

  assert.equal(first.status, "running");
  assert.equal(first.seq, 0);
  assert.equal(second.seq, 1);
  assert.equal(second.toolName, "filesystem.read");
  assert.deepEqual(second.input, { path: "/etc/hosts", _intent: "read the host table" });
  assert.equal(trace.steps.length, 2);
});

test("requireApproval moves the step to waiting and publishes the decision", () => {
  const seen: TraceEvent[] = [];
  const trace = new TraceRecorder("run_1", "t");
  const step = trace.startToolStep({ title: "docker restart", toolName: "docker.restart", callInput: {} });
  trace.subscribe((event) => seen.push(event));

  trace.requireApproval(decision(), step.stepId);

  assert.equal(trace.steps[0]?.status, "waiting_approval");
  assert.equal(trace.steps[0]?.kind, "approval");
  const approval = seen.find((event) => event.type === "approval.required");
  assert.equal(approval?.type === "approval.required" ? approval.decision.policyId : undefined, "policy.production");
});

test("a tool result's status maps onto the trace's own vocabulary", () => {
  const trace = new TraceRecorder("run_1", "t");
  const succeeded = trace.startToolStep({ title: "a", toolName: "system.echo", callInput: {} });
  const cancelled = trace.startToolStep({ title: "b", toolName: "system.echo", callInput: {} });
  const failed = trace.startToolStep({ title: "c", toolName: "system.echo", callInput: {} });

  trace.finishToolStep(succeeded.stepId, result());
  trace.finishToolStep(cancelled.stepId, result({ status: "cancelled" }));
  trace.finishToolStep(failed.stepId, result({ status: "failed", error: { code: "timeout", message: "took too long", retryable: true } }));

  assert.deepEqual(
    trace.steps.map((step) => step.status),
    ["done", "skipped", "failed"],
  );
  assert.equal(trace.steps[0]?.outputSummary, "hello");
  assert.equal(trace.steps[1]?.durationMs, 1_000);
  assert.equal(trace.steps[2]?.error, "took too long");
});

test("finish reports the terminal status once, however often it is called", () => {
  const seen: TraceEvent[] = [];
  const trace = new TraceRecorder("run_1", "t");
  trace.subscribe((event) => seen.push(event));

  trace.finish("completed");
  trace.finish("failed");

  const finished = seen.filter((event) => event.type === "trace.finished");
  assert.equal(finished.length, 1);
  assert.equal(finished[0]?.type === "trace.finished" ? finished[0].status : undefined, "completed");
});

test("an unsubscribed listener stops receiving events, and an unknown step is a no-op", () => {
  const seen: TraceEvent[] = [];
  const trace = new TraceRecorder("run_1", "t");
  const unsubscribe = trace.subscribe((event) => seen.push(event));
  unsubscribe();

  const step = trace.startToolStep({ title: "a", toolName: "system.echo", callInput: {} });
  trace.finishToolStep(step.stepId, result());

  assert.deepEqual(seen, []);
  assert.equal(trace.updateStep("stp_not_mine", { status: "done" }), undefined);
  assert.equal(trace.steps[0]?.status, "done");
});
