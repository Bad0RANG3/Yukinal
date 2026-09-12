/**
 * The run state machine — the vocabulary of a run, and the only legal ways it may move.
 *
 * This is deliberately separate from `AgentLoop`: the loop is the thing that *does* the
 * work (context, provider, permissions, tools), while this module is the thing that says
 * what a run's state is allowed to become. It has no dependencies beyond the shared run
 * state enum, so it can be tested on its own — and the UI can never invent a state the
 * loop could not have produced.
 *
 * `RUN_EVENT_TRANSITIONS` is the table; `transition()` is the table. `isTerminal()` is the
 * question every caller asks before emitting anything else.
 */

import type { AgentRunState } from "@yukinal/shared";

export const RUN_EVENT_TRANSITIONS = {
  idle: ["user_prompt"],
  thinking: ["tool_call_requested", "approval_required", "text_delta", "run_completed", "run_failed", "user_stop"],
  running_tool: ["tool_completed", "approval_required", "run_failed", "user_stop"],
  waiting_approval: ["approval_granted", "approval_rejected", "approval_expired", "user_stop"],
  completed: [],
  failed: [],
  cancelled: [],
} as const satisfies Record<AgentRunState, readonly string[]>;

export type RunEvent =
  | "user_prompt"
  | "text_delta"
  | "tool_call_requested"
  | "tool_completed"
  | "approval_required"
  | "approval_granted"
  | "approval_rejected"
  | "approval_expired"
  | "run_completed"
  | "run_failed"
  | "user_stop";

const TERMINAL: readonly AgentRunState[] = ["completed", "failed", "cancelled"];

export function isTerminal(state: AgentRunState): boolean {
  return TERMINAL.includes(state);
}

/** Pure so the loop can be tested without a model, and so the UI never invents a state. */
export function transition(state: AgentRunState, event: RunEvent): AgentRunState {
  const allowed: readonly string[] = RUN_EVENT_TRANSITIONS[state];
  if (!allowed.includes(event)) {
    throw new InvalidTransitionError(state, event);
  }

  switch (event) {
    case "user_prompt":
      return "thinking";
    case "text_delta":
      return "thinking";
    case "tool_call_requested":
      return "running_tool";
    case "approval_required":
      return "waiting_approval";
    case "approval_granted":
      return "running_tool";
    case "approval_rejected":
    case "approval_expired":
    case "tool_completed":
      return "thinking";
    case "run_completed":
      return "completed";
    case "run_failed":
      return "failed";
    case "user_stop":
      return "cancelled";
  }
}

export class InvalidTransitionError extends Error {
  constructor(state: AgentRunState, event: RunEvent) {
    super(`Cannot apply "${event}" while "${state}"`);
    this.name = "InvalidTransitionError";
  }
}
