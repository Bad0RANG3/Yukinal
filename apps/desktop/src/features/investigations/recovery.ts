import type { InvestigationTask } from "@yukinal/shared";

/**
 * A host recovery choice has already fenced the previous run. Only the
 * non-terminal `investigating` result is safe to hand back to the durable
 * start entry point; waiting, stop and rollback choices deliberately return a
 * different task state and remain user-controlled.
 */
export function shouldAutoStartRecoveredTask(
  task: Pick<InvestigationTask, "status" | "activeRunId">,
): boolean {
  return task.status === "investigating" && !task.activeRunId;
}

/**
 * A retryable transport/timeout failure may be recovered without another click
 * only when the user already delegated this exact executable task. The attempt
 * budget is checked here as a second UI-side guard; the host still owns the
 * durable recovery transaction and can refuse the request.
 */
export function shouldAutoRecoverDelegatedTask(
  task: Pick<
    InvestigationTask,
    "status" | "activeRunId" | "mode" | "permissionMode" | "automationLevel" | "scope"
  > & {
    budget: Pick<InvestigationTask["budget"], "maxAttempts">;
    lastFailure?: Pick<NonNullable<InvestigationTask["lastFailure"]>, "retryable" | "attempt" | "code">;
  },
): boolean {
  const failure = task.lastFailure;
  return (
    task.status === "failed" &&
    !task.activeRunId &&
    task.permissionMode === "auto" &&
    task.mode === "goal" &&
    task.automationLevel === "execute" &&
    task.scope.host === "remote" &&
    (task.scope.environment === "development" || task.scope.environment === "staging") &&
    failure?.retryable === true &&
    failure.attempt < task.budget.maxAttempts
  );
}
