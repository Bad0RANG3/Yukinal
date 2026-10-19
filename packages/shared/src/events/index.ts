/**
 * System logical event names — the declared event vocabulary.
 *
 * These names **are** the channel names. The C++ host keys its subscriber map on the
 * exact string the UI passes to `listen`, and `BridgeContext::publish_event`
 * (`native/desktop_host/src/main.cpp:67-71`) hands the emitter's own string straight to
 * `__YUKINAL_HOST__.dispatchEvent(event, payload)`; a sidecar notification is routed
 * under `params.type` unchanged (`native/desktop_host/src/ipc_router.cpp:8988-8990`).
 * There is no separator rewriting between this list and the wire, so a name here is a
 * name the UI can subscribe to verbatim.
 *
 * The vocabulary is *not* a producer census. Several names are declared with no emitter
 * anywhere in this repository: see the annotations on each entry below, and
 * `schemas/event-vocabulary.test.ts`, which records which names have a producer, which
 * have a subscriber, and which have neither. Keeping a declared name with no producer is
 * a decision (a reserved name for a capability that is not implemented yet) and is
 * written down as one rather than left to be discovered.
 */

import type { Activity } from "../types/activity.js";
import type { AgentStreamEvent } from "../types/chat.js";
import type { ServerStatus } from "../types/server.js";
import type { InvestigationNotificationPolicy } from "../types/schedule.js";
import type { LocalFileDropEvent, TransferSnapshot } from "../types/file.js";

/*
 * Per-name producer state, verified by literal scan of `native/**`, `agent/src/**` (the
 * only two emitters) and recorded here because a declared-but-unemitted name used to be
 * indistinguishable from a live one. "No producer" is a state, not an oversight, and each
 * one below says why it is kept. `schemas/event-vocabulary.test.ts` pins the partition.
 */
export const EVENT_NAMES = [
  // 无生产者：`server_connect` / `server_disconnect` 只改数据库里的状态
  // （`ipc_router.cpp:8837`、`:8807`），从不发事件。保留是因为宿主已经有这两个命令，
  // 补发事件是增量改动；删掉会让将来接线变成契约破坏。
  "server.connected",
  "server.disconnected",
  // 无生产者：全仓库没有任何 emit，`consistency.ts` 曾断言「Rust 在发」是错的。
  "server.updated",
  // 无生产者：宿主侧不存在 `auth_prompt`/`auth_challenge` 字面量，`server_auth_respond`
  // 也没有把回答写回 PTY 的路径；UI 的挑战弹窗（`ServerAuthChallengeModal.tsx:26`）
  // 因此在等到超时之前什么都收不到。空洞的通道见 C-1/C-2 —— 要么补生产者，要么连同
  // 弹窗一起撤掉，两种都要显式决定，不能维持现状。
  "server.auth_challenge",
  "mcp.oauth_device_code",
  "agent.started",
  // 无生产者：Python sidecar 只会发 started/text/tool_call/tool_result/waiting_approval/
  // approval_expired/completed/failed（`sidecar.py:1019` 转发 `params.type`），没有
  // thinking/usage。UI 有分支（`useAgentRun.ts`），所以这是「界面等待一个从不出现的
  // 增量」，保留是为了让补发成为增量改动，而不是让 UI 的既有分支变成死代码。
  "agent.thinking",
  "agent.text",
  // 无生产者：同上，`agent.usage` 在 sidecar 里没有任何 emit。
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
  // 无生产者：`native/` 里没有 `output_lost`，终端的广播通道也没有任何 `Lagged`
  // 处理，所以「丢帧要告知用户」这条语义目前只存在于类型和 UI 文案里。
  "terminal.output_lost",
  // 无生产者：宿主写入 `activity_list` 表，但从不发这条事件。UI 有 4 个订阅者
  // （ActivityFeed / InfrastructureDashboard / ServerOverview / useWorkpageOperations），
  // 它们各自轮询兜底，所以缺的不是消费者而是生产者。
  "activity.created",
  // 无生产者：调度器只在 `investigation_schedule_*` 的响应里回话，从不推事件；
  // `InvestigationsPane.tsx:102` 的订阅目前收不到任何东西。
  "investigation.schedule_notification",
  "file.local_dropped",
  "file.transfer_updated",
] as const;

export type EventName = (typeof EVENT_NAMES)[number];

/**
 * The channel name the host expects for a logical event name.
 *
 * It is the identity function, and that is the point: the C++ host keys subscriptions on
 * the exact string it is given and `publish_event` passes the emitter's own string to
 * `dispatchEvent`, so a name in `EVENT_NAMES` is already the channel. This exists so the
 * one place that used to *rewrite* the name (a `.`→`:` substitution inherited from a
 * retired event system) is visible as a deliberate no-op rather than something callers
 * have to remember not to do.
 */
export function eventChannelName(name: EventName): EventName {
  return name;
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

/**
 * A broadcast gap: the UI fell behind and `missedEvents` terminal frames were dropped
 * before it could read them.
 *
 * It is deliberately **not** per-session (the host's broadcast channel is global) and
 * deliberately not an error: losing output under a flood is expected, and the honest
 * response is to say so rather than to silently stop forwarding forever (ADR: the
 * terminal must survive `Lagged` instead of ending its forwarding task).
 */
export interface TerminalOutputLostEvent {
  missedEvents: number;
}

export interface TerminalClosedEvent {
  terminalSessionId: string;
  exitCode: number | null;
}

/** A scheduler notification that survived the configured policy gate. */
export interface InvestigationScheduleNotificationEvent {
  scheduleId: string;
  scheduleRunId: string;
  taskId: string;
  outcome: "changed" | "failed" | "baseline" | "no_change" | "insufficient_evidence";
  notificationPolicy: InvestigationNotificationPolicy;
  title: string;
  evidenceIds: string[];
  findingId?: string;
  briefId?: string;
  at: string;
}

export type { LocalFileDropEvent };

export interface FileTransferUpdatedEvent {
  transfer: TransferSnapshot;
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
  | { name: "terminal.output_lost"; payload: TerminalOutputLostEvent }
  | { name: "activity.created"; payload: Activity }
  | { name: "investigation.schedule_notification"; payload: InvestigationScheduleNotificationEvent }
  | { name: "file.local_dropped"; payload: LocalFileDropEvent }
  | { name: "file.transfer_updated"; payload: FileTransferUpdatedEvent }
  | { name: "agent.started" | "agent.thinking" | "agent.text" | "agent.usage" | "agent.tool_call" | "agent.tool_result" | "agent.waiting_approval" | "agent.approval_expired" | "agent.completed" | "agent.failed"; payload: AgentStreamEvent };
