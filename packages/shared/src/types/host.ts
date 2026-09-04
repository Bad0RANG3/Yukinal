/** Sidecar requests that are executed by the Rust host, never by Node. */

import type { ToolError, ToolTarget } from "./tool.js";

export const HOST_METHODS = {
  toolExecute: "host.tool.execute",
  contextFetch: "host.context.fetch",
} as const;

export interface HostToolExecuteRequest {
  callId: string;
  traceId: string;
  toolName: string;
  input: unknown;
  target: ToolTarget;
}

export type HostToolExecuteResponse =
  | { status: "success"; output?: unknown }
  | { status: "failed"; error: ToolError }
  | { status: "cancelled"; error?: ToolError };

export type HostContextKind = "server" | "snapshot" | "workspace";

export interface HostContextRequest {
  kind: HostContextKind;
  /** For snapshot, this is the server id whose latest snapshot is requested. */
  id: string;
}

export type HostContextResponse =
  | { status: "success"; data: unknown }
  | { status: "not_found" }
  | { status: "failed"; error: ToolError };
