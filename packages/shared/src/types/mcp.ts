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

import type {
  McpHttpAuthHeaderConfig,
  McpOAuthClientAuth,
  McpOAuthConfig,
  McpOAuthFlow,
  McpServerConfig,
} from "./provider.js";
import type { McpCatalogFailureCode } from "./host.js";
import type { RestartRecord } from "./lifecycle.js";

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
 * all (the repository `docs/boundaries/mcp.md`, 「边界：外部工具（MCP）」: description text is untrusted
 * data).
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
 * `lastExit` is the field that makes a stdio crash *visible*. Bounded recovery may replace the
 * process while keeping this record, so a recovered server still explains why it restarted.
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
  /** Bounded automatic recovery after `lastExit`; absent when none was attempted. */
  restart?: RestartRecord;
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
 * belong to the separate review command; a connection edit must not overwrite the reviewed tool
 * surface with whatever the ordinary form happened to hold.
 */
export interface McpServerSaveInput {
  id: string;
  label: string;
  transport: string;
  command?: string;
  args?: string[];
  url?: string;
  /** Ordered headers. A missing secret preserves the matching stored header. */
  httpAuthHeaders?: McpHttpAuthHeaderInput[];
  oauth?: McpOAuthInput;
  enabled: boolean;
}

/** One write-only static header entry from the settings form. */
export interface McpHttpAuthHeaderInput {
  name: string;
  /** Write-only; absent preserves the matching existing credential. */
  secret?: string;
}

/** Write-only OAuth configuration from the settings form. */
export interface McpOAuthInput {
  issuer: string;
  /** Empty means registration_endpoint from discovery will provision a public client. */
  clientId: string;
  /** Absent means `authorization_code`, the stored default for rows written before it. */
  flow?: McpOAuthFlow;
  /** Absent means `none`: a public client that sends only its client id. */
  clientAuth?: McpOAuthClientAuth;
  /**
   * Write-only, like an HTTP auth header secret: absent preserves the stored secret, and
   * switching `clientAuth` to `none` deletes it.
   */
  clientSecret?: string;
  /**
   * Ask for sender-constrained tokens (RFC 9449 DPoP). Absent means off, which is what
   * rows written before the setting mean; turning it on makes the host generate a key and
   * require `token_type: DPoP` from the server (ADR 0018).
   */
  dpop?: boolean;
  scopes: string[];
}

/** Result of a completed browser authorization flow. */
export interface McpOAuthConnectResponse {
  serverId: string;
  issuer: string;
  scopes: string[];
  tokenEndpoint: string;
}

/**
 * `mcp_oauth_cancel`: whether an in-flight flow was actually stopped.
 *
 * `false` is a real answer, not an error: by the time the user clicks cancel the flow may
 * have already finished, timed out, or never belonged to this window.
 */
export interface McpOAuthCancelResponse {
  accepted: boolean;
}

/**
 * Review is separate from save so changing a label can never silently widen the tool
 * surface. The caller must name tools that the running server actually advertised.
 */
export interface McpServerReviewInput {
  serverId: string;
  allowedTools: string[];
  trustLevel: "reviewed" | "unreviewed";
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

export type { McpHttpAuthHeaderConfig, McpOAuthClientAuth, McpOAuthConfig, McpOAuthFlow };
