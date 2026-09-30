/**
 * Projection boundary for tool lifecycle events.
 *
 * The agent loop owns execution and permission decisions. This module owns the
 * deliberately smaller job of turning those already-made decisions into the
 * redacted event stream consumed by the host and UI. Keeping the projection
 * here makes it harder for a future execution path to accidentally emit raw
 * tool arguments or output summaries.
 */

import {
  ServerExecInterruptionSchema,
  type AgentStreamEvent,
  type PermissionApprovalSource,
  type PermissionDecision,
  type ServerExecInterruption,
  type ToolCallRequest,
  type ToolDeclaration,
  type ToolError,
} from "@yukinal/shared";

import { redactSensitiveText, redactSensitiveValue } from "../security/sensitive-data.js";

interface ToolEventBase {
  traceId: string;
  stepId: string;
  callId: string;
  toolName: string;
  input: unknown;
  target: ToolCallRequest["target"];
  riskLevel: PermissionDecision["finalRisk"];
  decision: PermissionDecision["outcome"];
  approvedBy?: PermissionApprovalSource;
  /** The policy actually applied by the permission engine. */
  policyId: PermissionDecision["policyId"];
  /** Built-in and MCP tools must remain distinguishable in the audit stream. */
  origin: ToolDeclaration["origin"];
  planId?: string;
  planStepId?: string;
  evidenceIds?: string[];
}

export interface ToolCallEventInput extends ToolEventBase {}

export interface ToolResultEventInput extends ToolEventBase {
  errorCode?: ToolError["code"];
  executionState?: ServerExecInterruption["state"];
  status: "success" | "failed" | "cancelled";
  outputSummary: string;
  error?: string;
  startedAt: string;
  endedAt: string;
  durationMs: number;
}

export function createToolEventEmitter({
  runId,
  emit,
  now,
}: {
  runId: string;
  emit: (event: AgentStreamEvent) => void;
  now: () => string;
}) {
  const emitToolCall = (call: ToolCallEventInput): void => {
    emit({
      type: "agent.tool_call",
      runId,
      traceId: call.traceId,
      stepId: call.stepId,
      callId: call.callId,
      toolName: call.toolName,
      input: redactSensitiveValue(call.input),
      target: call.target,
      riskLevel: call.riskLevel,
      decision: call.decision,
      approvedBy: call.approvedBy,
      policyId: call.policyId,
      origin: call.origin,
      planId: call.planId,
      planStepId: call.planStepId,
      evidenceIds: call.evidenceIds,
      at: now(),
    });
  };

  const emitToolResult = (result: ToolResultEventInput): void => {
    emit({
      type: "agent.tool_result",
      runId,
      traceId: result.traceId,
      stepId: result.stepId,
      callId: result.callId,
      toolName: result.toolName,
      input: redactSensitiveValue(result.input),
      target: result.target,
      riskLevel: result.riskLevel,
      decision: result.decision,
      approvedBy: result.approvedBy,
      policyId: result.policyId,
      origin: result.origin,
      planId: result.planId,
      planStepId: result.planStepId,
      evidenceIds: result.evidenceIds,
      errorCode: result.errorCode,
      executionState: result.executionState,
      status: result.status,
      outputSummary: redactSensitiveText(result.outputSummary),
      error: result.error === undefined ? undefined : redactSensitiveText(result.error),
      startedAt: result.startedAt,
      endedAt: result.endedAt,
      durationMs: result.durationMs,
      at: result.endedAt,
    });
  };

  return { emitToolCall, emitToolResult };
}

/** Project only the bounded state discriminator; stdout/stderr remain in the redacted summary. */
export function serverExecInterruptionState(
  toolName: string,
  detail: unknown,
): ServerExecInterruption["state"] | undefined {
  if (toolName !== "server.exec") return undefined;
  const parsed = ServerExecInterruptionSchema.safeParse(detail);
  return parsed.success ? parsed.data.state : undefined;
}
