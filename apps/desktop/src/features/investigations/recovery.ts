import type { InvestigationTask } from "@yukinal/shared";

/**
 * A host recovery choice has already fenced the previous run. Only the
 * non-terminal `investigating` result is safe to hand back to the durable
 * start entry point; waiting, stop and rollback choices deliberately return a
 * different task state and remain user-controlled.
 *
 * This is the *explicit* user action path (the pane's recovery button). The
 * automatic recovery decision for delegated retryable failures is host-owned
 * and runs once at startup (`commands/recovery.rs`); the pane must not decide it.
 */
export function shouldAutoStartRecoveredTask(
  task: Pick<InvestigationTask, "status" | "activeRunId">,
): boolean {
  return task.status === "investigating" && !task.activeRunId;
}
