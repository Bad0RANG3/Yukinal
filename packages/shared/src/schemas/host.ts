import { z } from "zod";

import {
  HOST_CONTEXT_KINDS,
  HOST_METHODS,
  MCP_CATALOG_FAILURE_CODES,
  type HostToolCancelResponse,
  type HostContextResponse,
  type HostMcpCatalogResponse,
  type HostToolExecuteResponse,
} from "../types/host.js";
import { TOOL_ERROR_CODES } from "../types/tool.js";
import { ToolTargetSchema } from "./server.js";

export const HostToolExecuteRequestSchema = z.strictObject({
  callId: z.string().min(1),
  traceId: z.string().min(1),
  toolName: z.string().min(1),
  input: z.unknown(),
  target: ToolTargetSchema,
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
