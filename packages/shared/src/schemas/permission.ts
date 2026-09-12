/**
 * Schemas for permission, tools and agent runs. See `types/risk.ts` for the model.
 */

import { z } from "zod";

import { APPROVAL_DECISIONS } from "../types/chat.js";
import { AGENT_PERMISSION_MODES, AGENT_RUN_MODES, PERMISSION_APPROVAL_SOURCES, PERMISSION_MODES, PERMISSION_TIERS } from "../types/risk.js";
import { TOOL_EXECUTION_STATUSES } from "../types/enums.js";
import { EnvironmentSchema, RiskLevelSchema, SERVER_ID_SCHEMA, ToolTargetSchema } from "./server.js";
import { HttpBaseUrlSchema, SafeCustomHeadersSchema } from "./provider.js";

/** Per-run provider material (mirrors `RuntimeProviderConfig`). */
export const RuntimeProviderConfigSchema = z.strictObject({
  kind: z.literal("openai-compatible"),
  baseUrl: HttpBaseUrlSchema,
  model: z.string().trim().min(1).max(256),
  apiKey: z.string().max(4096).optional(),
  customHeaders: SafeCustomHeadersSchema.optional(),
  timeoutMs: z.number().int().min(100).max(10 * 60_000).optional(),
  wireApi: z.enum(["chat", "responses"]).optional(),
});

export const PermissionTierSchema = z.enum(PERMISSION_TIERS);
export const PermissionModeSchema = z.enum(PERMISSION_MODES);
export const AgentPermissionModeSchema = z.enum(AGENT_PERMISSION_MODES);
export const AgentRunModeSchema = z.enum(AGENT_RUN_MODES);
export const PermissionApprovalSourceSchema = z.enum(PERMISSION_APPROVAL_SOURCES);

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
  approvedBy: PermissionApprovalSourceSchema.optional(),
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

export const ApprovalResponseSchema = z.strictObject({
  approvalId: z.string().min(1),
  runId: z.string().min(1),
  decision: z.enum(APPROVAL_DECISIONS),
  respondedAt: z.string(),
});

export const AgentPromptPartSchema = z.strictObject({
  type: z.literal("text"),
  text: z.string().min(1),
});

export const AgentRunRequestSchema = z.strictObject({
  runId: z.string().trim().min(1).max(256),
  sessionId: z.string().trim().min(1).max(256),
  prompt: z.string().trim().min(1).max(100_000),
  messageId: z.string().trim().min(1).max(256).optional(),
  parts: z.array(AgentPromptPartSchema).min(1).max(128).optional(),
  delivery: z.enum(["async", "sync"]).optional(),
  resume: z.boolean().optional(),
  workspaceId: z.string().trim().min(1).max(256).optional(),
  focusServerId: SERVER_ID_SCHEMA.optional(),
  target: ToolTargetSchema.optional(),
  /**
   * Deliberately not `z.enum(BUILTIN_POLICY_IDS)`: the *registry* is the authority on
   * which ids exist (`apps/agent/src/permissions/policy-registry.ts`), and it answers an
   * unknown id with an error that names the id and the known ones. Pinning the built-ins
   * here as well would add a second place to change and turn that error into a generic
   * "invalid params". Bounded, because the value crosses into a decision.
   */
  policyId: z.string().trim().min(1).max(256).optional(),
  permissionMode: AgentPermissionModeSchema.optional(),
  /** Omitted -> `goal`, the unconstrained mode. */
  mode: AgentRunModeSchema.optional(),
  providerConfig: RuntimeProviderConfigSchema.optional(),
});
