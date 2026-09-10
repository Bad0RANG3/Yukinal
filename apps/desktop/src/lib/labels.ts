/**
 * Display vocabulary for the two enums the UI shows constantly: server
 * environment and server status.
 *
 * These live in one module because they were previously copy-pasted six times,
 * and the copies had already drifted: `staging` read "预发" in the sidebar and
 * project list but "预发布" in the add-server form and "预发布环境" on the badge,
 * so the same server was described two ways on one screen. Likewise `error` read
 * "错误" in the list and "连接异常" in the header.
 *
 * Every map is an exhaustive `Record` over the shared enum, so adding a member to
 * `Environment` or `Server["status"]` is a compile error here rather than a
 * silently missing label.
 *
 * Environment is not decoration in this app — it gates write auto-approval (see
 * `features/agent/run-mode.ts`) — so a label that disagrees with itself is a
 * safety story that disagrees with itself.
 */

import type { Environment, Server } from "@yukinal/shared";

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
