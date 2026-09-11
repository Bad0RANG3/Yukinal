/** Sidecar requests that are executed by the Rust host, never by Node. */

import type { ToolError, ToolTarget } from "./tool.js";

export const HOST_METHODS = {
  toolExecute: "host.tool.execute",
  contextFetch: "host.context.fetch",
  toolCancel: "host.tool.cancel",
} as const;

export interface HostToolExecuteRequest {
  callId: string;
  traceId: string;
  toolName: string;
  input: unknown;
  target: ToolTarget;
}

/** Cancels a previously sent host.tool.execute request by its JSON-RPC id. */
export interface HostToolCancelRequest {
  requestId: number;
}

export interface HostToolCancelResponse {
  cancelled: boolean;
}

export type HostToolExecuteResponse =
  | { status: "success"; output?: unknown }
  | { status: "failed"; error: ToolError }
  | { status: "cancelled"; error?: ToolError };

/**
 * The tuple exists so `schemas/host.ts` can write `z.enum(HOST_CONTEXT_KINDS)`.
 *
 * It was previously a bare union here plus a hand-written `z.enum([...])` there — two
 * copies of one list with nothing pinning them. Adding a kind to the union would
 * compile, and every real payload carrying it would then be rejected at the IPC gate
 * as a *runtime* failure rather than a compile error. This is the same pattern
 * `types/enums.ts` and `types/risk.ts` already use.
 */
export const HOST_CONTEXT_KINDS = ["server", "snapshot", "workspace"] as const;
export type HostContextKind = (typeof HOST_CONTEXT_KINDS)[number];

export interface HostContextRequest {
  kind: HostContextKind;
  /** For snapshot, this is the server id whose latest snapshot is requested. */
  id: string;
}

export type HostContextResponse =
  | { status: "success"; data: unknown }
  | { status: "not_found" }
  | { status: "failed"; error: ToolError };
