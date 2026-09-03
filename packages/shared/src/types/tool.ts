/**
 * Tool system contracts.
 *
 * Rule (ADR 0004): tool names are dot-namespaced internally (`docker.ps`) and
 * double-underscored at the LLM boundary (`docker__ps`). See naming/tool-name.ts.
 */

import type { TOOL_EXECUTION_STATUSES } from "./enums.js";
import type { Environment, RiskLevel } from "./risk.js";

export type ToolExecutionStatus = (typeof TOOL_EXECUTION_STATUSES)[number];

/** JSON Schema (draft 2020-12) derived from the Tool's zod schema. */
export type JsonSchema = Record<string, unknown>;

export interface RetryPolicy {
  maxAttempts: number;
  backoffMs: number;
}

/**
 * Everything the runtime needs to know about a tool *before* calling it.
 * Declared by the tool, consumed by registry + permission engine + trace UI.
 */
export interface ToolDeclaration {
  /** Internal dot-namespaced name, e.g. `docker.logs`. */
  name: string;
  description: string;
  /** Base risk. A floor, never a ceiling (). */
  risk: RiskLevel;
  timeoutMs: number;
  cancellable: boolean;
  retry: RetryPolicy;
  inputSchema: JsonSchema;
  /** Grouping for UI + MCP provenance. */
  origin: ToolOrigin;
}

export type ToolOrigin =
  | { kind: "builtin" }
  | { kind: "mcp"; serverId: string }
  | { kind: "provider"; providerId: string };

/** every call carries a resolved, stable target. */
export interface ToolTarget {
  host: "local" | "remote";
  serverId?: string;
  workspaceId?: string;
  environment: Environment;
}

export interface ToolCallRequest {
  callId: string;
  /** The run this call belongs to; every call must be traceable (-R8). */
  traceId: string;
  /** Internal name (`docker.ps`), not the LLM-facing name. */
  toolName: string;
  input: unknown;
  target: ToolTarget;
  /** Why the agent believes this call is needed — shown in the trace. */
  intent?: string;
}

export interface ToolError {
  code:
    | "invalid_input"
    | "denied_by_policy"
    | "approval_rejected"
    | "approval_timeout"
    | "timeout"
    | "cancelled"
    | "not_found"
    | "transport"
    | "execution_failed"
    | "internal";
  message: string;
  detail?: unknown;
  /** Whether retrying with the same input may succeed (retries). */
  retryable: boolean;
}

export interface ToolCallResult {
  callId: string;
  toolName: string;
  status: ToolExecutionStatus;
  output?: unknown;
  error?: ToolError;
  traceId: string;
  startedAt: string;
  endedAt: string;
  durationMs: number;
  /** Truncated raw output the model actually received (observability). */
  outputSummary?: string;
}
