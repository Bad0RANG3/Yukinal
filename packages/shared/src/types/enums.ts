/**
 * Const tuples shared by the hand-written types and the zod schemas, so the two
 * can never drift apart silently.
 */

export const TOOL_EXECUTION_STATUSES = [
  "pending",
  "running",
  "waiting_approval",
  "success",
  "failed",
  "cancelled",
] as const;

export const AGENT_RUN_STATES = [
  "idle",
  "thinking",
  "running_tool",
  "waiting_approval",
  "completed",
  "failed",
  "cancelled",
] as const;

/**
 * The outcomes a *finished* tool call can report — deliberately a subset of
 * `TOOL_EXECUTION_STATUSES`, not a second spelling of it.
 *
 * `TOOL_EXECUTION_STATUSES` is the whole lifecycle and includes the in-flight states
 * (`pending`, `running`, `waiting_approval`). A tool *result* cannot be in any of them:
 * by the time one exists the call has stopped. Reusing the six-value tuple here would
 * widen the schema to accept a `success`-shaped result claiming `status: "running"`.
 *
 * `TraceStepStatus`-style "done"/"skipped" do not apply either — those describe a
 * trace step, which can be skipped, while a result either ran to an end or was
 * cancelled.
 *
 * Both homes previously wrote these three literals out separately: `types/chat.ts` as
 * an inline union and `schemas/agent.ts` as an inline `z.enum([...])`.
 */
export const TOOL_RESULT_STATUSES = ["success", "failed", "cancelled"] as const;

export const TRACE_STEP_STATUSES = [
  "pending",
  "running",
  "waiting_approval",
  "done",
  "failed",
  "skipped",
] as const;
