/**
 * Execution trace (-R8).
 *
 * The recorder is an in-memory, per-run ledger that *emits* as it goes: the UI must
 * see steps while the agent works, not after. Persistence happens in Rust
 * (`tool_executions` / `activities` tables) from the same events.
 */

import { randomUUID } from "node:crypto";

import type { PermissionDecision, ToolCallResult, TraceStep } from "@yukinal/shared";

export type TraceEvent =
  | { type: "trace.started"; traceId: string; runId: string; title: string; at: string }
  | { type: "step.started"; traceId: string; step: TraceStep }
  | { type: "step.updated"; traceId: string; step: TraceStep }
  | { type: "approval.required"; traceId: string; decision: PermissionDecision }
  | {
      type: "trace.finished";
      traceId: string;
      status: "completed" | "failed" | "cancelled";
      at: string;
    };

export type TraceListener = (event: TraceEvent) => void;

export class TraceRecorder {
  readonly traceId: string;
  readonly steps: TraceStep[] = [];
  readonly #listeners = new Set<TraceListener>();
  #seq = 0;
  #ended = false;

  constructor(
    private readonly runId: string,
    private readonly title: string,
    private readonly clock: () => string = () => new Date().toISOString(),
  ) {
    this.traceId = `trc_${randomUUID()}`;
    // Deferred one microtask so a listener attached immediately after construction
    // still observes the start -- without this the first event is always lost.
    queueMicrotask(() => {
      this.#emit({
        type: "trace.started",
        traceId: this.traceId,
        runId: this.runId,
        title: this.title,
        at: this.clock(),
      });
    });
  }

  subscribe(listener: TraceListener): () => void {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  }

  startToolStep(input: {
    title: string;
    toolName: string;
    callInput: unknown;
    intent?: string;
  }): TraceStep {
    const step: TraceStep = {
      traceId: this.traceId,
      stepId: `stp_${randomUUID()}`,
      seq: this.#seq++,
      title: input.title,
      status: "running",
      kind: "tool",
      toolName: input.toolName,
      input: input.intent === undefined ? input.callInput : { ...toObject(input.callInput), _intent: input.intent },
      startedAt: this.clock(),
    };
    this.steps.push(step);
    this.#emit({ type: "step.started", traceId: this.traceId, step });
    return step;
  }

  updateStep(stepId: string, patch: Partial<Omit<TraceStep, "stepId" | "traceId" | "seq">>): TraceStep | undefined {
    const step = this.steps.find((candidate) => candidate.stepId === stepId);
    if (!step) return undefined;
    Object.assign(step, patch);
    this.#emit({ type: "step.updated", traceId: this.traceId, step: { ...step } });
    return step;
  }

  requireApproval(decision: PermissionDecision, stepId: string): void {
    this.updateStep(stepId, { status: "waiting_approval", kind: "approval" });
    this.#emit({ type: "approval.required", traceId: this.traceId, decision });
  }

  finishToolStep(stepId: string, result: ToolCallResult): void {
    this.updateStep(stepId, {
      status: result.status === "success" ? "done" : result.status === "cancelled" ? "skipped" : "failed",
      outputSummary: result.outputSummary ?? result.error?.message ?? "",
      error: result.error?.message,
      endedAt: this.clock(),
      durationMs: result.durationMs,
    });
  }

  finish(status: "completed" | "failed" | "cancelled"): void {
    if (this.#ended) return;
    this.#ended = true;
    this.#emit({ type: "trace.finished", traceId: this.traceId, status, at: this.clock() });
  }

  #emit(event: TraceEvent): void {
    for (const listener of this.#listeners) listener(event);
  }
}

function toObject(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null ? (value as Record<string, unknown>) : { value };
}
