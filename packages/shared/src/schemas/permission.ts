/**
 * Schemas for permission, tools and agent runs. See `types/risk.ts` for the model.
 */

import { z } from "zod";

import { PERMISSION_MODES, PERMISSION_TIERS } from "../types/risk.js";
import { TOOL_EXECUTION_STATUSES } from "../types/enums.js";
import { EnvironmentSchema, RiskLevelSchema, ToolTargetSchema } from "./server.js";

export const PermissionTierSchema = z.enum(PERMISSION_TIERS);
export const PermissionModeSchema = z.enum(PERMISSION_MODES);

export const PermissionPolicySchema = z.object({
  id: z.string().min(1),
  name: z.string().min(1),
  environment: EnvironmentSchema,
  tiers: z.object({
    read: PermissionModeSchema,
    write: PermissionModeSchema,
    dangerous: PermissionModeSchema,
  }),
  builtin: z.boolean(),
});

/** Facts are auditable input to the decision; they can come from code or from rules. */
export const RiskFactSchema = z.discriminatedUnion("source", [
  z.object({
    source: z.literal("tool"),
    level: RiskLevelSchema,
    toolName: z.string().min(1),
    note: z.string().optional(),
  }),
  z.object({
    source: z.literal("command"),
    level: RiskLevelSchema,
    command: z.string(),
    matched: z.array(z.string()),
    note: z.string().optional(),
  }),
  z.object({
    source: z.literal("environment"),
    level: RiskLevelSchema,
    environment: EnvironmentSchema,
    note: z.string().optional(),
  }),
]);

export const PermissionDecisionSchema = z.object({
  outcome: PermissionModeSchema,
  intrinsicRisk: RiskLevelSchema,
  finalRisk: RiskLevelSchema,
  tier: PermissionTierSchema,
  facts: z.array(RiskFactSchema),
  policyId: z.string(),
  toolName: z.string().min(1),
  reason: z.string(),
  target: ToolTargetSchema,
  approvalId: z.string().optional(),
  requestedAt: z.string(),
});

export const RetryPolicySchema = z.object({
  maxAttempts: z.number().int().min(1).max(10),
  backoffMs: z.number().int().min(0),
});

export const ToolOriginSchema = z.discriminatedUnion("kind", [
  z.object({ kind: z.literal("builtin") }),
  z.object({ kind: z.literal("mcp"), serverId: z.string().min(1) }),
  z.object({ kind: z.literal("provider"), providerId: z.string().min(1) }),
]);

/** A tool that cannot describe its risk and timeout must not be registrable. */
export const ToolDeclarationSchema = z.object({
  name: z.string().min(3),
  description: z.string().min(1),
  risk: RiskLevelSchema,
  timeoutMs: z.number().int().positive(),
  cancellable: z.boolean(),
  retry: RetryPolicySchema,
  inputSchema: z.record(z.string(), z.unknown()),
  origin: ToolOriginSchema,
});

export const ToolExecutionStatusSchema = z.enum(TOOL_EXECUTION_STATUSES);

export const ApprovalResponseSchema = z.object({
  approvalId: z.string().min(1),
  decision: z.enum(["approve_once", "approve_session", "reject"]),
  respondedAt: z.string(),
});

export const AgentRunRequestSchema = z.object({
  runId: z.string().min(1),
  sessionId: z.string().min(1),
  prompt: z.string().min(1),
  workspaceId: z.string().optional(),
  focusServerId: z.string().optional(),
  target: ToolTargetSchema.optional(),
  policyId: z.string().optional(),
});

