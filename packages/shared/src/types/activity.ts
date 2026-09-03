import type { TRACE_STEP_STATUSES } from "./enums.js";
import type { Environment, PermissionMode, RiskLevel } from "./risk.js";
import type { ToolExecutionStatus } from "./tool.js";

/**
 * Activity + execution trace.
 *
 * Activity = audit stream (who/what/where/when/why/result).
 * Trace = the Agent's step-by-step working, rendered as cards in the AI panel.
 */

export type ActivitySource = "agent" | "user" | "system" | "docker" | "git" | "cloud";

export const ACTIVITY_TYPES = [
  "connection",
  "authentication",
  "configuration",
  "deployment",
  "service",
  "container",
  "file_change",
  "agent_action",
  "approval",
  "health",
] as const;
export type ActivityType = (typeof ACTIVITY_TYPES)[number];

export interface Activity {
  id: string;
  serverId?: string;
  workspaceId?: string;
  type: ActivityType;
  title: string;
  description?: string;
  source: ActivitySource;
  /** Actor: which agent / which user / which subsystem ("who"). */
  actor: string;
  /** Why it happened — the agent's stated intent, or the triggering event. */
  reason?: string;
  outcome?: "success" | "failure" | "cancelled" | "denied";
  /** Link to the trace record for drill-down. */
  traceId?: string;
  createdAt: string;
}

export type TraceStepStatus = (typeof TRACE_STEP_STATUSES)[number];

/** One visible line in "Agent working...". */
export interface TraceStep {
  traceId: string;
  stepId: string;
  seq: number;
  /** Short verb phrase for the card title, e.g. "Read application logs". */
  title: string;
  status: TraceStepStatus;
  kind: "tool" | "reasoning" | "approval" | "verification" | "note";
  toolName?: string;
  /** Raw provider-facing name is never stored; internal names only (ADR 0004). */
  input?: unknown;
  outputSummary?: string;
  outputPreview?: string;
  outputTruncated?: boolean;
  error?: string;
  startedAt?: string;
  endedAt?: string;
  durationMs?: number;
}

/** Persisted into `tool_executions` (). */
export interface ToolExecutionRecord {
  traceId: string;
  stepId: string;
  callId: string;
  toolName: string;
  serverId?: string;
  environment: Environment;
  riskLevel: RiskLevel;
  /** Which layer decided: "policy" means it was auto-approved by rules, "user" means approved in UI. */
  decision: PermissionMode;
  approvedBy?: "user" | "policy";
  status: ToolExecutionStatus;
  input: unknown;
  output?: unknown;
  error?: string;
  startedAt: string;
  endedAt?: string;
  durationMs?: number;
}
