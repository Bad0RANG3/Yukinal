import { z } from "zod";

import { ACTIVITY_OUTCOMES, ACTIVITY_SOURCES, ACTIVITY_TYPES } from "../types/activity.js";
import { TOOL_EXECUTION_STATUSES } from "../types/enums.js";
import { ENVIRONMENTS, PERMISSION_MODES, RISK_LEVELS } from "../types/risk.js";

export const ActivitySchema = z.strictObject({
  id: z.string().min(1),
  serverId: z.string().min(1).optional(),
  workspaceId: z.string().min(1).optional(),
  type: z.enum(ACTIVITY_TYPES),
  title: z.string().min(1),
  description: z.string().optional(),
  source: z.enum(ACTIVITY_SOURCES),
  actor: z.string().min(1),
  reason: z.string().optional(),
  outcome: z.enum(ACTIVITY_OUTCOMES).optional(),
  traceId: z.string().min(1).optional(),
  createdAt: z.string().min(1),
});

export const ToolExecutionRecordSchema = z.strictObject({
  traceId: z.string().min(1),
  stepId: z.string().min(1),
  callId: z.string().min(1),
  toolName: z.string().min(1),
  serverId: z.string().min(1).optional(),
  environment: z.enum(ENVIRONMENTS),
  riskLevel: z.enum(RISK_LEVELS),
  decision: z.enum(PERMISSION_MODES),
  approvedBy: z.enum(["user", "policy"]).optional(),
  status: z.enum(TOOL_EXECUTION_STATUSES),
  input: z.unknown(),
  output: z.unknown().optional(),
  error: z.string().optional(),
  startedAt: z.string().min(1),
  endedAt: z.string().min(1).optional(),
  durationMs: z.number().int().nonnegative().optional(),
});

export const ToolExecutionListResponseSchema = z.strictObject({
  executions: z.array(ToolExecutionRecordSchema),
});
