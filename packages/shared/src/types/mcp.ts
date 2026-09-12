/**
 * MCP servers as the **settings UI** sees them (ADR 0014).
 *
 * This is the `mcp_server_*` IPC surface, not the agent-side host RPC surface (`host.mcp.catalog`,
 * which lives in `types/host.ts`). The two are deliberately separate shapes even though both
 * describe the same servers:
 *
 * - the catalog is what the sidecar needs in order to *call* tools (internal names, remote names,
 *   input schemas) and carries no configuration detail;
 * - this is what a human needs in order to *configure and watch* servers (label, command, args,
 *   enabled, pid, exit record, why it is unavailable).
 *
 * Merging them would put a server's launch command into the agent's prompt-adjacent data and the
 * agent's tool descriptions into the settings form. Neither belongs there.
 *
 * Every shape here mirrors `apps/desktop/src-tauri/src/commands/mcp.rs` field for field. The Rust
 * DTOs are the source of truth; if they change, this file is wrong and the Zod schemas next to it
 * will reject the payload at the IPC gate rather than letting a half-read object into the UI.
 */

import type { McpServerConfig } from "./provider.js";
import type { McpCatalogFailureCode } from "./host.js";

/**
 * One MCP tool as the server declared it (mirrors `McpToolDescriptor`).
 *
 * **`name` is the remote spelling, not the internal tool name.** The supervisor hands back what
 * `tools/list` said (`"echo"`, `"get_weather"`); the internal name the agent registers is
 * `mcp.<segment>.<name>` with both segments normalized (ADR 0004), and it is derived on the
 * host side when the catalog is built (`internal_tool_name()`), not carried here. A UI that
 * labelled these as internal names would be showing a name the model never calls.
 *
 * `description` and `inputSchema` are untrusted remote text; the UI may *show* them, and that is
 * all (MCP README §6).
 */
export interface McpToolDescriptor {
  /** The name `tools/call` needs — the server's own spelling, before normalization. */
  name: string;
  /** Set only when the remote spelling was rewritten; says what the server calls it. */
  remoteName?: string;
  description: string;
  inputSchema: unknown;
  outputSchema?: unknown;
}

/** A process exit, kept so "why did it die" survives the restart that fixed it. */
export interface McpExitRecord {
  code?: number | null;
  signal?: string | null;
  at: string;
  /** Human-readable form of the two fields above, produced by Rust. */
  reason: string;
}

/**
 * Live status of one server (mirrors `McpServerStatus`).
 *
 * `lastExit` is the field that makes a crash *visible*: this repo never restarts a crashed MCP
 * server (ADR 0014), so the exit record is the only thing standing between a user and "it just
 * doesn't work".
 */
export interface McpServerStatus {
  serverId: string;
  running: boolean;
  pid?: number | null;
  program?: string | null;
  startedAt?: string | null;
  protocolVersion?: string | null;
  serverName?: string | null;
  serverVersion?: string | null;
  toolCount: number;
  lastExit?: McpExitRecord | null;
  /** Bounded, redacted tail of the server's stderr. */
  stderrTail: string[];
  diagnostics: string[];
}

/** Why a configured server is not usable right now. Absent means "nothing known is wrong". */
export interface McpServerUnavailable {
  code: McpCatalogFailureCode;
  /** The Rust error text, verbatim: it names the next step. */
  message: string;
}

/** One row of the settings list. */
export interface McpServerView {
  config: McpServerConfig;
  status: McpServerStatus;
  /** Cached descriptors; empty while the server is not running. */
  tools: McpToolDescriptor[];
  unavailable?: McpServerUnavailable;
}

export interface McpServerListResponse {
  servers: McpServerView[];
}

/**
 * What the form sends. Deliberately **not** `McpServerConfig`: `allowedTools` and `trustLevel`
 * are stored-but-unread today (no UI writes them), and a save that carried them would overwrite
 * whatever a future review flow had recorded with whatever this form happened to hold.
 */
export interface McpServerSaveInput {
  id: string;
  label: string;
  transport: string;
  command?: string;
  args?: string[];
  url?: string;
  enabled: boolean;
}

export interface McpServerDeleteResponse {
  deleted: boolean;
  /** There was a running process and it has been stopped. */
  stopped: boolean;
}

export interface McpShutdownOutcome {
  wasRunning: boolean;
  /** `true` means closing stdin was not enough and the process was killed. */
  killed: boolean;
  /** `true` means it could not be reaped within the budget. */
  unreaped: boolean;
}

export interface McpServerStopResponse {
  server: McpServerView;
  /** Absent when this server was never started, so there was nothing to stop. */
  shutdown?: McpShutdownOutcome;
}
