/**
 * The run state machine's own contract.
 *
 * These cases live next to the table they exercise rather than next to `AgentLoop`,
 * because they are about the vocabulary of a run — which event is legal in which state,
 * and where "Stop" can land — not about what the loop does with a provider.
 */

import assert from "node:assert/strict";
import test from "node:test";

import type { AgentRunState } from "@yukinal/shared";

import { InvalidTransitionError, isTerminal, transition } from "./run-state.js";

test("the run state machine follows a full cycle", () => {
  const path: AgentRunState[] = [
    transition("idle", "user_prompt"),
    transition("thinking", "tool_call_requested"),
    transition("running_tool", "approval_required"),
    transition("waiting_approval", "approval_granted"),
    transition("running_tool", "tool_completed"),
    transition("thinking", "run_completed"),
  ];
  assert.deepEqual(path, ["thinking", "running_tool", "waiting_approval", "running_tool", "thinking", "completed"]);
});

test("a rejected approval sends the agent back to thinking, not to failed", () => {
  assert.equal(transition("waiting_approval", "approval_rejected"), "thinking");
  assert.equal(transition("waiting_approval", "approval_expired"), "thinking");
});

test("Stop always lands on cancelled from an active state", () => {
  for (const state of ["thinking", "running_tool", "waiting_approval"] as const) {
    assert.equal(transition(state, "user_stop"), "cancelled");
  }
});

test("terminal states are terminal", () => {
  for (const state of ["completed", "failed", "cancelled"] as const) {
    assert.equal(isTerminal(state), true);
    assert.throws(() => transition(state, "user_prompt"), InvalidTransitionError);
  }
  assert.equal(isTerminal("thinking"), false);
});

test("an illegal transition throws instead of silently drifting", () => {
  assert.throws(() => transition("idle", "tool_call_requested"), InvalidTransitionError);
  assert.throws(() => transition("running_tool", "text_delta"), InvalidTransitionError);
});
