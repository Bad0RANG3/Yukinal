import type { InvestigationRun, InvestigationTask, TaskRunStatus } from "@yukinal/shared";

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

/** Run states in which the host still owns the task's single live run. */
const ACTIVE_RUN_STATUSES = new Set<TaskRunStatus>(["admitted", "running", "waiting_user"]);

/** Task states in which starting a run is a meaningful request. */
const STARTABLE_TASK_STATUSES = new Set<InvestigationTask["status"]>([
  "pending",
  "investigating",
  "waiting_user",
]);

/**
 * Whether the task still has a live run, so the durable start entry point would
 * refuse another one.
 *
 * Choosing a decision option only records the choice: the follow-up run is
 * started by this window, and the previous run is often still finishing the step
 * it was on. The host permits exactly one live run per task, so asking again in
 * that window is a race, not a user mistake — the pane defers the start until the
 * task is idle instead of surfacing `INVALID_STATE`.
 */
export function taskHasActiveRun(
  task: Pick<InvestigationTask, "activeRunId">,
  runs: readonly Pick<InvestigationRun, "status" | "endedAt">[] = [],
): boolean {
  if (task.activeRunId) return true;
  return runs.some((run) => !run.endedAt && ACTIVE_RUN_STATUSES.has(run.status));
}

/** Whether a start can be requested now, or only after the live run ends. */
export function canStartTaskNow(
  task: Pick<InvestigationTask, "status" | "activeRunId">,
  runs: readonly Pick<InvestigationRun, "status" | "endedAt">[] = [],
): boolean {
  return STARTABLE_TASK_STATUSES.has(task.status) && !taskHasActiveRun(task, runs);
}

/**
 * Whether a failed start is the single-live-run race instead of a real fault.
 *
 * The host answers a duplicate start with `INVALID_STATE` and that message, so
 * the pane can treat exactly that answer as "wait for the current run to finish"
 * and keep every other failure visible.
 */
export function isActiveRunConflict(error: unknown): boolean {
  const message = error instanceof Error ? error.message : String(error);
  return /already has an active Agent run/i.test(message);
}
