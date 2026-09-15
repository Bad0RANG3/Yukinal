/** Sidecar requests that are executed by the Rust host, never by Node. */

import type { ToolError, ToolTarget } from "./tool.js";

export const HOST_METHODS = {
  toolExecute: "host.tool.execute",
  contextFetch: "host.context.fetch",
  toolCancel: "host.tool.cancel",
  /**
   * The MCP tool catalog (ADR 0014). The host owns every child process, so the sidecar
   * cannot discover MCP servers itself: it asks for `host.mcp.catalog` and registers what
   * comes back.
   *
   * A request has no parameters (the host reads its own `mcp_servers` table), and the
   * response is `HostMcpCatalogResponse`.
   */
  mcpCatalog: "host.mcp.catalog",
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

/* ── MCP catalog (ADR 0014) ────────────────────────────────────────────── */

/**
 * Why one MCP server is not usable right now.
 *
 * This tuple is the TypeScript half of `McpFailureCode::ALL` in
 * `apps/desktop/src-tauri/src/commands/mcp.rs`; a Rust test asserts the two lists agree
 * word for word. Kept as a tuple (not a bare union) for the reason spelled out above
 * `HOST_CONTEXT_KINDS`: `z.enum` needs a value at runtime.
 */
export const MCP_CATALOG_FAILURE_CODES = [
  "disabled",
  "invalid_config",
  "launch_failed",
  /** The process is dead. It is **not** restarted automatically. */
  "exited",
  /** It did not come up inside the catalog request's budget. */
  "timeout",
  "request_failed",
] as const;
export type McpCatalogFailureCode = (typeof MCP_CATALOG_FAILURE_CODES)[number];

/**
 * One MCP tool, as declared by the server over the wire.
 *
 * `name` is the internal name from ADR 0004 (`mcp.<server>.<tool>`) and is what the agent
 * registers; `serverId` is the identity the call is attributed to; `tool` is the name
 * `tools/call` needs. `description` and `inputSchema` are *untrusted* remote declarations —
 * the MCP boundary's rule that description text is always untrusted data (the repository
 * `docs/boundaries/mcp.md`, 「边界：外部工具（MCP）」) is why they are carried through unchanged, never
 * executed or trusted.
 */
export interface HostMcpCatalogTool {
  name: string;
  serverId: string;
  tool: string;
  /** Present only when the remote spelling had to be rewritten; says what the server calls it. */
  remoteName?: string;
  description: string;
  inputSchema: unknown;
}

export interface HostMcpCatalogServer {
  serverId: string;
  /** The normalized name segment inside `name` (ADR 0004). */
  segment: string;
  label: string;
  tools: HostMcpCatalogTool[];
}

export interface HostMcpCatalogFailure {
  serverId: string;
  code: McpCatalogFailureCode;
  message: string;
}

export interface HostMcpCatalogResponse {
  servers: HostMcpCatalogServer[];
  failures: HostMcpCatalogFailure[];
}

/** Body of a successful `mcp.<server>.<tool>` call through `host.tool.execute`. */
export interface HostMcpToolCallOutput {
  serverId: string;
  tool: string;
  /**
   * The tool ran and reported an error. Distinct from a failed call (`HostToolExecuteResponse`
   * `status: "failed"`): the agent adapter turns this into a `ToolFailure`, the host does not.
   */
  isError: boolean;
  /** Text blocks of `content`, joined with a newline. Untrusted. */
  text: string;
  content: HostMcpContentBlock[];
  structuredContent?: unknown;
}

/**
 * A result content block, mirroring `McpContentBlock`
 * (`crates/core/src/mcp/descriptor.rs`, `#[serde(tag = "type")]`).
 *
 * Blocks this repo does not model (image / audio / resource / resource_link) arrive as
 * `other` with the raw value: the host moves content, the adapter decides what to show.
 */
export type HostMcpContentBlock =
  | { type: "text"; text: string }
  | { type: "other"; value: unknown };
