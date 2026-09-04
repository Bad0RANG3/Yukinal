/** Sidecar requests that are executed by the Rust host, never by Node. */

import type { ToolError, ToolTarget } from "./tool.js";

export const HOST_METHODS = {
  toolExecute: "host.tool.execute",
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
