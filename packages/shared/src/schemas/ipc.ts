/**
 * Per-command runtime gates for the desktop IPC contract (see `ipc/index.ts`).
 *
 * `IpcCommandMap` is the compile-time contract; this module is the runtime gate.
 * The map is keyed over `IpcCommandName` and exhaustively `satisfies` that union, so
 * adding a command to the map without a schema here fails compilation.
 *
 * Response strictness depends on **who authors the payload**, because that is what
 * decides who a new field can surprise:
 *
 * - A response the C++ host composes field by field (`agent_status`, `server_connect`,
 *   the host-key replies, ...) stays strict: an unknown key is a real disagreement
 *   between two files in this repository, and failing the parse is the correct alarm.
 * - A response the host hands back verbatim from the Python sidecar
 *   (`SIDECAR_FORWARDED_COMMANDS` below) tolerates unknown keys. The sidecar is a
 *   separately-versioned process whose answer this repository does not own, so an added
 *   field must not turn a successful call into a parse exception in the UI; the addition
 *   is reported as a diagnostic instead (`unknownResponseKeys` + the desktop gate).
 *   Tolerance is not passthrough: missing fields and wrong types are still refused.
 *
 * Params are strict either way — the UI is our own code, so a wrong param shape is a bug
 * the gate should catch.
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
  AgentReasoningEffortSchema,
  ApprovalResponseSchema,
  EMPTY_PROMPT_MESSAGE,
} from "./permission.js";
import { AgentCoordinationConfigSchema } from "./coordination.js";
import { AGENT_EVENT_MEMBER_SCHEMAS, AgentRunResultSchema, AgentTeamTaskSchema } from "./agent.js";
import { AgentParticipantSchema, AgentPersonaSchema, AgentMemorySchema, AgentMemoryListInputSchema, AgentMemorySaveInputSchema, AgentMemoryDeleteInputSchema } from "./agent-context.js";
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
import {
  InvestigationTaskCreateInputSchema,
  InvestigationTaskDetailResponseSchema,
  InvestigationTaskListInputSchema,
  InvestigationTaskListResponseSchema,
  InvestigationTaskRecoverInputSchema,
  InvestigationTaskResponseSchema,
  InvestigationTaskStartResponseSchema,
  InvestigationTaskStopInputSchema,
  InvestigationTaskStatusUpdateInputSchema,
  TaskBudgetSchema,
  InvestigationBriefResponseSchema,
  InvestigationBriefSelectInputSchema,
} from "./investigation.js";
import {
  InvestigationScheduleCreateInputSchema,
  InvestigationScheduleListResponseSchema,
  InvestigationScheduleRunListResponseSchema,
  InvestigationScheduleSchema,
  InvestigationScheduleTickInputSchema,
  InvestigationScheduleTickResponseSchema,
  InvestigationScheduleUpdateInputSchema,
} from "./schedule.js";
import {
  InvestigationRetentionPreviewInputSchema,
  InvestigationRetentionPreviewResponseSchema,
  InvestigationRetentionPruneInputSchema,
  InvestigationRetentionPruneResponseSchema,
} from "./retention.js";
import { INVESTIGATION_NOTIFICATION_POLICIES } from "../types/schedule.js";
import {
  LocalFileDropEventSchema,
  LocalPathHandleSchema,
  FilePreviewResponseSchema,
  PreparedRemoteDragSchema,
  TransferConflictActionSchema,
  TransferSnapshotSchema,
} from "./file.js";

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

/**
 * The commands whose response the C++ host hands back **verbatim** from the Python sidecar.
 *
 * `IpcRouter::agent_run_command` (`native/desktop_host/src/ipc_router.cpp`) forwards
 * `client.request(...)` as the response frame without rebuilding it field by field, and
 * `provider_command` does the same. The sidecar is a separately-versioned process: this
 * repository does not own its JSON, so an added field there is a *compatibility* event,
 * not a bug in a file we can fix. Gating these responses strictly meant one added key —
 * top level or nested — turned a successful agent/provider call into a parse exception in
 * the UI, which is the failure this list exists to scope.
 *
 * Pinned by name, and pinned again in `ipc.test.ts`: a command moving in or out of this
 * list changes how much compatibility the contract owes a foreign process.
 *
 * Everything else in `IPC_SCHEMAS` stays strict. A response the host composes itself
 * (`agent_status`, `server_connect`, the host-key replies) comes from two files in this
 * repository, so an unknown key there is a real disagreement and failing is the alarm.
 */
export const SIDECAR_FORWARDED_COMMANDS = [
  "agent_run_start",
  "agent_run_stop",
  "agent_context_reset",
  "agent_context_reset_all",
  "agent_memory_list",
  "agent_memory_save",
  "agent_memory_delete",
  "provider_list",
  "provider_save",
  "provider_activate",
  "provider_delete",
  "provider_models",
  "provider_test",
] as const;

export type SidecarForwardedCommand = (typeof SIDECAR_FORWARDED_COMMANDS)[number];

type RawSchemaDef = Record<string, unknown> & { type: string };

function rawDef(schema: z.ZodType): RawSchemaDef {
  return (schema as unknown as { def: RawSchemaDef }).def;
}

/**
 * How a rebuilt tree treats a key its shape does not mention.
 *
 * - `tolerate` — keep the key (a foreign process may add fields we do not know yet);
 * - `strict` — refuse it (`unrecognized_keys`), i.e. the pre-tolerance reading.
 *
 * There is deliberately no `strip` (drop-it) option. Nobody needs it, and zod's default
 * already means it for a plain `z.object`; adding a third mode to the walker would be a
 * dead branch inside the fix whose whole point is that the contract's two readings of an
 * unknown key are both explicit.
 */
type KeyPolicy = "tolerate" | "strict";

/**
 * Rebuild a schema tree with a different unknown-key policy, keeping everything else.
 *
 * This walks the *actual* schema — `def.shape`, array `element`, wrapper `innerType`,
 * union `options`, tuple `items`, record `keyType`/`valueType` — and clones each node with
 * `z.clone`, which keeps that node's own `checks` (so a `.refine()` still runs) and its
 * declared output type. Setting `catchall` is the only semantic change: `z.unknown()` for
 * tolerance, `z.never()` for strictness.
 *
 * Deep, not shallow: a top-level `catchall` alone would let the sidecar add a field inside
 * `provider.providers[0]` and still crash the UI, which is the same bug one level down.
 *
 * The return type is the input type, deliberately. The *declared* contract of a forwarded
 * command stays its known shape — `consistency.ts` pins it, and handlers keep the types
 * they had — while the runtime gate accepts a superset and reports the difference.
 */
function rebuildKeyPolicy<T extends z.ZodType>(schema: T, policy: KeyPolicy): T {
  const def = rawDef(schema);
  const clone = (next: Record<string, unknown>): T =>
    z.clone(schema, { ...def, ...next } as never) as unknown as T;
  switch (def.type) {
    case "object": {
      const shape = (def["shape"] ?? {}) as Record<string, z.ZodType>;
      const rebuilt: Record<string, z.ZodType> = {};
      for (const [key, value] of Object.entries(shape)) rebuilt[key] = rebuildKeyPolicy(value, policy);
      return clone({ shape: rebuilt, catchall: policy === "strict" ? z.never() : z.unknown() });
    }
    case "array":
      return clone({ element: rebuildKeyPolicy(def["element"] as z.ZodType, policy) });
    case "optional":
    case "nullable":
    case "default":
    case "readonly":
    case "nonoptional":
    case "catch":
    case "prefault":
      return clone({ innerType: rebuildKeyPolicy(def["innerType"] as z.ZodType, policy) });
    case "union":
      return clone({ options: (def["options"] as z.ZodType[]).map((option) => rebuildKeyPolicy(option, policy)) });
    case "tuple":
      return clone({ items: (def["items"] as z.ZodType[]).map((item) => rebuildKeyPolicy(item, policy)) });
    case "record":
      return clone({
        keyType: rebuildKeyPolicy(def["keyType"] as z.ZodType, policy),
        valueType: rebuildKeyPolicy(def["valueType"] as z.ZodType, policy),
      });
    case "lazy":
      return clone({ getter: () => rebuildKeyPolicy((def["getter"] as () => z.ZodType)(), policy) });
    default:
      // Leaves, enums, literals, primitives: nothing below them can carry an unknown key.
      return schema;
  }
}

/**
 * The tolerant reading of a sidecar-forwarded response: unknown keys are kept.
 *
 * Missing fields and wrong types are still refused — this is a relaxation of *one*
 * question ("may there be a key I do not know?"), not a passthrough. Keep the addition
 * observable with `unknownResponseKeys`, otherwise a drifting sidecar becomes invisible.
 */
export function forwardedResponseSchema<T extends z.ZodType>(schema: T): T {
  return rebuildKeyPolicy(schema, "tolerate");
}

/**
 * The strict reading of the same tree: unknown keys are refused (`unrecognized_keys`).
 *
 * Exists so the loosening stays a *reading* rather than a deletion — a test or audit can
 * still ask "would this have failed before?" and get the answer the strict rows give.
 */
export function strictResponseSchema<T extends z.ZodType>(schema: T): T {
  return rebuildKeyPolicy(schema, "strict");
}

/**
 * The keys `value` carries that `schema`'s object shapes do not mention, anywhere in
 * the payload (top level or nested).
 *
 * This is what keeps tolerance honest. Accepting an unknown key without recording it
 * would trade a visible parse failure for an invisible drift: the UI would keep working
 * while the sidecar and this contract quietly diverged. The desktop gate turns each
 * returned name into one diagnostic per command.
 *
 * Only *names* are returned, not paths: the point is "this contract does not know this
 * field", and the shape that found it is the caller's `command`. Keys under an unknown
 * key are not walked — their shape is unknown by definition.
 */
export function unknownResponseKeys(schema: z.ZodType, value: unknown): string[] {
  const found = new Set<string>();
  collectUnknownKeys(schema, value, found);
  return [...found];
}

function collectUnknownKeys(schema: z.ZodType, value: unknown, found: Set<string>): void {
  if (value === null || typeof value !== "object") return;
  const def = rawDef(schema);
  switch (def.type) {
    case "object": {
      const shape = (def["shape"] ?? {}) as Record<string, z.ZodType>;
      for (const [key, entry] of Object.entries(value as Record<string, unknown>)) {
        const child = shape[key];
        if (child === undefined) found.add(key);
        else collectUnknownKeys(child, entry, found);
      }
      return;
    }
    case "array": {
      const element = def["element"] as z.ZodType;
      for (const item of value as unknown[]) collectUnknownKeys(element, item, found);
      return;
    }
    case "tuple": {
      const items = def["items"] as z.ZodType[];
      (value as unknown[]).forEach((item, index) => {
        const itemSchema = items[index];
        if (itemSchema !== undefined) collectUnknownKeys(itemSchema, item, found);
      });
      return;
    }
    case "record": {
      const valueType = def["valueType"] as z.ZodType;
      for (const entry of Object.values(value as Record<string, unknown>)) {
        collectUnknownKeys(valueType, entry, found);
      }
      return;
    }
    case "optional":
    case "nullable":
    case "default":
    case "readonly":
    case "nonoptional":
    case "catch":
    case "prefault":
      collectUnknownKeys(def["innerType"] as z.ZodType, value, found);
      return;
    case "union":
      // The tolerant tree validates every option, so the option that owns the payload is
      // not recoverable from a successful parse. Every object branch is walked and the
      // names are deduplicated, which is the honest answer: any of them could be the one.
      for (const option of def["options"] as z.ZodType[]) collectUnknownKeys(option, value, found);
      return;
    case "lazy":
      collectUnknownKeys((def["getter"] as () => z.ZodType)(), value, found);
      return;
    default:
      return;
  }
}

export const CorePingResponseSchema = z.strictObject({
  version: z.string().min(1),
  os: z.string().min(1),
});

export const ServerListResponseSchema = z.strictObject({ servers: z.array(ServerSchema) });
export const ServerAddResponseSchema = z.strictObject({ server: ServerSchema });
export const ServerConnectResponseSchema = z.strictObject({ status: z.literal("connected") });
export const ServerSnapshotResponseSchema = z.strictObject({ snapshot: ServerSnapshotSchema });
export const TerminalOpenResponseSchema = z.strictObject({ terminalSessionId: IpcTerminalSessionIdSchema });

/** Sidecar lifecycle, `agent_*` commands (fields mirror the host-side supervisor state). */

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
   * **Declared, but with no producer yet.**
   *
   * The shape is the recovery record ADR 0010 describes, and it is optional rather than
   * nullable so a status without recovery stays byte-identical to the older fixture. But
   * nothing in this repository emits it: `agent_status` is composed field by field by the
   * C++ host (`native/desktop_host/src/ipc_router.cpp:9010-9037`) and that branch never
   * sets `restart`, because the C++ host has no supervisor that could restart a sidecar
   * (R1-desktop H-1). It was previously documented as `skip_serializing_if` "on the Rust
   * side" — a host that no longer exists, which is how the absence went unnoticed.
   *
   * Kept rather than deleted: `ipc/index.ts` declares it on `AgentStatus`, and four UI
   * call sites branch on it (`features/agent/AgentPanel.tsx:117,476`,
   * `features/agent/AgentNotices.tsx:32`, `features/settings/RuntimeSettings.tsx:92-95`).
   * Removing the field would delete those branches instead of the missing producer, so the
   * field stays and the day a supervisor lands it has a contract to fill.
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
 * `pinnedFingerprint` is optional, never nullable: the host omits the key entirely when
 * there is no pin (`native/desktop_host/src/ipc_router.cpp:8497,8634` only write it when
 * present), and a `null` here would mean both sides disagree about what "no pin" looks
 * like.
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
  /**
   * **Host-implemented, but no UI call site.**
   *
   * `remote_file_list` and `remote_file_preview` are what the file browser actually calls;
   * nothing in `apps/desktop/src` reaches for a whole-file read, and the only caller of
   * this row is the preview fixture (`apps/desktop/src/lib/preview-fixtures.ts:500`, pinned
   * by `apps/desktop/tests/preview-fixtures.test.ts:21`). The host side does implement it.
   *
   * Kept because it is the one command that returns file *content* rather than a bounded
   * preview, and the file surface is still being built — a third party or the next UI
   * revision can call it without a contract change. It is annotated rather than deleted so
   * that "the fixture covers it" is not mistaken for "the product uses it".
   */
  remote_file_read: {
    params: z.strictObject({ serverId: IpcServerIdSchema, path: z.string().min(1) }),
    response: z.strictObject({ path: z.string().min(1), content: z.string(), truncated: z.boolean() }),
  },
  remote_file_preview: {
    params: z.strictObject({ serverId: IpcServerIdSchema, path: z.string().min(1).max(4_096).regex(/^\//) }),
    response: FilePreviewResponseSchema,
  },
  file_prepare_remote_drag: {
    params: z.strictObject({
      serverId: IpcServerIdSchema,
      path: z.string().min(1).max(4_096).regex(/^\//),
    }),
    response: PreparedRemoteDragSchema,
  },
  file_drag_out_start: {
    params: z.strictObject({ dragId: z.string().regex(/^remote_drag_[a-f0-9]{64}$/) }),
    response: EMPTY_PAYLOAD,
  },
  local_file_preview: {
    params: z.strictObject({ handleId: z.string().trim().min(16).max(128) }),
    response: FilePreviewResponseSchema,
  },
  local_path_pick: {
    params: z.strictObject({ kind: z.enum(["uploadFiles", "uploadDirectory", "downloadDirectory"]) }),
    response: z.strictObject({ handles: z.array(LocalPathHandleSchema).max(256) }),
  },
  file_transfer_upload: {
    params: z.strictObject({
      serverId: IpcServerIdSchema,
      remoteDirectory: z.string().min(1).max(4_096).regex(/^\//),
      sourceHandleIds: z.array(z.string().trim().min(16).max(128)).min(1).max(256),
    }),
    response: z.strictObject({ transfer: TransferSnapshotSchema }),
  },
  file_transfer_download: {
    params: z.strictObject({
      serverId: IpcServerIdSchema,
      remotePaths: z.array(z.string().min(1).max(4_096).regex(/^\//)).min(1).max(256),
      destinationHandleId: z.string().trim().min(16).max(128),
    }),
    response: z.strictObject({ transfer: TransferSnapshotSchema }),
  },
  file_transfer_list: {
    params: z.strictObject({ serverId: IpcServerIdSchema.optional() }),
    response: z.strictObject({ transfers: z.array(TransferSnapshotSchema).max(256) }),
  },
  /**
   * **Host-implemented, but no UI call site.**
   *
   * The transfer list and the `file.transfer_updated` event are what the transfer UI
   * consumes; nothing reads a single transfer by id, and this row's only caller is the
   * preview fixture (`apps/desktop/src/lib/preview-fixtures.ts:514`, pinned by
   * `apps/desktop/tests/preview-fixtures.test.ts:28`).
   *
   * Kept for the same reason as `remote_file_read`: it is the point-read half of the
   * transfer surface, useful to a caller that has a transfer id out of band, and deleting
   * it would remove the only way to ask about one transfer without listing them all.
   */
  file_transfer_get: {
    params: z.strictObject({ transferId: z.string().trim().min(1).max(128) }),
    response: z.strictObject({ transfer: TransferSnapshotSchema }),
  },
  file_transfer_cancel: {
    params: z.strictObject({ transferId: z.string().trim().min(1).max(128) }),
    response: z.strictObject({ transfer: TransferSnapshotSchema }),
  },
  file_transfer_resolve_conflict: {
    params: z.strictObject({ transferId: z.string().trim().min(1).max(128), action: TransferConflictActionSchema }),
    response: z.strictObject({ transfer: TransferSnapshotSchema }),
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
  /**
   * **Host-implemented, but no UI call site.**
   *
   * Stopping work goes through `agent_run_stop` (and the Python tool-call interruption
   * path); nothing in `apps/desktop/src` kills the sidecar process itself. The host does
   * implement it, and it is the blunt instrument of last resort for a wedged sidecar, so it
   * stays declared with a fixture — annotated, because a schema without a caller used to
   * read as a working feature.
   */
  agent_kill: { params: EMPTY_PAYLOAD, response: z.strictObject({ killed: z.boolean() }) },
  agent_status: { params: EMPTY_PAYLOAD, response: AgentStatusSchema },
  agent_logs: { params: EMPTY_PAYLOAD, response: AgentLogsResponseSchema },
  agent_run_start: {
    params: z
      .strictObject({
        runId: z.string().trim().min(1).max(256).optional(),
        sessionId: z.string().trim().min(1).max(256),
        taskId: z.string().trim().min(1).max(256).optional(),
        taskBudget: TaskBudgetSchema.optional(),
        prompt: z.string().max(100_000),
        persona: AgentPersonaSchema.optional(),
        participant: AgentParticipantSchema.optional(),
        messageId: z.string().trim().min(1).max(256).optional(),
        parts: AgentPromptPartsSchema.optional(),
        delivery: z.enum(["async", "sync"]).optional(),
        resume: z.boolean().optional(),
        providerId: z.string().trim().min(1).max(256).optional(),
        model: z.string().trim().min(1).max(256).optional(),
        reasoningEffort: AgentReasoningEffortSchema.optional(),
        toolAccess: z.literal("none").optional(),
        coordination: AgentCoordinationConfigSchema.optional(),
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
    response: forwardedResponseSchema(
      z.strictObject({
        runId: z.string().min(1),
        started: z.boolean(),
        duplicate: z.boolean().optional(),
        resumed: z.boolean().optional(),
        result: AgentRunResultSchema.optional(),
      }),
    ),
  },
  agent_team_tasks: {
    params: z.strictObject({ sessionId: z.string().trim().min(1).max(256).optional(), focusServerId: IpcServerIdSchema.optional() }),
    response: z.strictObject({ tasks: z.array(AgentTeamTaskSchema).max(128) }),
  },
  agent_run_stop: {
    params: z.strictObject({ runId: z.string().trim().min(1).max(256) }),
    response: forwardedResponseSchema(z.strictObject({ stopped: z.boolean() })),
  },
  agent_context_reset: {
    params: z.strictObject({ sessionId: z.string().trim().min(1).max(256), focusServerId: IpcServerIdSchema.optional() }),
    response: forwardedResponseSchema(z.strictObject({ reset: z.boolean() })),
  },
  agent_context_reset_all: {
    params: EMPTY_PAYLOAD,
    response: forwardedResponseSchema(z.strictObject({ reset: z.boolean(), count: z.number().int().nonnegative() })),
  },
  agent_memory_list: { params: AgentMemoryListInputSchema, response: forwardedResponseSchema(z.strictObject({ memories: z.array(AgentMemorySchema).max(100) })) },
  agent_memory_save: { params: AgentMemorySaveInputSchema, response: forwardedResponseSchema(z.strictObject({ memory: AgentMemorySchema })) },
  agent_memory_delete: { params: AgentMemoryDeleteInputSchema, response: forwardedResponseSchema(z.strictObject({ deleted: z.boolean() })) },
  agent_approval_respond: {
    params: ApprovalResponseSchema,
    response: z.strictObject({ accepted: z.boolean() }),
  },
  chat_session_list: {
    params: z.strictObject({
      query: z.string().trim().max(200).optional(),
      serverId: IpcServerIdSchema.nullable().optional(),
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
        participant: AgentParticipantSchema.optional(),
        model: z.string().trim().min(1).max(256).optional(),
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
  provider_list: { params: EMPTY_PAYLOAD, response: forwardedResponseSchema(z.strictObject({ providers: z.array(ProviderConfigSchema) })) },
  provider_save: {
    params: ProviderSaveInputSchema,
    response: forwardedResponseSchema(z.strictObject({ provider: ProviderConfigSchema })),
  },
  provider_activate: {
    params: z.strictObject({ providerId: z.string().min(1) }),
    response: forwardedResponseSchema(z.strictObject({ provider: ProviderConfigSchema })),
  },
  provider_delete: {
    params: z.strictObject({ providerId: z.string().min(1) }),
    response: forwardedResponseSchema(ProviderDeleteResponseSchema),
  },
  provider_models: {
    params: z.strictObject({ providerId: z.string().min(1) }),
    response: forwardedResponseSchema(z.strictObject({ models: z.array(ProviderModelOptionSchema) })),
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
  investigation_task_list: {
    params: InvestigationTaskListInputSchema,
    response: InvestigationTaskListResponseSchema,
  },
  investigation_task_get: {
    params: z.strictObject({ taskId: z.string().trim().min(1).max(256) }),
    response: InvestigationTaskDetailResponseSchema,
  },
  investigation_task_create: {
    params: z.strictObject({ input: InvestigationTaskCreateInputSchema }),
    response: InvestigationTaskResponseSchema,
  },
  investigation_task_start: {
    params: z.strictObject({ taskId: z.string().trim().min(1).max(256) }),
    response: InvestigationTaskStartResponseSchema,
  },
  investigation_task_stop: {
    params: InvestigationTaskStopInputSchema,
    response: InvestigationTaskResponseSchema,
  },
  /**
   * **Host-implemented, but no UI call site.**
   *
   * The UI drives task transitions through `investigation_task_start` /
   * `_stop` / `_recover`; the raw status write is the host-side primitive those three are
   * built on and nothing in `apps/desktop/src` calls it directly. Kept as the declared
   * primitive with a fixture, and annotated so the presence of a schema is not read as
   * evidence of a caller.
   */
  investigation_task_status_update: {
    params: InvestigationTaskStatusUpdateInputSchema,
    response: InvestigationTaskResponseSchema,
  },
  investigation_task_recover: {
    params: z.strictObject({ input: InvestigationTaskRecoverInputSchema }),
    response: InvestigationTaskResponseSchema,
  },
  investigation_brief_select: {
    params: z.strictObject({ input: InvestigationBriefSelectInputSchema }),
    response: InvestigationBriefResponseSchema,
  },
  investigation_retention_preview: {
    params: InvestigationRetentionPreviewInputSchema,
    response: InvestigationRetentionPreviewResponseSchema,
  },
  investigation_retention_prune: {
    params: z.strictObject({ input: InvestigationRetentionPruneInputSchema }),
    response: InvestigationRetentionPruneResponseSchema,
  },
  investigation_schedule_list: {
    params: EMPTY_PAYLOAD,
    response: InvestigationScheduleListResponseSchema,
  },
  investigation_schedule_runs: {
    params: z.strictObject({ scheduleId: z.string().trim().min(1).max(256) }),
    response: InvestigationScheduleRunListResponseSchema,
  },
  investigation_schedule_create: {
    params: z.strictObject({ input: InvestigationScheduleCreateInputSchema }),
    response: z.strictObject({ schedule: InvestigationScheduleSchema }),
  },
  investigation_schedule_update: {
    params: z.strictObject({ input: InvestigationScheduleUpdateInputSchema }),
    response: z.strictObject({ schedule: InvestigationScheduleSchema }),
  },
  /**
   * **Host-implemented, but no UI call site.**
   *
   * This is the scheduler's internal step boundary — the host calls it on its own clock
   * and answers with what the tick decided. The UI reads schedules through
   * `investigation_schedule_list` / `_runs` and never ticks one, so this row exists for
   * tests and for a host that wants to drive a tick explicitly. Annotated rather than
   * deleted because the tick is a real, specified state transition, not dead surface.
   */
  investigation_schedule_tick: {
    params: z.strictObject({ input: InvestigationScheduleTickInputSchema }),
    response: InvestigationScheduleTickResponseSchema,
  },
  provider_test: {
    params: z.strictObject({ providerId: z.string().min(1) }),
    response: forwardedResponseSchema(z.strictObject({ ok: z.literal(true) })),
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
 * be dropped, never re-asked. So the schema is here to make a drifted native payload
 * a *visible* dropped event rather than a silent runtime failure deep in a writer.
 *
 * Each `agent.*` channel is gated by **its own member schema**, not by the whole
 * `AgentStreamEventSchema` union. The union was one level too coarse: it could only
 * ask "is this some valid agent event", so `DesktopEventPayload<"agent.completed">`
 * was the entire union and `useAgentRun` had to cast nine times to reach the member it
 * already knew it was handling. A cast there is where a payload that passed the loose
 * gate reaches a handler as a shape nothing validated.
 *
 * Narrowing is safe because the channel name *is* the payload's discriminator: the sidecar
 * publishes an `agent.stream` notification tagged by `type`
 * (`agent/src/yukinal_agent/sidecar.py:1015-1019`) and the host routes it under that same
 * string (`native/desktop_host/src/ipc_router.cpp:8988-8990`), with no separator rewriting.
 * Nothing the transport can deliver is rejected by this; it stops accepting what the
 * transport cannot produce.
 */
export const EVENT_SCHEMAS = {
  "agent.started": AGENT_EVENT_MEMBER_SCHEMAS["agent.started"],
  "agent.team_task": AGENT_EVENT_MEMBER_SCHEMAS["agent.team_task"],
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
    // The host sends an integer exit code or nothing, so a negative or fractional code is drift.
    exitCode: z.number().int().nonnegative().nullable(),
  }),
  /**
   * A dropped-output notice. `missedEvents` is the count the broadcast channel reported
   * as lagged; it is not attributable to one session, so there is no session id here.
   */
  "terminal.output_lost": z.strictObject({
    missedEvents: z.number().int().nonnegative(),
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
  "file.local_dropped": LocalFileDropEventSchema,
  "file.transfer_updated": z.strictObject({ transfer: TransferSnapshotSchema }),
  "activity.created": ActivitySchema,
  "investigation.schedule_notification": z.strictObject({
    scheduleId: z.string().trim().min(1).max(256),
    scheduleRunId: z.string().trim().min(1).max(256),
    taskId: z.string().trim().min(1).max(256),
    outcome: z.enum(["changed", "failed", "baseline", "no_change", "insufficient_evidence"]),
    notificationPolicy: z.enum(INVESTIGATION_NOTIFICATION_POLICIES),
    title: z.string().trim().min(1).max(512),
    evidenceIds: z.array(z.string().trim().min(1).max(256)).max(256),
    findingId: z.string().trim().min(1).max(256).optional(),
    briefId: z.string().trim().min(1).max(256).optional(),
    at: z.string().trim().min(1).max(80),
  }),
} as const;
