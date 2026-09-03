/**
 * System event names. These are the *only* event channels the UI may
 * subscribe to and the only names Rust may emit; adding one means updating this map.
 *
 * Rust -> UI events travel over Tauri's event system; agent -> UI events are
 * mapped from `AgentStreamEvent` onto the same names so the UI has one code path.
 */

import type { Activity } from "../types/activity.js";
import type { AgentStreamEvent } from "../types/chat.js";
import type { ServerStatus } from "../types/server.js";

export const EVENT_NAMES = [
  "server.connected",
  "server.disconnected",
  "server.updated",
  "agent.started",
  "agent.thinking",
  "agent.tool_call",
  "agent.tool_result",
  "agent.waiting_approval",
  "agent.completed",
  "agent.failed",
  "terminal.opened",
  "terminal.closed",
  "activity.created",
] as const;

export type EventName = (typeof EVENT_NAMES)[number];

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

export interface TerminalOpenedEvent {
  terminalSessionId: string;
  serverId: string;
  cols: number;
  rows: number;
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
  | { name: "terminal.opened"; payload: TerminalOpenedEvent }
  | { name: "terminal.closed"; payload: TerminalClosedEvent }
  | { name: "activity.created"; payload: Activity }
  | { name: "agent.started" | "agent.thinking" | "agent.tool_call" | "agent.tool_result" | "agent.waiting_approval" | "agent.completed" | "agent.failed"; payload: AgentStreamEvent };
