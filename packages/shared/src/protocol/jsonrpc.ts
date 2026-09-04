/**
 * Sidecar protocol: React/UI  <->  apps/agent (standalone Node process, ADR 0001).
 *
 * Transport: newline-delimited JSON-RPC 2.0 over the Tauri sidecar's stdio
 * (ADR 0006). stdout carries frames only — logs go to stderr, never stdout.
 */

export const YUKINAL_RPC_VERSION = "1.0" as const;

export interface JsonRpcRequest {
  jsonrpc: "2.0";
  id: number;
  method: string;
  params?: unknown;
}

export interface JsonRpcNotification {
  jsonrpc: "2.0";
  method: string;
  params?: unknown;
}

export interface JsonRpcError {
  code: number;
  message: string;
  data?: unknown;
}

export interface JsonRpcSuccess {
  jsonrpc: "2.0";
  id: number;
  result: unknown;
}

export interface JsonRpcFailure {
  jsonrpc: "2.0";
  id: number;
  error: JsonRpcError;
}

export type JsonRpcMessage =
  | JsonRpcRequest
  | JsonRpcNotification
  | JsonRpcSuccess
  | JsonRpcFailure;

export const RPC_ERROR = {
  PARSE_ERROR: -32700,
  INVALID_REQUEST: -32600,
  METHOD_NOT_FOUND: -32601,
  INVALID_PARAMS: -32602,
  INTERNAL_ERROR: -32603,
  NOT_IMPLEMENTED: -32604,
  /** the user hit Stop and the runtime honoured it. */
  CANCELLED: -32800,
  TIMEOUT: -32001,
  /** denial is a first-class protocol outcome, not an exception. */
  DENIED_BY_POLICY: -32002,
  APPROVAL_REJECTED: -32003,
  UNKNOWN_TOOL: -32004,
} as const;

/** Methods the agent process answers; more are added as the runtime grows. */
export const AGENT_METHODS = {
  initialize: "initialize",
  describe: "system.describe",
  ping: "system.ping",
  listTools: "tools.list",
  runStart: "agent.run.start",
  runStop: "agent.run.stop",
  approvalRespond: "agent.approval.respond",
  providerModels: "provider.models",
} as const;

export type AgentMethodName = (typeof AGENT_METHODS)[keyof typeof AGENT_METHODS];

/** Notifications the agent emits upward (mapped 1:1 onto AgentStreamEvent, ). */
export const AGENT_NOTIFICATIONS = {
  stream: "agent.stream",
  log: "agent.log",
} as const;

export interface InitializeParams {
  protocolVersion: typeof YUKINAL_RPC_VERSION;
  /** Yukinal desktop version, for capability negotiation + audit. */
  clientVersion: string;
  /** Absolute path of the per-user data dir handed over by Rust (). */
  dataDir: string;
}

export interface InitializeResult {
  protocolVersion: typeof YUKINAL_RPC_VERSION;
  agentVersion: string;
  capabilities: {
    streaming: boolean;
    toolCalling: boolean;
    cancellation: boolean;
    mcp: boolean;
  };
}

export interface SystemDescribeResult {
  providers: Array<{ id: string; label: string; kind: string; configured: boolean }>;
  toolCount: number;
  permissionPolicyIds: string[];
  /** Tools whose provider-facing names collide (ADR 0004) — must be empty. */
  toolNameCollisions: string[];
  implemented: Record<string, boolean>;
}

export function isJsonRpcRequest(message: JsonRpcMessage): message is JsonRpcRequest {
  return "method" in message && "id" in message;
}

export function isJsonRpcNotification(message: JsonRpcMessage): message is JsonRpcNotification {
  return "method" in message && !("id" in message);
}

export function isJsonRpcResponse(message: JsonRpcMessage): message is JsonRpcSuccess | JsonRpcFailure {
  return "id" in message && !("method" in message);
}
