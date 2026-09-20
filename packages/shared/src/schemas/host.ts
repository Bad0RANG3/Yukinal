import { z } from "zod";

import {
  HOST_CONTEXT_KINDS,
  HOST_METHODS,
  MCP_CATALOG_FAILURE_CODES,
  type HostToolCancelResponse,
  type HostContextResponse,
  type HostMcpCatalogResponse,
  type HostToolExecuteResponse,
  type HostEvidenceRecordResponse,
  type HostEvidenceCompareResponse,
  type HostEvidenceCorrelationResponse,
  type HostEvidenceFetchResponse,
  type HostEvidenceSearchResponse,
  type HostRetentionPreviewResponse,
  type HostFindingRecordResponse,
  type HostBriefRecordResponse,
  type HostPlanRecordResponse,
  type HostPlanCheckResponse,
  type HostPlanStepResultResponse,
  type HostArtifactRecordResponse,
} from "../types/host.js";
import { TOOL_ERROR_CODES } from "../types/tool.js";
import { PLAN_STEP_KINDS } from "../types/investigation.js";
import { ToolTargetSchema } from "./server.js";
import {
  DecisionBriefSchema,
  EvidenceComparisonInputSchema,
  EvidenceComparisonResultSchema,
  EvidenceCorrelationInputSchema,
  EvidenceCorrelationResultSchema,
  EvidenceSchema,
  EvidenceSearchInputSchema,
  EvidenceSearchResultSchema,
  FindingSchema,
  InvestigationPlanDeviationSchema,
  InvestigationPlanSchema,
  InvestigationArtifactSchema,
} from "./investigation.js";
import {
  InvestigationRetentionPreviewInputSchema,
  InvestigationRetentionPreviewSchema,
} from "./retention.js";

export const HostToolExecuteRequestSchema = z.strictObject({
  callId: z.string().min(1),
  traceId: z.string().min(1),
  toolName: z.string().min(1),
  input: z.unknown(),
  target: ToolTargetSchema,
  taskId: z.string().trim().min(1).max(256).optional(),
  planId: z.string().trim().min(1).max(256).optional(),
  planStepId: z.string().trim().min(1).max(256).optional(),
  evidenceIds: z.array(z.string().trim().min(1).max(256)).max(256).optional(),
});

const ToolErrorSchema = z.strictObject({
  code: z.enum(TOOL_ERROR_CODES),
  message: z.string(),
  detail: z.unknown().optional(),
  retryable: z.boolean(),
});

export const HostToolExecuteResponseSchema = z.discriminatedUnion("status", [
  z.strictObject({ status: z.literal("success"), output: z.unknown().optional() }),
  z.strictObject({ status: z.literal("failed"), error: ToolErrorSchema }),
  z.strictObject({ status: z.literal("cancelled"), error: ToolErrorSchema.optional() }),
]) satisfies z.ZodType<HostToolExecuteResponse>;

const HostEvidenceToolErrorSchema = z.strictObject({
  code: z.enum(TOOL_ERROR_CODES),
  message: z.string(),
  detail: z.unknown().optional(),
  retryable: z.boolean(),
});

export const HostEvidenceRecordResponseSchema = z.union([
  z.strictObject({
    recorded: z.literal(true),
    evidenceId: z.string().trim().min(1).max(256).optional(),
    reused: z.boolean().optional(),
  }),
  z.strictObject({ recorded: z.literal(false), error: HostEvidenceToolErrorSchema }),
]) satisfies z.ZodType<HostEvidenceRecordResponse>;

export const HostEvidenceRecordRequestSchema = z.strictObject({ evidence: EvidenceSchema });

export const HostEvidenceFetchRequestSchema = z.strictObject({
  taskId: z.string().trim().min(1).max(256),
  evidenceId: z.string().trim().min(1).max(256),
});

export const HostEvidenceFetchResponseSchema = z.discriminatedUnion("status", [
  z.strictObject({ status: z.literal("success"), evidence: EvidenceSchema }),
  z.strictObject({ status: z.literal("not_found") }),
  z.strictObject({ status: z.literal("failed"), error: HostEvidenceToolErrorSchema }),
]) satisfies z.ZodType<HostEvidenceFetchResponse>;

export const HostEvidenceSearchRequestSchema = z.strictObject({
  taskId: z.string().trim().min(1).max(256),
  ...EvidenceSearchInputSchema.shape,
});

export const HostEvidenceSearchResponseSchema = z.discriminatedUnion("status", [
  z.strictObject({ status: z.literal("success"), ...EvidenceSearchResultSchema.shape }),
  z.strictObject({ status: z.literal("failed"), error: HostEvidenceToolErrorSchema }),
]) satisfies z.ZodType<HostEvidenceSearchResponse>;

export const HostEvidenceCorrelationRequestSchema = z.strictObject({
  taskId: z.string().trim().min(1).max(256),
  ...EvidenceCorrelationInputSchema.shape,
});

export const HostEvidenceCorrelationResponseSchema = z.discriminatedUnion("status", [
  z.strictObject({ status: z.literal("success"), correlation: EvidenceCorrelationResultSchema.shape.correlation }),
  z.strictObject({ status: z.literal("failed"), error: HostEvidenceToolErrorSchema }),
]) satisfies z.ZodType<HostEvidenceCorrelationResponse>;

export const HostEvidenceCompareRequestSchema = z.strictObject({
  taskId: z.string().trim().min(1).max(256),
  ...EvidenceComparisonInputSchema.shape,
});

export const HostEvidenceCompareResponseSchema = z.discriminatedUnion("status", [
  z.strictObject({ status: z.literal("success"), ...EvidenceComparisonResultSchema.shape }),
  z.strictObject({ status: z.literal("failed"), error: HostEvidenceToolErrorSchema }),
]) satisfies z.ZodType<HostEvidenceCompareResponse>;

export const HostRetentionPreviewRequestSchema = InvestigationRetentionPreviewInputSchema;
export const HostRetentionPreviewResponseSchema = z.discriminatedUnion("status", [
  z.strictObject({ status: z.literal("success"), preview: InvestigationRetentionPreviewSchema }),
  z.strictObject({ status: z.literal("failed"), error: HostEvidenceToolErrorSchema }),
]) satisfies z.ZodType<HostRetentionPreviewResponse>;

export const HostFindingRecordRequestSchema = z.strictObject({ finding: FindingSchema });
export const HostFindingRecordResponseSchema = z.union([
  z.strictObject({ recorded: z.literal(true), finding: FindingSchema }),
  z.strictObject({ recorded: z.literal(false), error: HostEvidenceToolErrorSchema }),
]) satisfies z.ZodType<HostFindingRecordResponse>;

export const HostBriefRecordRequestSchema = z.strictObject({ brief: DecisionBriefSchema });
export const HostBriefRecordResponseSchema = z.union([
  z.strictObject({ recorded: z.literal(true), brief: DecisionBriefSchema }),
  z.strictObject({ recorded: z.literal(false), error: HostEvidenceToolErrorSchema }),
]) satisfies z.ZodType<HostBriefRecordResponse>;

export const HostPlanRecordRequestSchema = z.strictObject({ plan: InvestigationPlanSchema });
export const HostPlanRecordResponseSchema = z.union([
  z.strictObject({ recorded: z.literal(true), plan: InvestigationPlanSchema }),
  z.strictObject({ recorded: z.literal(false), error: HostEvidenceToolErrorSchema }),
]) satisfies z.ZodType<HostPlanRecordResponse>;

export const HostPlanCheckRequestSchema = z.strictObject({
  taskId: z.string().trim().min(1).max(256),
  toolName: z.string().trim().min(1).max(256),
  input: z.unknown(),
  target: ToolTargetSchema,
});
export const HostPlanCheckResponseSchema = z.discriminatedUnion("status", [
  z.strictObject({
    status: z.literal("allowed"),
    planId: z.string().min(1),
    stepId: z.string().min(1),
    stepKind: z.enum(PLAN_STEP_KINDS),
    evidenceIds: z.array(z.string().min(1)),
    requiresApproval: z.boolean(),
  }),
  z.strictObject({ status: z.literal("deviation"), deviation: InvestigationPlanDeviationSchema }),
  z.strictObject({ status: z.literal("failed"), error: HostEvidenceToolErrorSchema }),
]) satisfies z.ZodType<HostPlanCheckResponse>;

export const HostPlanStepResultRequestSchema = z.strictObject({
  taskId: z.string().trim().min(1).max(256),
  planId: z.string().trim().min(1).max(256),
  stepId: z.string().trim().min(1).max(256),
  status: z.enum(["success", "failed", "cancelled"]),
  retryable: z.boolean(),
  outputSummary: z.string().max(4_096).optional(),
});
export const HostPlanStepResultResponseSchema = z.union([
  z.strictObject({
    recorded: z.literal(true),
    plan: InvestigationPlanSchema,
    observation: z.enum(["running", "failed"]).optional(),
    sampleAccepted: z.boolean().optional(),
    nextSampleAt: z.string().trim().min(1).max(80).optional(),
  }),
  z.strictObject({ recorded: z.literal(false), error: HostEvidenceToolErrorSchema }),
]) satisfies z.ZodType<HostPlanStepResultResponse>;

export const HostArtifactRecordRequestSchema = z.strictObject({
  artifact: InvestigationArtifactSchema,
  planId: z.string().trim().min(1).max(256).optional(),
  planStepId: z.string().trim().min(1).max(256).optional(),
  evidenceIds: z.array(z.string().trim().min(1).max(256)).max(256).optional(),
});
export const HostArtifactRecordResponseSchema = z.union([
  z.strictObject({ recorded: z.literal(true), artifact: InvestigationArtifactSchema }),
  z.strictObject({ recorded: z.literal(false), error: HostEvidenceToolErrorSchema }),
]) satisfies z.ZodType<HostArtifactRecordResponse>;

export const HostToolCancelRequestSchema = z.strictObject({
  requestId: z.number().int().positive(),
});

export const HostToolCancelResponseSchema = z.strictObject({
  cancelled: z.boolean(),
}) satisfies z.ZodType<HostToolCancelResponse>;

export const HostContextRequestSchema = z.strictObject({
  kind: z.enum(HOST_CONTEXT_KINDS),
  id: z.string().min(1).max(160),
});

export const HostContextResponseSchema = z.discriminatedUnion("status", [
  z.strictObject({ status: z.literal("success"), data: z.unknown() }),
  z.strictObject({ status: z.literal("not_found") }),
  z.strictObject({ status: z.literal("failed"), error: ToolErrorSchema }),
]) satisfies z.ZodType<HostContextResponse>;

export const HOST_TOOL_EXECUTE_METHOD = HOST_METHODS.toolExecute;
export const HOST_CONTEXT_FETCH_METHOD = HOST_METHODS.contextFetch;

/* ── MCP catalog (ADR 0014) ────────────────────────────────────────────── */

/**
 * A description or an input schema comes from a third-party process. It is carried, not
 * trusted: `inputSchema` stays `unknown` on purpose so nothing downstream starts
 * validating model input against a document the model itself could influence.
 */
export const HostMcpCatalogToolSchema = z.strictObject({
  name: z.string().min(1).max(160),
  serverId: z.string().min(1).max(160),
  tool: z.string().min(1).max(160),
  remoteName: z.string().min(1).max(160).optional(),
  description: z.string().max(4096),
  inputSchema: z.unknown(),
});

export const HostMcpCatalogServerSchema = z.strictObject({
  serverId: z.string().min(1).max(160),
  segment: z.string().min(1).max(64),
  label: z.string(),
  tools: z.array(HostMcpCatalogToolSchema),
});

export const HostMcpCatalogFailureSchema = z.strictObject({
  serverId: z.string().min(1).max(160),
  code: z.enum(MCP_CATALOG_FAILURE_CODES),
  message: z.string(),
});

export const HostMcpCatalogResponseSchema = z.strictObject({
  servers: z.array(HostMcpCatalogServerSchema),
  failures: z.array(HostMcpCatalogFailureSchema),
}) satisfies z.ZodType<HostMcpCatalogResponse>;

/**
 * Mirrors `McpContentBlock` (`crates/core/src/mcp/descriptor.rs`). A `strictObject` per
 * variant on purpose: a server that invents a block type gets it back as `other` with the
 * raw value, so an unmodelled type can never arrive looking like a text block.
 */
export const HostMcpContentBlockSchema = z.union([
  z.strictObject({ type: z.literal("text"), text: z.string() }),
  z.strictObject({ type: z.literal("other"), value: z.unknown() }),
]);

export const HostMcpToolCallOutputSchema = z.strictObject({
  serverId: z.string().min(1).max(160),
  tool: z.string().min(1).max(160),
  isError: z.boolean(),
  text: z.string(),
  content: z.array(HostMcpContentBlockSchema),
  structuredContent: z.unknown().optional(),
});

export const HOST_MCP_CATALOG_METHOD = HOST_METHODS.mcpCatalog;
