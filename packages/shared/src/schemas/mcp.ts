import { z } from "zod";

import { MCP_CATALOG_FAILURE_CODES } from "../types/host.js";
import type {
  McpHttpAuthHeaderConfig,
  McpOAuthConfig,
  McpServerConfig,
} from "../types/provider.js";
import type {
  McpOAuthCancelResponse,
  McpHttpAuthHeaderInput,
  McpOAuthConnectResponse,
  McpOAuthInput,
  McpServerDeleteResponse,
  McpServerListResponse,
  McpServerReviewInput,
  McpServerSaveInput,
  McpServerStatus,
  McpServerStopResponse,
  McpServerView,
  McpToolDescriptor,
} from "../types/mcp.js";
import { RestartRecordSchema } from "./lifecycle.js";

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

const McpHttpAuthHeaderConfigSchema = z.strictObject({
  name: z.string().max(128),
  credentialRef: z.string().min(1).max(512),
}) satisfies z.ZodType<McpHttpAuthHeaderConfig>;

const McpHttpAuthHeaderInputSchema = z.strictObject({
  name: z.string().max(128),
  secret: z.string().min(1).max(8_192).optional(),
}) satisfies z.ZodType<McpHttpAuthHeaderInput>;

/**
 * Client authentication methods. The same closed set as Rust's `McpOAuthClientAuth`: an
 * unknown value would mean sending credentials a way the host cannot perform.
 */
const McpOAuthClientAuthSchema = z.enum(["none", "client_secret_post", "client_secret_basic"]);

const McpOAuthConfigSchema = z.strictObject({
  issuer: z.string().max(2_048),
  clientId: z.string().max(512),
  flow: z.enum(["authorization_code", "device_code"]),
  clientAuth: McpOAuthClientAuthSchema,
  clientSecretRef: z.string().min(1).max(512).optional(),
  // Always present on the way out, like `flow`: the host defaults rows written before the
  // setting existed to `false`, so a missing field here would be a contract that has
  // drifted from what Rust actually sends.
  dpop: z.boolean(),
  dpopKeyRef: z.string().min(1).max(512).optional(),
  scopes: z.array(z.string().min(1).max(128)).max(32),
  tokenEndpoint: z.string().min(1).max(2_048).optional(),
  credentialRef: z.string().min(1).max(512).optional(),
}) satisfies z.ZodType<McpOAuthConfig>;

const McpOAuthInputSchema = z.strictObject({
  issuer: z.string().max(2_048),
  clientId: z.string().max(512),
  // Optional on the way in and always present on the way out: a client that omits it is
  // asking for the browser redirect, which is what rows written before this field mean.
  flow: z.enum(["authorization_code", "device_code"]).optional(),
  clientAuth: McpOAuthClientAuthSchema.optional(),
  // Write-only. Bounded like an HTTP auth secret, and never echoed back: the stored shape
  // next to it carries only `clientSecretRef`.
  clientSecret: z.string().min(1).max(8_192).optional(),
  dpop: z.boolean().optional(),
  scopes: z.array(z.string().min(1).max(128)).max(32),
}) satisfies z.ZodType<McpOAuthInput>;

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
  httpAuthHeaders: z.array(McpHttpAuthHeaderConfigSchema).max(16),
  oauth: McpOAuthConfigSchema.optional(),
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
  restart: RestartRecordSchema.optional(),
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
  httpAuthHeaders: z.array(McpHttpAuthHeaderInputSchema).max(16).optional(),
  oauth: McpOAuthInputSchema.optional(),
  enabled: z.boolean(),
}) satisfies z.ZodType<McpServerSaveInput>;

export const McpOAuthConnectResponseSchema = z.strictObject({
  serverId: IdSchema,
  issuer: z.string().min(1).max(2_048),
  scopes: z.array(z.string().min(1).max(128)).max(32),
  tokenEndpoint: z.string().min(1).max(2_048),
}) satisfies z.ZodType<McpOAuthConnectResponse>;

export const McpOAuthCancelResponseSchema = z.strictObject({
  accepted: z.boolean(),
}) satisfies z.ZodType<McpOAuthCancelResponse>;

export const McpServerReviewInputSchema = z.strictObject({
  serverId: IdSchema,
  allowedTools: z.array(z.string().trim().min(1).max(256)).max(512),
  trustLevel: z.enum(["reviewed", "unreviewed"]),
}) satisfies z.ZodType<McpServerReviewInput>;

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
