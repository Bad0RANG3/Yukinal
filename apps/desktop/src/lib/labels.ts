/**
 * Display vocabulary for the enums the UI shows constantly: server environment and
 * status, plus the risk / decision / approval words the audit trail is written in.
 *
 * These live in one module because they were previously copy-pasted, and the copies
 * had already drifted: `staging` read "预发" in the sidebar and project list but
 * "预发布" in the add-server form and "预发布环境" on the badge, so the same server
 * was described two ways on one screen. Likewise `error` read "错误" in the list and
 * "连接异常" in the header.
 *
 * The risk vocabulary earned its place here the same way. `features/agent/transcript.ts`
 * and `features/activity/ActivityFeed.tsx` each rendered the risk level, the permission
 * decision, who approved a call, and how it ended — four maps, defined twice. Besides
 * being a sibling-feature import in either direction, that pair is a safety story: the
 * Agent panel and the audit log are two views of one decision about a production write,
 * and if they disagree about whether it was "需审批" or "自动批准", the disagreement is
 * invisible in review and only shows up as a contradiction on screen.
 *
 * Every map is an exhaustive `Record` over the shared enum, so adding a member to
 * `Environment`, `Server["status"]`, `RiskLevel` or `PermissionMode` is a compile error
 * here rather than a silently missing label.
 *
 * Environment is not decoration in this app — it gates write auto-approval (see
 * `features/agent/run-mode.ts`) — so a label that disagrees with itself is a
 * safety story that disagrees with itself.
 */

import type {
  Environment,
  PermissionApprovalSource,
  PermissionMode,
  RiskLevel,
  Server,
  ToolExecutionRecord,
} from "@yukinal/shared";

/** Full form, for badges and sentences: "生产环境". */
export const ENVIRONMENT_LABEL: Record<Environment, string> = {
  production: "生产环境",
  staging: "预发布环境",
  development: "开发环境",
  local: "本地环境",
  unknown: "未知环境",
};

/** Compact form, for sidebars and select options where space is tight. */
export const ENVIRONMENT_LABEL_SHORT: Record<Environment, string> = {
  production: "生产",
  staging: "预发布",
  development: "开发",
  local: "本地",
  unknown: "未知",
};

/** Declaration order, for rendering a picker. */
export const ENVIRONMENTS: readonly Environment[] = [
  "local",
  "development",
  "staging",
  "production",
  "unknown",
];

export const SERVER_STATUS_LABEL: Record<Server["status"], string> = {
  connecting: "连接中",
  connected: "已连接",
  disconnected: "未连接",
  error: "连接异常",
};

export function environmentLabel(environment: Environment): string {
  return ENVIRONMENT_LABEL[environment];
}

export function environmentLabelShort(environment: Environment): string {
  return ENVIRONMENT_LABEL_SHORT[environment];
}

/* ── 审计与审批词汇 ─────────────────────────────────────────────────────────
 *
 * 这四个概念同属一条链路：一次工具调用有多危险（risk）、策略怎么判（decision）、
 * 谁批的（approvedBy）、最后什么结果（status）。它们分别由 Agent 面板的动态流
 * 和活动页的审计表格渲染，必须是同一套词。
 */

export const RISK_LEVEL_LABEL: Record<RiskLevel, string> = {
  read: "只读",
  low: "低",
  medium: "中",
  high: "高",
  critical: "严重",
};

/** 策略对一次调用的判定。`deny` 是「策略禁止」，不是「没人审批」—— 它不可推翻。 */
export const DECISION_LABEL: Record<PermissionMode, string> = {
  auto: "自动批准",
  ask: "需审批",
  deny: "策略禁止",
};

/** 谁按下了批准。三者含义完全不同，所以措辞必须能区分开。 */
export const APPROVAL_SOURCE_LABEL: Record<PermissionApprovalSource, string> = {
  user: "用户批准",
  policy: "策略批准",
  agent: "Agent 自主批准",
};

/** 一次工具执行的完整状态。比「结果」多出「还没结束」的三种。 */
export const EXECUTION_STATUS_LABEL: Record<ToolExecutionRecord["status"], string> = {
  pending: "排队",
  running: "执行中",
  waiting_approval: "等待审批",
  success: "成功",
  failed: "失败",
  cancelled: "已取消",
};

/** 终结态的结果词。是 `EXECUTION_STATUS_LABEL` 的子集，用同一套措辞。 */
export function resultLabel(status: "success" | "failed" | "cancelled"): string {
  return EXECUTION_STATUS_LABEL[status];
}

export function riskLabel(level: RiskLevel): string {
  return RISK_LEVEL_LABEL[level];
}

export function decisionLabel(decision: PermissionMode): string {
  return DECISION_LABEL[decision];
}

export function approvalSourceLabel(source: PermissionApprovalSource): string {
  return APPROVAL_SOURCE_LABEL[source];
}
