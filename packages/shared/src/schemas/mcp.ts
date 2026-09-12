import { z } from "zod";

import { MCP_CATALOG_FAILURE_CODES } from "../types/host.js";
import type { McpServerConfig } from "../types/provider.js";
import type {
  McpServerDeleteResponse,
  McpServerListResponse,
  McpServerSaveInput,
  McpServerStatus,
  McpServerStopResponse,
  McpServerView,
  McpToolDescriptor,
} from "../types/mcp.js";

/**
 * `mcp_server_*` payloads, parsed at the IPC gate (`lib/ipc.ts`).
 *
 * These mirror `apps/desktop/src-tauri/src/commands/mcp.rs`. They are strict on the fields the UI
 * branches on and permissive exactly where the value is remote, untrusted text: a tool
 * description or an input schema from a third-party process must never be able to fail the parse
 * of an otherwise valid settings screen (a server that sends a huge or exotic schema would
 * otherwise blank the whole panel).
 */

const IdSchema = z.string().trim().min(1).max(160);
const LabelSchema = z.string().max(200);

/**
 * The **stored** row (`mcp_servers`), which is stricter than the save input on purpose:
 * `transport` is an enum here because a row can only hold what the Rust core accepted, while the
 * form sends a plain string and lets the core refuse `http` with a reason a user can read.
 */
export const McpServerConfigSchema = z.strictObject({
  id: IdSchema,
  label: LabelSchema,
  transport: z.enum(["stdio", "http"]),
  command: z.string().optional(),
  args: z.array(z.string()).optional(),
  url: z.string().optional(),
  enabled: z.boolean(),
  allowedTools: z.array(z.string()),
  trustLevel: z.enum(["reviewed", "unreviewed"]),
}) satisfies z.ZodType<McpServerConfig>;

export const McpExitRecordSchema = z.strictObject({
  code: z.number().int().nullable().optional(),
  signal: z.string().nullable().optional(),
  at: z.string(),
  reason: z.string(),
});

export const McpServerStatusSchema = z.strictObject({
  serverId: IdSchema,
  running: z.boolean(),
  pid: z.number().int().nullable().optional(),
  program: z.string().nullable().optional(),
  startedAt: z.string().nullable().optional(),
  protocolVersion: z.string().nullable().optional(),
  serverName: z.string().nullable().optional(),
  serverVersion: z.string().nullable().optional(),
  toolCount: z.number().int().nonnegative(),
  lastExit: McpExitRecordSchema.nullable().optional(),
  stderrTail: z.array(z.string()),
  diagnostics: z.array(z.string()),
}) satisfies z.ZodType<McpServerStatus>;

export const McpToolDescriptorSchema = z.strictObject({
  name: z.string(),
  remoteName: z.string().optional(),
  // Untrusted remote text: bounded so one hostile server cannot blow up the parse or the panel.
  description: z.string().max(8_192),
  inputSchema: z.unknown(),
  outputSchema: z.unknown().optional(),
}) satisfies z.ZodType<McpToolDescriptor>;

export const McpServerUnavailableSchema = z.strictObject({
  code: z.enum(MCP_CATALOG_FAILURE_CODES),
  message: z.string(),
});

export const McpServerViewSchema = z.strictObject({
  config: McpServerConfigSchema,
  status: McpServerStatusSchema,
  tools: z.array(McpToolDescriptorSchema),
  unavailable: McpServerUnavailableSchema.optional(),
}) satisfies z.ZodType<McpServerView>;

export const McpServerListResponseSchema = z.strictObject({
  servers: z.array(McpServerViewSchema),
}) satisfies z.ZodType<McpServerListResponse>;

/**
 * The form's payload.
 *
 * `transport` is a plain string, not an enum: the refusal of `http` belongs to the Rust core
 * (`McpStdioConfig::from_server_config`), and turning it into a schema error here would give the
 * user a validation message instead of the reason. `id` and `label` are *not* trimmed here
 * either — the backend trims and decides, and the UI shows what it decided.
 */
export const McpServerSaveInputSchema = z.strictObject({
  id: z.string().max(160),
  label: z.string().max(200),
  transport: z.string().max(32),
  command: z.string().max(1_024).optional(),
  args: z.array(z.string().max(1_024)).max(64).optional(),
  url: z.string().max(2_048).optional(),
  enabled: z.boolean(),
}) satisfies z.ZodType<McpServerSaveInput>;

export const McpServerDeleteResponseSchema = z.strictObject({
  deleted: z.boolean(),
  stopped: z.boolean(),
}) satisfies z.ZodType<McpServerDeleteResponse>;

export const McpShutdownOutcomeSchema = z.strictObject({
  wasRunning: z.boolean(),
  killed: z.boolean(),
  unreaped: z.boolean(),
});

export const McpServerStopResponseSchema = z.strictObject({
  server: McpServerViewSchema,
  shutdown: McpShutdownOutcomeSchema.optional(),
}) satisfies z.ZodType<McpServerStopResponse>;

export { IdSchema as McpServerIdSchema, LabelSchema as McpServerLabelSchema };
