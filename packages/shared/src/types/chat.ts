/**
 * Agent run + chat contracts.
 *
 * These types are the vocabulary between: React <-> agent-sdk <-> apps/agent <-> LLM.
 */

import type { AGENT_RUN_STATES } from "./enums.js";
import type { RuntimeProviderConfig } from "./provider.js";
import type { ToolTarget } from "./tool.js";

export type AgentRunState = (typeof AGENT_RUN_STATES)[number];

export interface ChatSession {
  id: string;
  workspaceId?: string;
  serverId?: string;
  title: string;
  createdAt: string;
  updatedAt: string;
}

export interface ChatMessage {
  id: string;
  sessionId: string;
  role: "user" | "assistant" | "tool" | "system";
  content: string;
  /** Set for role === "tool": links the bubble to its trace card. */
  traceId?: string;
  createdAt: string;
}

/** What UI sends to start a run.: targets are ids, not prose. */
export interface AgentRunRequest {
  runId: string;
  sessionId: string;
  prompt: string;
  workspaceId?: string;
  /** Currently focused server; the agent may still need to disambiguate. */
  focusServerId?: string;
  target?: ToolTarget;
  /** Overrides the policy that would otherwise be derived from the environment. */
  policyId?: string;
  /** Durable sidecar needs a per-run provider: Rust resolves and injects this. */
  providerConfig?: RuntimeProviderConfig;
}

/** `agent.run.stop` params: stop decouples from the run only by its id. */
export interface AgentRunStopParams {
  runId: string;
}

export interface AgentRunResult {
  runId: string;
  state: AgentRunState;
  /** Final assistant text, also streamed as chunks. */
  text: string;
  steps: number;
  toolCalls: number;
  error?: string;
}

/** Approval round-trip (). */
export interface ApprovalRequest {
  approvalId: string;
  runId: string;
  toolName: string;
  input: unknown;
  reason: string;
  /** Pre-rendered, risk-ordered explanation of *why* this needs approval. */
  factsSummary: string[];
  target: ToolTarget;
  expiresAt: string;
}

/** "approve_session" only ever widens within the current run, never across runs. */
export type ApprovalDecision = "approve_once" | "approve_session" | "reject";

export interface ApprovalResponse {
  approvalId: string;
  decision: ApprovalDecision;
  respondedAt: string;
}

export type AgentStreamEvent =
  | { type: "agent.started"; runId: string; at: string }
  | { type: "agent.thinking"; runId: string; textDelta?: string; at: string }
  | { type: "agent.text"; runId: string; textDelta: string; at: string }
  | { type: "agent.tool_call"; runId: string; traceId: string; stepId: string; toolName: string; input: unknown; at: string }
  | { type: "agent.tool_result"; runId: string; traceId: string; stepId: string; toolName: string; status: "success" | "failed" | "cancelled"; outputSummary: string; at: string }
  | { type: "agent.waiting_approval"; runId: string; approval: ApprovalRequest; at: string }
  | { type: "agent.completed"; runId: string; result: AgentRunResult; at: string }
  | { type: "agent.failed"; runId: string; error: string; at: string };
