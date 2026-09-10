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

import type { IpcCommandMap, IpcCommandName } from "../ipc/index.js";
import { AddServerInputSchema, ServerSchema, UpdateServerInputSchema, WorkspaceListResponseSchema } from "./server.js";
import { ActivitySchema, ToolExecutionListResponseSchema } from "./activity.js";
import { ServerSnapshotSchema } from "./collector.js";
import { ServerServicesResponseSchema } from "./service.js";
import { ServerLogsResponseSchema } from "./log.js";
import { AgentPermissionModeSchema, AgentRunModeSchema, ApprovalResponseSchema } from "./permission.js";
import { AgentStreamEventSchema } from "./agent.js";
import {
  ChatMessageAppendResponseSchema,
  ChatMessageRoleSchema,
  ChatSessionCreateResponseSchema,
  ChatSessionDetailResponseSchema,
  ChatSessionListResponseSchema,
} from "./chat.js";
import {
  ProviderConfigSchema,
  ProviderModelOptionSchema,
  ProviderSaveInputSchema,
} from "./provider.js";

/** "This command takes no params / returns no payload" <> `Record<string, never>`. */
export const EMPTY_PAYLOAD = z.record(z.string(), z.never());

/** Opaque `srv_` ids are the only admissible server references on the wire. */
export const IpcServerIdSchema = z.string().trim().min(1).max(256).regex(/^srv_[a-z0-9]+$/, "server id must be an opaque srv_ id");

const IpcTerminalSessionIdSchema = z.string().trim().min(1).max(256);
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

type IpcCommandSchemaMap = {
  [C in IpcCommandName]: {
    params: z.ZodType<IpcCommandMap[C]["params"]>;
    response: z.ZodType<IpcCommandMap[C]["response"]>;
  };
};

/**
 * Every command of `IpcCommandMap`, each with a params + response gate. The
 * `satisfies` mapped type over `IpcCommandName` forces completeness; individual
 * assignability against the map's response types is asserted in `consistency.ts`.
 */
export const IPC_SCHEMAS = {
  core_ping: { params: EMPTY_PAYLOAD, response: CorePingResponseSchema },
  server_list: { params: EMPTY_PAYLOAD, response: ServerListResponseSchema },
  workspace_list: { params: EMPTY_PAYLOAD, response: WorkspaceListResponseSchema },
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
  server_services: {
    params: z.strictObject({ serverId: IpcServerIdSchema }),
    response: ServerServicesResponseSchema,
  },
  server_logs: {
    params: z.strictObject({ serverId: IpcServerIdSchema }),
    response: ServerLogsResponseSchema,
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
  tool_execution_list: {
    params: z.strictObject({
      traceId: z.string().min(1).optional(),
      serverId: IpcServerIdSchema.optional(),
      limit: z.number().int().min(1).max(100).optional(),
    }),
    response: ToolExecutionListResponseSchema,
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
    params: z.strictObject({ terminalSessionId: IpcTerminalSessionIdSchema, data: z.string().max(64 * 1024) }),
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
      runId: z.string().trim().min(1).max(256).optional(),
      sessionId: z.string().trim().min(1).max(256),
      prompt: z.string().trim().min(1).max(100_000),
      messageId: z.string().trim().min(1).max(256).optional(),
      parts: z.array(z.strictObject({ type: z.literal("text"), text: z.string().trim().min(1).max(100_000) })).min(1).max(128).optional(),
      delivery: z.enum(["async", "sync"]).optional(),
      resume: z.boolean().optional(),
      providerId: z.string().trim().min(1).max(256).optional(),
      model: z.string().trim().min(1).max(256).optional(),
      workspaceId: z.string().trim().min(1).max(256).optional(),
      focusServerId: IpcServerIdSchema.optional(),
      permissionMode: AgentPermissionModeSchema.optional(),
      /** Bounds what the run may accomplish; enforced by the permission engine. */
      mode: AgentRunModeSchema.optional(),
    }),
    response: z.strictObject({ runId: z.string().min(1) }),
  },
  agent_run_stop: {
    params: z.strictObject({ runId: z.string().trim().min(1).max(256) }),
    response: z.strictObject({ stopped: z.boolean() }),
  },
  agent_approval_respond: {
    params: ApprovalResponseSchema,
    response: z.strictObject({ accepted: z.boolean() }),
  },
  chat_session_list: {
    params: z.strictObject({
      query: z.string().trim().max(200).optional(),
      archived: z.boolean().optional(),
      limit: z.number().int().min(1).max(100).optional(),
    }),
    response: ChatSessionListResponseSchema,
  },
  chat_session_get: {
    params: z.strictObject({ sessionId: z.string().trim().min(1).max(256) }),
    response: ChatSessionDetailResponseSchema,
  },
  chat_session_create: {
    params: z.strictObject({
      sessionId: z.string().trim().min(1).max(256).optional(),
      workspaceId: z.string().trim().min(1).max(256).optional(),
      serverId: IpcServerIdSchema.optional(),
      title: z.string().trim().min(1).max(200),
    }),
    response: ChatSessionCreateResponseSchema,
  },
  chat_message_append: {
    params: z.strictObject({
      sessionId: z.string().trim().min(1).max(256),
      messageId: z.string().trim().min(1).max(256).optional(),
      role: ChatMessageRoleSchema,
      content: z.string().trim().min(1).max(100_000),
      traceId: z.string().trim().min(1).max(256).optional(),
      createdAt: z.string().min(1).max(80).optional(),
    }),
    response: ChatMessageAppendResponseSchema,
  },
  chat_session_archive: {
    params: z.strictObject({ sessionId: z.string().trim().min(1).max(256), archived: z.boolean() }),
    response: ChatSessionCreateResponseSchema,
  },
  chat_session_delete: {
    params: z.strictObject({ sessionId: z.string().trim().min(1).max(256) }),
    response: z.strictObject({ deleted: z.boolean() }),
  },
  provider_list: { params: EMPTY_PAYLOAD, response: z.strictObject({ providers: z.array(ProviderConfigSchema) }) },
  provider_save_openai: {
    params: ProviderSaveInputSchema,
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
} satisfies IpcCommandSchemaMap;

/**
 * Runtime gate for the *event* half of the IPC contract.
 *
 * `IPC_SCHEMAS` above gates commands; events had no equivalent, so their payloads
 * crossed into the UI by cast. That is the same untrusted-boundary problem the
 * command gate exists to solve — a `terminal.data` payload carries raw bytes read
 * off a remote host — and one call site wrote them straight into xterm without
 * checking the shape at all.
 *
 * Events are notifications, not requests: a payload that fails its schema can only
 * be dropped, never re-asked. So the schema is here to make a drifted Rust payload
 * a *visible* dropped event rather than a silent runtime failure deep in a writer.
 */
export const EVENT_SCHEMAS = {
  // Agent stream events all share one discriminated union, so the UI can hand a
  // payload to the same reducer regardless of which event it arrived on.
  "agent.started": AgentStreamEventSchema,
  "agent.thinking": AgentStreamEventSchema,
  "agent.tool_call": AgentStreamEventSchema,
  "agent.tool_result": AgentStreamEventSchema,
  "agent.waiting_approval": AgentStreamEventSchema,
  "agent.approval_expired": AgentStreamEventSchema,
  "agent.completed": AgentStreamEventSchema,
  "agent.failed": AgentStreamEventSchema,
  "terminal.data": z.strictObject({
    terminalSessionId: IpcTerminalSessionIdSchema,
    // The bound is deliberately loose. This gate exists to catch *shape* drift —
    // a renamed field, a wrong type — not to police size: terminal output is a
    // high-volume stream read off a remote host, and a tight cap here would
    // silently drop legitimate chunks, which is worse than a large payload. The
    // host already rejects any streamed payload over 1,000,000 bytes, so this
    // matches an enforced bound rather than inventing a new one.
    data: z.string().max(1_000_000),
  }),
  "terminal.closed": z.strictObject({
    terminalSessionId: IpcTerminalSessionIdSchema,
    // Rust sends `Option<u32>`, so a negative or fractional code is drift.
    exitCode: z.number().int().nonnegative().nullable(),
  }),
  "activity.created": ActivitySchema,
} as const;
