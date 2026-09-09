/** Runtime gate for agent.stream notifications crossing the sidecar boundary. */

import { z } from "zod";

import { RiskLevelSchema, ToolTargetSchema } from "./server.js";
import { PermissionApprovalSourceSchema, PermissionModeSchema } from "./permission.js";

const RunIdSchema = z.string().trim().min(1).max(256);
const TimestampSchema = z.string().min(1).max(80);
const AgentRunStateSchema = z.enum(["idle", "thinking", "running_tool", "waiting_approval", "completed", "failed", "cancelled"]);

const ApprovalRequestSchema = z.strictObject({
  approvalId: z.string().trim().min(1).max(256),
  runId: RunIdSchema,
  toolName: z.string().trim().min(1).max(256),
  input: z.unknown(),
  reason: z.string().max(4_000),
  factsSummary: z.array(z.string().max(1_000)).max(32),
  target: ToolTargetSchema,
  expiresAt: TimestampSchema,
});

const AgentRunResultSchema = z.strictObject({
  runId: RunIdSchema,
  state: AgentRunStateSchema,
  text: z.string().max(200_000),
  steps: z.number().int().nonnegative(),
  toolCalls: z.number().int().nonnegative(),
  error: z.string().max(4_000).optional(),
});

/** Events the desktop is allowed to render from the sidecar. */
export const AgentStreamEventSchema = z.discriminatedUnion("type", [
  z.strictObject({ type: z.literal("agent.started"), runId: RunIdSchema, at: TimestampSchema }),
  z.strictObject({ type: z.literal("agent.thinking"), runId: RunIdSchema, textDelta: z.string().max(20_000).optional(), at: TimestampSchema }),
  z.strictObject({ type: z.literal("agent.text"), runId: RunIdSchema, textDelta: z.string().max(20_000), at: TimestampSchema }),
  z.strictObject({
    type: z.literal("agent.tool_call"),
    runId: RunIdSchema,
    traceId: z.string().trim().min(1).max(256),
    stepId: z.string().trim().min(1).max(256),
    callId: z.string().trim().min(1).max(256),
    toolName: z.string().trim().min(1).max(256),
    input: z.unknown(),
    target: ToolTargetSchema,
    riskLevel: RiskLevelSchema,
    decision: PermissionModeSchema,
    approvedBy: PermissionApprovalSourceSchema.optional(),
    at: TimestampSchema,
  }),
  z.strictObject({
    type: z.literal("agent.tool_result"),
    runId: RunIdSchema,
    traceId: z.string().trim().min(1).max(256),
    stepId: z.string().trim().min(1).max(256),
    callId: z.string().trim().min(1).max(256),
    toolName: z.string().trim().min(1).max(256),
    input: z.unknown(),
    target: ToolTargetSchema,
    riskLevel: RiskLevelSchema,
    decision: PermissionModeSchema,
    approvedBy: PermissionApprovalSourceSchema.optional(),
    status: z.enum(["success", "failed", "cancelled"]),
    outputSummary: z.string().max(4_000),
    error: z.string().max(4_000).optional(),
    startedAt: TimestampSchema,
    endedAt: TimestampSchema,
    durationMs: z.number().nonnegative(),
    at: TimestampSchema,
  }),
  z.strictObject({ type: z.literal("agent.waiting_approval"), runId: RunIdSchema, approval: ApprovalRequestSchema, at: TimestampSchema }),
  z.strictObject({ type: z.literal("agent.approval_expired"), runId: RunIdSchema, approvalId: RunIdSchema, at: TimestampSchema }),
  z.strictObject({ type: z.literal("agent.completed"), runId: RunIdSchema, result: AgentRunResultSchema, at: TimestampSchema }),
  z.strictObject({ type: z.literal("agent.failed"), runId: RunIdSchema, error: z.string().max(4_000), at: TimestampSchema }),
]);
