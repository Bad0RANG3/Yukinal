import assert from "node:assert/strict";
import test from "node:test";

import { RPC_ERROR, type AgentRunState } from "@yukinal/shared";

import { RpcFailure } from "../errors.js";
import { AgentLoop, InvalidTransitionError, isTerminal, transition } from "./agent-loop.js";
import { createRuntime } from "./create-runtime.js";
import type { AgentLogger } from "../config.js";

const noop = (): void => {};
const silent: AgentLogger = { debug: noop, info: noop, warn: noop, error: noop, child: () => silent };

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

test("without a provider the loop refuses to run instead of faking output", async () => {
  const runtime = createRuntime({ log: silent });
  const loop = new AgentLoop(runtime.loop.deps);
  await assert.rejects(
    loop.start(
      {
        runId: "run_1",
        sessionId: "ses_1",
        prompt: "check the api",
        target: { host: "remote", serverId: "srv_1", environment: "staging" },
      },
      { emit: noop },
      undefined as never,
    ),
    (error: unknown) => error instanceof RpcFailure && error.code === RPC_ERROR.NOT_IMPLEMENTED,
  );
});