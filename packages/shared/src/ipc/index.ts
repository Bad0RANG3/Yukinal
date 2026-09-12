/**
 * Tauri IPC contract shared by React and the Rust command layer.
 *
 * React may only call these commands and listen to these events. Rust mirrors the
 * names 1:1; the agent sidecar is reached through `@yukinal/agent-sdk`, never here.
 * Anything not in this map does not exist for the UI.
 */

import type {
  ApprovalResponse,
  AgentPromptPart,
  AgentRunRequest,
  AgentRunResult,
  ChatMessage,
  ChatMessageAppendInput,
  ChatSession,
  ChatSessionCreateInput,
  ChatSessionDetail,
  ChatSessionListInput,
} from "../types/chat.js";
import type { AgentPermissionMode, AgentRunMode } from "../types/risk.js";
import type {
  ActivityListInput,
  ActivityListResponse,
  ToolExecutionListInput,
  ToolExecutionListResponse,
} from "../types/activity.js";
import type { ServerSnapshot } from "../types/collector.js";
import type { ServerServicesResponse } from "../types/service.js";
import type { ServerLogsResponse } from "../types/log.js";
import type { RemoteFileListResponse, RemoteFileReadResponse } from "../types/file.js";
import type { AddServerInput, Server, UpdateServerInput, WorkspaceListResponse } from "../types/server.js";
import type { AiProviderConfig, ProviderModelOption, ProviderSaveInput } from "../types/provider.js";

export const IPC_COMMANDS = {
  /** Proves the IPC round trip works. */
  corePing: "core_ping",
  /** Server CRUD, backed by the local database. */
  serverList: "server_list",
  workspaceList: "workspace_list",
  serverAdd: "server_add",
  serverUpdate: "server_update",
  serverDelete: "server_delete",
  /** SSH connect / disconnect. */
  serverConnect: "server_connect",
  serverDisconnect: "server_disconnect",
  /** Latest collected snapshot. */
  serverSnapshot: "server_snapshot",
  /** Read-only systemd/Docker service discovery. */
  serverServices: "server_services",
  serverLogs: "server_logs",
  remoteFileList: "remote_file_list",
  remoteFileRead: "remote_file_read",
  activityList: "activity_list",
  toolExecutionList: "tool_execution_list",
  /** Terminal byte streams leave as events, not command returns. */
  terminalOpen: "terminal_open",
  terminalWrite: "terminal_write",
  terminalResize: "terminal_resize",
  terminalClose: "terminal_close",
  /** Sidecar lifecycle is owned by Rust; the UI never spawns processes. */
  agentSpawn: "agent_spawn",
  agentKill: "agent_kill",
  agentStatus: "agent_status",
  agentLogs: "agent_logs",
  /** Agent runs: Rust resolves the provider + credential and hands the call to the sidecar. */
  agentRunStart: "agent_run_start",
  agentRunStop: "agent_run_stop",
  agentApprovalRespond: "agent_approval_respond",
  /** Persistent Agent conversation history. */
  chatSessionList: "chat_session_list",
  chatSessionGet: "chat_session_get",
  chatSessionCreate: "chat_session_create",
  chatMessageAppend: "chat_message_append",
  chatSessionArchive: "chat_session_archive",
  chatSessionDelete: "chat_session_delete",
  /** AI provider config: settings panel only; the key never leaves the keychain. */
  providerList: "provider_list",
  providerSaveOpenai: "provider_save_openai",
  providerActivate: "provider_activate",
  providerModels: "provider_models",
  providerTest: "provider_test",
} as const;

export type IpcCommandName = (typeof IPC_COMMANDS)[keyof typeof IPC_COMMANDS];

export interface IpcCommandMap {
  provider_test: { params: { providerId: string }; response: { ok: true } };
  core_ping: { params: Record<string, never>; response: { version: string; os: string } };
  server_list: { params: Record<string, never>; response: { servers: Server[] } };
  workspace_list: { params: Record<string, never>; response: WorkspaceListResponse };
  server_add: { params: AddServerInput; response: { server: Server } };
  server_update: { params: UpdateServerInput; response: { server: Server } };
  server_delete: { params: { serverId: string }; response: { deleted: boolean } };
  server_connect: { params: { serverId: string }; response: { status: "connected" } };
  server_disconnect: { params: { serverId: string }; response: Record<string, never> };
  server_snapshot: { params: { serverId: string }; response: { snapshot: ServerSnapshot } };
  server_services: { params: { serverId: string }; response: ServerServicesResponse };
  server_logs: { params: { serverId: string }; response: ServerLogsResponse };
  remote_file_list: { params: { serverId: string; path: string }; response: RemoteFileListResponse };
  remote_file_read: { params: { serverId: string; path: string }; response: RemoteFileReadResponse };
  activity_list: { params: ActivityListInput; response: ActivityListResponse };
  tool_execution_list: { params: ToolExecutionListInput; response: ToolExecutionListResponse };
  terminal_open: {
    params: { serverId: string; cols: number; rows: number };
    response: { terminalSessionId: string };
  };
  terminal_write: { params: { terminalSessionId: string; data: string }; response: Record<string, never> };
  terminal_resize: {
    params: { terminalSessionId: string; cols: number; rows: number };
    response: Record<string, never>;
  };
  terminal_close: { params: { terminalSessionId: string }; response: Record<string, never> };
  agent_spawn: { params: Record<string, never>; response: AgentSpawnResponse };
  agent_kill: { params: Record<string, never>; response: { killed: boolean } };
  agent_status: { params: Record<string, never>; response: AgentStatus };
  agent_logs: { params: Record<string, never>; response: AgentLogs };
  agent_run_start: {
    params: {
      /** UI-generated identity lets the event consumer reject a foreign started event. */
      runId?: string;
      sessionId: string;
      prompt: string;
      messageId?: string;
      parts?: AgentPromptPart[];
      delivery?: "async" | "sync";
      resume?: boolean;
      providerId?: string;
      model?: string;
      workspaceId?: string;
      focusServerId?: string;
      permissionMode?: AgentPermissionMode;
      /** Bounds what the run may accomplish; enforced by the permission engine. */
      mode?: AgentRunMode;
      /**
       * Which policy must decide the run. Forwarded verbatim — Rust does not know what
       * a policy id means, and the sidecar's registry is what rejects an unknown one.
       * Omitted -> the sidecar derives the policy from the target environment.
       */
      policyId?: string;
    };
    /**
     * `started` says whether *this* call began execution. It is the sidecar's own
     * answer, forwarded rather than assumed: an admitted-but-not-executed message
     * (`resume: false`) and a retry of an already-running one both answer `false`.
     *
     * `result` is present exactly when the request asked for `delivery: "sync"`, and
     * carrying it is the whole point of that mode — the call already waits for the run
     * to reach a terminal state, and the run's outcome is what it was waiting for. For
     * `async` the same information arrives as the `agent.completed` / `agent.failed`
     * notification instead.
     */
    response: { runId: string; started: boolean; result?: AgentRunResult };
  };
  agent_run_stop: { params: { runId: string }; response: { stopped: boolean } };
  agent_approval_respond: { params: ApprovalResponse; response: { accepted: boolean } };
  chat_session_list: { params: ChatSessionListInput; response: { sessions: ChatSession[] } };
  chat_session_get: { params: { sessionId: string }; response: ChatSessionDetail };
  chat_session_create: { params: ChatSessionCreateInput; response: { session: ChatSession } };
  chat_message_append: { params: ChatMessageAppendInput; response: { message: ChatMessage } };
  chat_session_archive: { params: { sessionId: string; archived: boolean }; response: { session: ChatSession } };
  chat_session_delete: { params: { sessionId: string }; response: { deleted: boolean } };
  provider_list: { params: Record<string, never>; response: { providers: AiProviderConfig[] } };
  provider_save_openai: {
    params: ProviderSaveInput;
    response: { provider: AiProviderConfig };
  };
  provider_activate: {
    params: { providerId: string };
    response: { provider: AiProviderConfig };
  };
  provider_models: {
    params: { providerId: string };
    response: { models: ProviderModelOption[] };
  };
}

/**
 * Sidecar runtime identity. `entry` is the JS bundle Rust launches — recorded so the
 * UI can show *what actually started* instead of what the build assumed.
 */
export interface AgentSpawnResponse {
  pid: number;
  protocolVersion: string;
  agentVersion: string;
  entry: string;
  toolCount: number;
  alreadyRunning: boolean;
}

export interface AgentStatus {
  running: boolean;
  pid: number | null;
  protocolVersion: string | null;
  agentVersion: string | null;
  /** Registered tools reported by the sidecar at handshake time. */
  toolCount: number | null;
  /** Which bundle Rust actually launched (ADR 0008) — "what started" must be visible. */
  entry: string | null;
  startedAt: string | null;
  /** Last abnormal exit, kept until the next successful spawn so a crash stays visible. */
  lastExit: SidecarExit | null;
}

export interface SidecarExit {
  /** null when killed by signal / on platforms without exit codes. */
  code: number | null;
  signal: string | null;
  at: string;
}

/** Bounded tail of sidecar stderr: the "why did it die" affordance. */
export interface AgentLogs {
  lines: string[];
  capacity: number;
}

/** Re-exported so the UI can validate payloads it sends into the sidecar. */
export type { AgentRunRequest, ApprovalResponse };
