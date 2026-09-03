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

export const TRACE_STEP_STATUSES = [
  "pending",
  "running",
  "waiting_approval",
  "done",
  "failed",
  "skipped",
] as const;
