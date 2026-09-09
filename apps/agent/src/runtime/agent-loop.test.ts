import assert from "node:assert/strict";
import test from "node:test";
import { z } from "zod";

import { RPC_ERROR, type AgentRunState, type AgentStreamEvent } from "@yukinal/shared";
import type { LLMProvider } from "@yukinal/provider-sdk";

import { createEmptyContextSource } from "../context/empty-source.js";
import { ContextEngine } from "../context/context-engine.js";
import { RpcFailure } from "../errors.js";
import { PermissionEngine } from "../permissions/permission-engine.js";
import { ToolRegistry } from "../tools/registry.js";
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

test("approval responses are bound to the run that displayed them", async () => {
  const registry = new ToolRegistry();
  registry.register({
    name: "danger.test",
    description: "Test action requiring approval",
    risk: "high",
    timeoutMs: 1_000,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    input: z.strictObject({}),
    execute: async () => ({ ok: true }),
  });
  const loop = new AgentLoop({
    registry,
    permission: new PermissionEngine(),
    context: new ContextEngine(createEmptyContextSource()),
  });

  let calls = 0;
  const provider: LLMProvider = {
    id: "test-provider",
    model: "test-model",
    async listModels() {
      return [];
    },
    async *stream() {
      calls += 1;
      if (calls === 1) {
        yield { type: "tool_call", call: { id: "call_approval", name: "danger__test", arguments: {} } };
        yield { type: "done", finishReason: "tool_calls" };
      } else {
        yield { type: "text_delta", text: "approved" };
        yield { type: "done", finishReason: "stop" };
      }
    },
  };

  let resolveApproval: ((approval: Extract<AgentStreamEvent, { type: "agent.waiting_approval" }>['approval']) => void) | undefined;
  const approval = new Promise<Extract<AgentStreamEvent, { type: "agent.waiting_approval" }>['approval']>((resolve) => {
    resolveApproval = resolve;
  });
  const run = loop.start(
    {
      runId: "run_approval",
      sessionId: "ses_approval",
      prompt: "run the protected test action",
      target: { host: "remote", serverId: "srv_approval", environment: "production" },
    },
    {
      emit: (event) => {
        if (event.type === "agent.waiting_approval") resolveApproval?.(event.approval);
      },
    },
    provider,
  );

  const request = await approval;
  const response = { approvalId: request.approvalId, decision: "approve_once" as const, respondedAt: new Date().toISOString() };
  assert.equal(loop.respondApproval({ ...response, runId: "run_other" }), false);
  assert.equal(loop.respondApproval({ ...response, runId: "run_approval" }), true);
  const result = await run;
  assert.equal(result.state, "completed", JSON.stringify(result));
  assert.equal(result.toolCalls, 1);
});

test("auto mode lets the Agent self-approve an allowed tool and labels the audit source", async () => {
  const registry = new ToolRegistry();
  registry.register({
    name: "danger.test",
    description: "Test action eligible for delegated approval",
    risk: "high",
    timeoutMs: 1_000,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    input: z.strictObject({}),
    execute: async () => ({ ok: true }),
  });
  const loop = new AgentLoop({
    registry,
    permission: new PermissionEngine(),
    context: new ContextEngine(createEmptyContextSource()),
  });

  let calls = 0;
  const provider: LLMProvider = {
    id: "test-provider",
    model: "test-model",
    async listModels() {
      return [];
    },
    async *stream() {
      calls += 1;
      if (calls === 1) {
        yield { type: "tool_call", call: { id: "call_agent_auto", name: "danger__test", arguments: {} } };
        yield { type: "done", finishReason: "tool_calls" };
      } else {
        yield { type: "text_delta", text: "已完成委托执行" };
        yield { type: "done", finishReason: "stop" };
      }
    },
  };

  const events: AgentStreamEvent[] = [];
  const result = await loop.start(
    {
      runId: "run_agent_auto",
      sessionId: "ses_agent_auto",
      prompt: "run the delegated test action",
      permissionMode: "auto",
      target: { host: "remote", serverId: "srv_agent_auto", environment: "production" },
    },
    { emit: (event) => events.push(event) },
    provider,
  );

  assert.equal(result.state, "completed", JSON.stringify(result));
  assert.equal(result.toolCalls, 1);
  assert.equal(events.some((event) => event.type === "agent.waiting_approval"), false);
  const toolResult = events.find((event) => event.type === "agent.tool_result");
  assert(toolResult && toolResult.type === "agent.tool_result");
  assert.equal(toolResult.approvedBy, "agent");
  assert.equal(toolResult.status, "success");
});

test("approval expiry is streamed and does not leave a live approval", async () => {
  const registry = new ToolRegistry();
  registry.register({
    name: "danger.test",
    description: "Test action requiring approval",
    risk: "high",
    timeoutMs: 1_000,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    input: z.strictObject({}),
    execute: async () => ({ ok: true }),
  });
  const loop = new AgentLoop({
    registry,
    permission: new PermissionEngine(),
    context: new ContextEngine(createEmptyContextSource()),
    approvalTtlMs: 10,
  });

  let calls = 0;
  const provider: LLMProvider = {
    id: "test-provider",
    model: "test-model",
    async listModels() {
      return [];
    },
    async *stream() {
      calls += 1;
      if (calls === 1) {
        yield { type: "tool_call", call: { id: "call_expiry", name: "danger__test", arguments: {} } };
        yield { type: "done", finishReason: "tool_calls" };
      } else {
        yield { type: "text_delta", text: "continued after expiry" };
        yield { type: "done", finishReason: "stop" };
      }
    },
  };

  const events: AgentStreamEvent[] = [];
  const result = await loop.start(
    {
      runId: "run_expiry",
      sessionId: "ses_expiry",
      prompt: "run the protected test action",
      target: { host: "remote", serverId: "srv_expiry", environment: "production" },
    },
    { emit: (event) => events.push(event) },
    provider,
  );

  assert.equal(result.state, "completed", JSON.stringify(result));
  assert.equal(events.some((event) => event.type === "agent.approval_expired" && event.approvalId.length > 0), true);
  const rejected = events.find((event) => event.type === "agent.tool_result");
  assert(rejected && rejected.type === "agent.tool_result");
  assert.equal(rejected.outputSummary, "审批已过期");
  assert.match(result.text, /continued after expiry/);
});
