/**
 * System logical event names. These are the *only* event types the UI may
 * subscribe to and the only names Rust may emit; adding one means updating this map.
 *
 * Rust -> UI events travel over Tauri's event system; agent -> UI events are
 * mapped from `AgentStreamEvent` onto the same names so the UI has one code path.
 *
 * These are logical names: `tauriEventName` below is the single place that turns
 * them into the channel Tauri actually accepts.
 */

import type { Activity } from "../types/activity.js";
import type { AgentStreamEvent } from "../types/chat.js";
import type { ServerStatus } from "../types/server.js";

export const EVENT_NAMES = [
  "server.connected",
  "server.disconnected",
  "server.updated",
  "server.auth_challenge",
  "mcp.oauth_device_code",
  "agent.started",
  "agent.thinking",
  "agent.text",
  "agent.usage",
  "agent.tool_call",
  "agent.tool_result",
  "agent.waiting_approval",
  "agent.approval_expired",
  "agent.completed",
  "agent.failed",
  "terminal.opened",
  "terminal.data",
  "terminal.closed",
  "activity.created",
] as const;

export type EventName = (typeof EVENT_NAMES)[number];

/**
 * Convert a logical event name to the channel accepted by Tauri.
 *
 * Payload discriminators intentionally keep their dotted names (for example,
 * `agent.started`), while Tauri event channels only allow `:`, `/`, `_`, `-`
 * and alphanumeric characters.
 */
export function tauriEventName(name: string): string {
  return name.replace(/\./g, ":");
}

export interface ServerConnectedEvent {
  serverId: string;
  status: Extract<ServerStatus, "connected">;
  at: string;
}

export interface ServerDisconnectedEvent {
  serverId: string;
  reason: "user" | "transport" | "keepalive" | "error";
  at: string;
}

export interface ServerUpdatedEvent {
  serverId: string;
  status: ServerStatus;
  capabilitiesChanged: boolean;
  at: string;
}

export interface ServerAuthPrompt {
  prompt: string;
  /** `false` requests a non-echoed secret; the UI must mask this input. */
  echo: boolean;
}

/** A server-issued second-factor challenge awaiting one bounded UI response. */
export interface ServerAuthChallengeEvent {
  authId: string;
  serverId: string;
  username: string;
  host: string;
  name: string;
  instructions: string;
  prompts: ServerAuthPrompt[];
  expiresAt: string;
}

/**
 * One in-flight RFC 8628 device authorization, announced so the UI can show the
 * `user_code` and the verification link while the host polls.
 *
 * This is a *display* event, not a prompt: there is nothing for the user to answer
 * here. The terminal state arrives as the result of the `mcp_oauth_connect` call that
 * is already waiting, and the flow is stopped with `mcp_oauth_cancel`.
 *
 * `userCode` and the verification URLs are remote text from the authorization server.
 * The UI may show them and hand the URL to the OS opener; it must not parse them into
 * anything else.
 */
export interface McpOAuthDeviceCodeEvent {
  serverId: string;
  userCode: string;
  /** Where the user types the code. Never contains the code itself. */
  verificationUri: string;
  /** Same URL with the code already embedded; preferred when the server sends it. */
  verificationUriComplete?: string;
  expiresAt: string;
}

export interface TerminalOpenedEvent {
  terminalSessionId: string;
  serverId: string;
  cols: number;
  rows: number;
}

/** Stream bytes: the only per-keystroke channel. Payload `data` is UTF-8 (MVP). */
export interface TerminalDataEvent {
  terminalSessionId: string;
  data: string;
}

export interface TerminalClosedEvent {
  terminalSessionId: string;
  exitCode: number | null;
}

/** UI-facing envelope: every event on the wire is one of these. */
export type YukinalEvent =
  | { name: "server.connected"; payload: ServerConnectedEvent }
  | { name: "server.disconnected"; payload: ServerDisconnectedEvent }
  | { name: "server.updated"; payload: ServerUpdatedEvent }
  | { name: "server.auth_challenge"; payload: ServerAuthChallengeEvent }
  | { name: "mcp.oauth_device_code"; payload: McpOAuthDeviceCodeEvent }
  | { name: "terminal.opened"; payload: TerminalOpenedEvent }
  | { name: "terminal.data"; payload: TerminalDataEvent }
  | { name: "terminal.closed"; payload: TerminalClosedEvent }
  | { name: "activity.created"; payload: Activity }
  | { name: "agent.started" | "agent.thinking" | "agent.text" | "agent.usage" | "agent.tool_call" | "agent.tool_result" | "agent.waiting_approval" | "agent.approval_expired" | "agent.completed" | "agent.failed"; payload: AgentStreamEvent };
