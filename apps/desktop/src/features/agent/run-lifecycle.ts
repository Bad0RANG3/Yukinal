import type { AgentPromptPart } from "@yukinal/shared";

/**
 * A message admitted with `resume: false` but not yet executed.
 *
 * This is the UI half of the durable admission contract: a retry reuses the same
 * `messageId` and `runId` instead of inserting another user message or forking a run.
 * It may only survive a *retryable* failure (the resume call itself failed); any
 * failure that killed the run or the sidecar must drop it, because the host-side
 * admission is gone with it.
 */
export type PendingAdmission = {
  runId: string;
  messageId: string;
  sessionId: string;
  prompt: string;
  parts: AgentPromptPart[];
  taskId?: string | null;
  routingKey: string;
  uiContext?: string;
};

export type AdmissionRef = { current: PendingAdmission | null };

/** Drop the pending admission so the next send starts from a fresh runId/messageId. */
export function resetAdmissionOnFailure(admission: AdmissionRef): void {
  admission.current = null;
}

/** The host can deliver started/completed before the start command resolves. */
export class RunLifecycle {
  runId: string | null = null;
  running = false;
  private requesting = false;
  private expectedRunId: string | null = null;

  begin(expectedRunId?: string): boolean {
    if (this.running || this.requesting) return false;
    this.requesting = true;
    this.running = true;
    this.runId = null;
    this.expectedRunId = expectedRunId ?? null;
    return true;
  }

  started(runId: string): boolean {
    if (!this.requesting || this.runId !== null || (this.expectedRunId !== null && this.expectedRunId !== runId)) return false;
    this.runId = runId;
    return true;
  }

  acknowledge(runId: string): boolean {
    if (this.expectedRunId !== null && this.expectedRunId !== runId) return false;
    const newlyStarted = this.running && this.runId === null;
    if (newlyStarted) this.runId = runId;
    this.requesting = false;
    return newlyStarted;
  }

  isActive(runId: string): boolean {
    return this.running && this.runId === runId;
  }

  finish(runId: string): boolean {
    if (!this.isActive(runId)) return false;
    this.running = false;
    return true;
  }

  fail(): void {
    this.requesting = false;
    this.running = false;
    this.expectedRunId = null;
  }
}
