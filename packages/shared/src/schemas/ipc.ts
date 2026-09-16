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
import {
  agentPromptCarriesContent,
  AgentPermissionModeSchema,
  AgentPromptPartsSchema,
  AgentRunModeSchema,
  ApprovalResponseSchema,
  EMPTY_PROMPT_MESSAGE,
} from "./permission.js";
import { AGENT_EVENT_MEMBER_SCHEMAS, AgentRunResultSchema } from "./agent.js";
import {
  ChatMessageAppendResponseSchema,
  ChatMessageRoleSchema,
  ChatSessionDetailResponseSchema,
  ChatSessionListResponseSchema,
  ChatSessionResponseSchema,
} from "./chat.js";
import {
  ProviderConfigSchema,
  ProviderDeleteResponseSchema,
  ProviderModelOptionSchema,
  ProviderSaveInputSchema,
} from "./provider.js";
import { SERVER_ID_SCHEMA } from "./server.js";
import {
  McpServerDeleteResponseSchema,
  McpServerIdSchema,
  McpServerListResponseSchema,
  McpOAuthCancelResponseSchema,
  McpOAuthConnectResponseSchema,
  McpServerReviewInputSchema,
  McpServerSaveInputSchema,
  McpServerStopResponseSchema,
  McpServerViewSchema,
} from "./mcp.js";
import { HOST_KEY_COMPARISONS } from "../types/host-key.js";
import { REMOTE_FILE_TYPES } from "../types/file.js";
import { RestartRecordSchema } from "./lifecycle.js";
import { NetworkProxySaveInputSchema, NetworkProxyViewSchema } from "./network.js";

/** "This command takes no params / returns no payload" <> `Record<string, never>`. */
export const EMPTY_PAYLOAD = z.record(z.string(), z.never());

/**
 * Opaque `srv_` ids are the only admissible server references on the wire.
 *
 * 这里**不再重写一遍正则**，只是给 `SERVER_ID_SCHEMA` 起一个 IPC 语境下的别名。
 * 原先它是一条独立的等价定义，于是「全项目唯一一处 id 规则」这句话就不成立了：
 * 改了一条忘了另一条时，`server_update` 接受的 id 与 `server_list` 期望的 id
 * 会悄悄分叉，而这正是 `consistency.ts` 那套检查想拦住的事。
 */
export const IpcServerIdSchema = SERVER_ID_SCHEMA;

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
  /**
   * Optional rather than nullable: the field is *omitted* when there is nothing to
   * report (`skip_serializing_if` on the Rust side), so the status of an idle supervisor
   * stays byte-identical to the fixture that predates automatic recovery.
   */
  restart: RestartRecordSchema.optional(),
});

export const AgentLogsResponseSchema = z.strictObject({
  lines: z.array(z.string()),
  capacity: z.number().int().positive(),
});

/**
 * Host-key trust (ADR 0012).
 *
 * A fingerprint's canonical form is `SHA256:` + 43 base64 characters (unpadded — that is
 * what `ssh-key` prints and what the `known_hosts` file stores, ADR 0012 point 6), so the
 * gate checks the shape rather than only a length. This matters most on `trust`: that
 * fingerprint is what gets written to disk, and a gate that accepted "not pinned (first
 * connect must be explicitly trusted)" would let a placeholder string become a pin.
 */
export const IpcFingerprintSchema = z
  .string()
  .trim()
  .regex(/^SHA256:[A-Za-z0-9+/]{43}$/, "fingerprint must be SHA256:<43 base64 chars, unpadded>");

/**
 * `pinnedFingerprint` is optional, never nullable: Rust omits the field when there is no
 * pin (`skip_serializing_if`), and a `null` here would mean both sides disagree about
 * what "no pin" looks like.
 */
export const ServerHostKeyStatusResponseSchema = z.strictObject({
  host: z.string().trim().min(1).max(256),
  port: IpcPortSchema,
  pinned: z.boolean(),
  pinnedFingerprint: IpcFingerprintSchema.optional(),
});

export const ServerHostKeyProbeResponseSchema = z.strictObject({
  host: z.string().trim().min(1).max(256),
  port: IpcPortSchema,
  probeTicket: z.string().regex(/^probe_[a-f0-9]{64}$/, "probe ticket must be opaque and complete"),
  /** What the server presented. Not "verified" — see `types/host-key.ts`. */
  presentedFingerprint: IpcFingerprintSchema,
  comparison: z.enum(HOST_KEY_COMPARISONS),
  pinnedFingerprint: IpcFingerprintSchema.optional(),
});

export const ServerHostKeyTrustResponseSchema = z.strictObject({
  host: z.string().trim().min(1).max(256),
  port: IpcPortSchema,
  fingerprint: IpcFingerprintSchema,
  alreadyPinned: z.boolean(),
});

export const ServerHostKeyForgetResponseSchema = z.strictObject({
  host: z.string().trim().min(1).max(256),
  port: IpcPortSchema,
  forgotten: z.boolean(),
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
    server_auth_respond: {
      params: z.strictObject({
        authId: z.string().trim().min(1).max(256),
        responses: z.array(z.string().max(4_096)).max(16),
      }),
      response: z.strictObject({ accepted: z.boolean() }),
    },
    server_auth_cancel: {
      params: z.strictObject({ authId: z.string().trim().min(1).max(256) }),
      response: z.strictObject({ accepted: z.boolean() }),
    },
  server_host_key_status: {
    params: z.strictObject({ serverId: IpcServerIdSchema }),
    response: ServerHostKeyStatusResponseSchema,
  },
  server_host_key_probe: {
    params: z.strictObject({ serverId: IpcServerIdSchema }),
    response: ServerHostKeyProbeResponseSchema,
  },
  server_host_key_trust: {
    params: z.strictObject({
      serverId: IpcServerIdSchema,
      probeTicket: z.string().regex(/^probe_[a-f0-9]{64}$/, "probe ticket must be opaque and complete"),
      fingerprint: IpcFingerprintSchema,
    }),
    response: ServerHostKeyTrustResponseSchema,
  },
  server_host_key_forget: {
    params: z.strictObject({ serverId: IpcServerIdSchema }),
    response: ServerHostKeyForgetResponseSchema,
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
    response: z.strictObject({ path: z.string().min(1), entries: z.array(z.strictObject({ name: z.string(), path: z.string(), type: z.enum(REMOTE_FILE_TYPES), size: z.number().nonnegative() })) }),
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
    params: z
      .strictObject({
        runId: z.string().trim().min(1).max(256).optional(),
        sessionId: z.string().trim().min(1).max(256),
        prompt: z.string().max(100_000),
        messageId: z.string().trim().min(1).max(256).optional(),
        parts: AgentPromptPartsSchema.optional(),
        delivery: z.enum(["async", "sync"]).optional(),
        resume: z.boolean().optional(),
        providerId: z.string().trim().min(1).max(256).optional(),
        model: z.string().trim().min(1).max(256).optional(),
        workspaceId: z.string().trim().min(1).max(256).optional(),
        focusServerId: IpcServerIdSchema.optional(),
        permissionMode: AgentPermissionModeSchema.optional(),
        /** Bounds what the run may accomplish; enforced by the permission engine. */
        mode: AgentRunModeSchema.optional(),
        /** Forwarded verbatim to the sidecar, which owns what a policy id means. */
        policyId: z.string().trim().min(1).max(256).optional(),
      })
      /*
       * The rule itself lives next to the part vocabulary (`agentPromptCarriesContent`), not
       * here: this gate and the run schema ask the same question, and when the answer was
       * written out twice, adding the audio kind left one copy refusing an audio-only prompt.
       */
      .refine(agentPromptCarriesContent, {
        message: EMPTY_PROMPT_MESSAGE,
        path: ["prompt"],
      }),
    /**
     * `result` is present only for `delivery: "sync"`; it is the run's outcome, and the
     * same object the `agent.completed` notification carries. Validated here as well as
     * there, because it reaches the UI through a response frame rather than the event
     * channel and therefore does not pass the event gate.
     */
    response: z.strictObject({
      runId: z.string().min(1),
      started: z.boolean(),
      duplicate: z.boolean().optional(),
      resumed: z.boolean().optional(),
      result: AgentRunResultSchema.optional(),
    }),
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
      // Bounded like every other page: the host rejects an offset past this rather than
      // running an unbounded scan.
      offset: z.number().int().min(0).max(10_000).optional(),
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
    response: ChatSessionResponseSchema,
  },
  chat_message_append: {
    params: z
      .strictObject({
        sessionId: z.string().trim().min(1).max(256),
        messageId: z.string().trim().min(1).max(256).optional(),
        role: ChatMessageRoleSchema,
        content: z.string().max(100_000),
        parts: AgentPromptPartsSchema.optional(),
        traceId: z.string().trim().min(1).max(256).optional(),
        createdAt: z.string().min(1).max(80).optional(),
      })
      .refine(
        (message) => message.content.trim().length > 0 || Boolean(message.parts?.length),
        { message: "content must contain text or prompt parts", path: ["content"] },
      ),
    response: ChatMessageAppendResponseSchema,
  },
  chat_session_archive: {
    params: z.strictObject({ sessionId: z.string().trim().min(1).max(256), archived: z.boolean() }),
    response: ChatSessionResponseSchema,
  },
  chat_session_rename: {
    params: z.strictObject({
      sessionId: z.string().trim().min(1).max(256),
      title: z.string().trim().min(1).max(200),
    }),
    response: ChatSessionResponseSchema,
  },
  chat_session_delete: {
    params: z.strictObject({ sessionId: z.string().trim().min(1).max(256) }),
    response: z.strictObject({ deleted: z.boolean() }),
  },
  provider_list: { params: EMPTY_PAYLOAD, response: z.strictObject({ providers: z.array(ProviderConfigSchema) }) },
  provider_save: {
    params: ProviderSaveInputSchema,
    response: z.strictObject({ provider: ProviderConfigSchema }),
  },
  provider_activate: {
    params: z.strictObject({ providerId: z.string().min(1) }),
    response: z.strictObject({ provider: ProviderConfigSchema }),
  },
  provider_delete: {
    params: z.strictObject({ providerId: z.string().min(1) }),
    response: ProviderDeleteResponseSchema,
  },
  provider_models: {
    params: z.strictObject({ providerId: z.string().min(1) }),
    response: z.strictObject({ models: z.array(ProviderModelOptionSchema) }),
  },
  mcp_server_list: { params: z.strictObject({}), response: McpServerListResponseSchema },
  mcp_server_save: {
    params: z.strictObject({ input: McpServerSaveInputSchema }),
    response: McpServerViewSchema,
  },
  mcp_server_delete: {
    params: z.strictObject({ serverId: McpServerIdSchema }),
    response: McpServerDeleteResponseSchema,
  },
  mcp_server_start: {
    params: z.strictObject({ serverId: McpServerIdSchema }),
    response: McpServerViewSchema,
  },
  mcp_server_stop: {
    params: z.strictObject({ serverId: McpServerIdSchema }),
    response: McpServerStopResponseSchema,
  },
  mcp_server_review: {
    params: McpServerReviewInputSchema,
    response: McpServerViewSchema,
  },
  mcp_oauth_connect: {
    params: z.strictObject({ serverId: McpServerIdSchema }),
    response: McpOAuthConnectResponseSchema,
  },
  mcp_oauth_cancel: {
    params: z.strictObject({ serverId: McpServerIdSchema }),
    response: McpOAuthCancelResponseSchema,
  },
  network_proxy_get: {
    params: EMPTY_PAYLOAD,
    response: NetworkProxyViewSchema,
  },
  network_proxy_save: {
    params: z.strictObject({ input: NetworkProxySaveInputSchema }),
    response: NetworkProxyViewSchema,
  },
  provider_test: {
    params: z.strictObject({ providerId: z.string().min(1) }),
    response: z.strictObject({ ok: z.literal(true) }),
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
 *
 * Each `agent.*` channel is gated by **its own member schema**, not by the whole
 * `AgentStreamEventSchema` union. The union was one level too coarse: it could only
 * ask "is this some valid agent event", so `DesktopEventPayload<"agent.completed">`
 * was the entire union and `useAgentRun` had to cast nine times to reach the member it
 * already knew it was handling. A cast there is where a payload that passed the loose
 * gate reaches a handler as a shape nothing validated.
 *
 * Narrowing is safe because the channel name *is* the payload's discriminator: Rust
 * reads `params.type` and emits on `tauri_event_name(event_type)`
 * (`apps/desktop/src-tauri/src/commands/mod.rs:470,504`). Nothing the transport can
 * deliver is rejected by this; it stops accepting what the transport cannot produce.
 */
export const EVENT_SCHEMAS = {
  "agent.started": AGENT_EVENT_MEMBER_SCHEMAS["agent.started"],
  "agent.thinking": AGENT_EVENT_MEMBER_SCHEMAS["agent.thinking"],
  "agent.text": AGENT_EVENT_MEMBER_SCHEMAS["agent.text"],
  "agent.usage": AGENT_EVENT_MEMBER_SCHEMAS["agent.usage"],
  "agent.tool_call": AGENT_EVENT_MEMBER_SCHEMAS["agent.tool_call"],
  "agent.tool_result": AGENT_EVENT_MEMBER_SCHEMAS["agent.tool_result"],
  "agent.waiting_approval": AGENT_EVENT_MEMBER_SCHEMAS["agent.waiting_approval"],
  "agent.approval_expired": AGENT_EVENT_MEMBER_SCHEMAS["agent.approval_expired"],
  "agent.completed": AGENT_EVENT_MEMBER_SCHEMAS["agent.completed"],
  "agent.failed": AGENT_EVENT_MEMBER_SCHEMAS["agent.failed"],
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
  "server.auth_challenge": z.strictObject({
    authId: z.string().trim().min(1).max(256),
    serverId: IpcServerIdSchema,
    username: z.string().trim().min(1).max(256),
    host: z.string().trim().min(1).max(4_096),
    name: z.string().max(256),
    instructions: z.string().max(4_096),
    prompts: z
      .array(
        z.strictObject({
          prompt: z.string().max(1_024),
          echo: z.boolean(),
        }),
      )
      .max(16),
    expiresAt: z.string().min(1).max(80),
  }),
  /**
   * The device-code prompt. `userCode` and the verification URLs are remote text: the
   * gate bounds their length and nothing else, because rejecting a server's spelling
   * would leave the user without the one string they need to type.
   */
  "mcp.oauth_device_code": z.strictObject({
    serverId: IpcServerIdSchema,
    userCode: z.string().min(1).max(256),
    verificationUri: z.string().min(1).max(2_048),
    verificationUriComplete: z.string().min(1).max(2_048).optional(),
    expiresAt: z.string().min(1).max(80),
  }),
  "activity.created": ActivitySchema,
} as const;
