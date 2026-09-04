/**
 * Per-command runtime gates for the Tauri IPC contract (see `ipc/index.ts`).
 *
 * `IpcCommandMap` is the compile-time contract; this module is the runtime gate.
 * The map is keyed over `IpcCommandName` and exhaustively `satisfies` that union, so
 * adding a command to the map without a schema here fails compilation.
 *
 * Response schemas are strict: an extra field from Rust is a serde drift and must
 * fail the parse, not be silently stripped. Params are the same — the UI is our own
 * code, so a wrong param shape is a bug the gate should catch.
 */

import { z } from "zod";

import type { IpcCommandName } from "../ipc/index.js";
import { AddServerInputSchema, ServerSchema, UpdateServerInputSchema } from "./server.js";
import { ActivitySchema } from "./activity.js";
import { ServerSnapshotSchema } from "./collector.js";
import { ApprovalResponseSchema } from "./permission.js";
import {
  CcSwitchProviderCandidateSchema,
  ProviderConfigSchema,
  ProviderModelOptionSchema,
  ProviderSaveInputSchema,
} from "./provider.js";

/** "This command takes no params / returns no payload" <> `Record<string, never>`. */
export const EMPTY_PAYLOAD = z.record(z.string(), z.never());

/** Opaque `srv_` ids are the only admissible server references on the wire. */
export const IpcServerIdSchema = z.string().regex(/^srv_[a-z0-9]+$/, "server id must be an opaque srv_ id");

const IpcTerminalSessionIdSchema = z.string().min(1);
const IpcPortSchema = z.number().int().min(1).max(65535);

export const CorePingResponseSchema = z.strictObject({
  version: z.string().min(1),
  os: z.string().min(1),
});

export const ServerListResponseSchema = z.strictObject({ servers: z.array(ServerSchema) });
export const ServerAddResponseSchema = z.strictObject({ server: ServerSchema });
export const ServerConnectResponseSchema = z.strictObject({ status: z.literal("connected") });
export const ServerSnapshotResponseSchema = z.strictObject({ snapshot: ServerSnapshotSchema });
export const TerminalOpenResponseSchema = z.strictObject({ terminalSessionId: IpcTerminalSessionIdSchema });

/** Sidecar lifecycle, `agent_*` commands (fields mirror the Rust supervisor state). */

export const SidecarExitSchema = z.strictObject({
  code: z.number().int().nullable(),
  signal: z.string().nullable(),
  at: z.string().min(1),
});

export const AgentSpawnResponseSchema = z.strictObject({
  pid: z.number().int().positive(),
  protocolVersion: z.string().min(1),
  agentVersion: z.string().min(1),
  entry: z.string().min(1),
  toolCount: z.number().int().nonnegative(),
  alreadyRunning: z.boolean(),
});

export const AgentStatusSchema = z.strictObject({
  running: z.boolean(),
  pid: z.number().int().positive().nullable(),
  protocolVersion: z.string().min(1).nullable(),
  agentVersion: z.string().min(1).nullable(),
  toolCount: z.number().int().nonnegative().nullable(),
  entry: z.string().min(1).nullable(),
  startedAt: z.string().min(1).nullable(),
  lastExit: SidecarExitSchema.nullable(),
});

export const AgentLogsResponseSchema = z.strictObject({
  lines: z.array(z.string()),
  capacity: z.number().int().positive(),
});

interface IpcCommandSchema {
  params: z.ZodType;
  response: z.ZodType;
}

/**
 * Every command of `IpcCommandMap`, each with a params + response gate. The
 * `satisfies` mapped type over `IpcCommandName` forces completeness; individual
 * assignability against the map's response types is asserted in `consistency.ts`.
 */
export const IPC_SCHEMAS = {
  core_ping: { params: EMPTY_PAYLOAD, response: CorePingResponseSchema },
  server_list: { params: EMPTY_PAYLOAD, response: ServerListResponseSchema },
  server_add: { params: AddServerInputSchema, response: ServerAddResponseSchema },
  server_update: { params: UpdateServerInputSchema, response: ServerAddResponseSchema },
  server_delete: {
    params: z.strictObject({ serverId: IpcServerIdSchema }),
    response: z.strictObject({ deleted: z.boolean() }),
  },
  server_connect: {
    params: z.strictObject({ serverId: IpcServerIdSchema }),
    response: ServerConnectResponseSchema,
  },
  server_disconnect: {
    params: z.strictObject({ serverId: IpcServerIdSchema }),
    response: EMPTY_PAYLOAD,
  },
  server_snapshot: {
    params: z.strictObject({ serverId: IpcServerIdSchema }),
    response: ServerSnapshotResponseSchema,
  },
  remote_file_list: {
    params: z.strictObject({ serverId: IpcServerIdSchema, path: z.string().min(1) }),
    response: z.strictObject({ path: z.string().min(1), entries: z.array(z.strictObject({ name: z.string(), path: z.string(), type: z.enum(["file", "directory", "symlink", "other"]), size: z.number().nonnegative() })) }),
  },
  remote_file_read: {
    params: z.strictObject({ serverId: IpcServerIdSchema, path: z.string().min(1) }),
    response: z.strictObject({ path: z.string().min(1), content: z.string(), truncated: z.boolean() }),
  },
  activity_list: {
    params: z.strictObject({
      serverId: IpcServerIdSchema.optional(),
      limit: z.number().int().min(1).max(100).optional(),
    }),
    response: z.strictObject({ activities: z.array(ActivitySchema) }),
  },
  terminal_open: {
    params: z.strictObject({
      serverId: IpcServerIdSchema,
      cols: IpcPortSchema,
      rows: IpcPortSchema,
    }),
    response: TerminalOpenResponseSchema,
  },
  terminal_write: {
    params: z.strictObject({ terminalSessionId: IpcTerminalSessionIdSchema, data: z.string() }),
    response: EMPTY_PAYLOAD,
  },
  terminal_resize: {
    params: z.strictObject({
      terminalSessionId: IpcTerminalSessionIdSchema,
      cols: IpcPortSchema,
      rows: IpcPortSchema,
    }),
    response: EMPTY_PAYLOAD,
  },
  terminal_close: {
    params: z.strictObject({ terminalSessionId: IpcTerminalSessionIdSchema }),
    response: EMPTY_PAYLOAD,
  },
  agent_spawn: { params: EMPTY_PAYLOAD, response: AgentSpawnResponseSchema },
  agent_kill: { params: EMPTY_PAYLOAD, response: z.strictObject({ killed: z.boolean() }) },
  agent_status: { params: EMPTY_PAYLOAD, response: AgentStatusSchema },
  agent_logs: { params: EMPTY_PAYLOAD, response: AgentLogsResponseSchema },
  agent_run_start: {
    params: z.strictObject({
      sessionId: z.string().min(1),
      prompt: z.string().min(1),
      providerId: z.string().min(1).optional(),
      model: z.string().min(1).optional(),
    }),
    response: z.strictObject({ runId: z.string().min(1) }),
  },
  agent_run_stop: {
    params: z.strictObject({ runId: z.string().min(1) }),
    response: z.strictObject({ stopped: z.boolean() }),
  },
  agent_approval_respond: {
    params: ApprovalResponseSchema,
    response: z.strictObject({ accepted: z.boolean() }),
  },
  provider_list: { params: EMPTY_PAYLOAD, response: z.strictObject({ providers: z.array(ProviderConfigSchema) }) },
  provider_save_openai: {
    params: ProviderSaveInputSchema,
    response: z.strictObject({ provider: ProviderConfigSchema }),
  },
  provider_import_ccswitch: {
    params: EMPTY_PAYLOAD,
    response: z.strictObject({ providers: z.array(CcSwitchProviderCandidateSchema) }),
  },
  provider_import_ccswitch_apply: {
    params: z.strictObject({ ccSwitchProviderId: z.string().min(1) }),
    response: z.strictObject({ provider: ProviderConfigSchema }),
  },
  provider_import_codex: {
    params: EMPTY_PAYLOAD,
    response: z.strictObject({ providers: z.array(CcSwitchProviderCandidateSchema) }),
  },
  provider_import_codex_apply: {
    params: z.strictObject({ codexProviderId: z.string().min(1), model: z.string().min(1).optional() }),
    response: z.strictObject({ provider: ProviderConfigSchema }),
  },
  provider_activate: {
    params: z.strictObject({ providerId: z.string().min(1) }),
    response: z.strictObject({ provider: ProviderConfigSchema }),
  },
  provider_models: {
    params: z.strictObject({ providerId: z.string().min(1) }),
    response: z.strictObject({ models: z.array(ProviderModelOptionSchema) }),
  },
} satisfies Record<IpcCommandName, IpcCommandSchema>;
