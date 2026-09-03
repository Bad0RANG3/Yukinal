/**
 * Agent Loop — the run state machine is implemented here; the LLM
 * turn-taking is not implemented yet, which is why `AgentLoop.start` refuses to pretend.
 *
 * The intended shape, kept next to the state machine it must obey:
 *
 *   user message
 *     -> ContextEngine.build
 *     -> provider.stream(messages, tools) (ADR 0003, name mapping ADR 0004)
 *     -> tool call?
 *          no  -> text -> completed
 *          yes -> PermissionEngine.evaluate()      (ADR 0005, never the model's call)
 *               -> auto  -> ToolRegistry.execute(policy_auto)
 *               -> ask   -> approval request -> wait -> execute(user_approved) | denied result
 *               -> deny  -> denied result to the model
 *          -> result back into messages -> loop (bounded by maxSteps)
 *     -> verification step before claiming success
 *     -> report
 */

import {
  RPC_ERROR,
  type AgentRunRequest,
  type AgentRunResult,
  type AgentRunState,
  type AgentStreamEvent,
} from "@yukinal/shared";

import type { LLMProvider } from "@yukinal/provider-sdk";

import { ContextEngine } from "../context/context-engine.js";
import { PermissionEngine } from "../permissions/permission-engine.js";
import { ToolRegistry } from "../tools/registry.js";
import { RpcFailure } from "../errors.js";

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

export interface AgentLoopDeps {
  /** Absent until the agent loop wires the OpenAI-compatible provider (ADR 0003). */
  provider?: LLMProvider;
  registry: ToolRegistry;
  permission: PermissionEngine;
  context: ContextEngine;
  /** multi-step execution must be bounded. */
  maxSteps?: number;
}

export interface AgentRunHooks {
  emit(event: AgentStreamEvent): void;
  signal?: AbortSignal;
}

export class AgentLoop {
  readonly maxSteps: number;

  constructor(readonly deps: AgentLoopDeps) {
    this.maxSteps = deps.maxSteps ?? 25;
  }

  get ready(): boolean {
    return this.deps.provider !== undefined;
  }

  async start(_request: AgentRunRequest, _hooks: AgentRunHooks): Promise<AgentRunResult> {
    if (!this.ready) {
      throw new RpcFailure(
        RPC_ERROR.NOT_IMPLEMENTED,
        "agent loop requires a configured LLM provider before it can run",
      );
    }
    throw new RpcFailure(RPC_ERROR.NOT_IMPLEMENTED, "multi-step agent loop is not implemented yet");
  }
}
