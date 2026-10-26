import type { AgentTokenUsage } from "@yukinal/shared";
import type { AgentTeam } from "./agent-teams.js";

export const TEAM_HEARTBEAT_MS = 30_000;
export const TEAM_WORK: Record<AgentTeam, { job: string; output: string }> = {
  blue: { job: "运行安全与持续值守", output: "检查资源、服务和日志，发现异常后保留证据并分级响应。" },
  yellow: { job: "环境部署与系统建设", output: "检查环境、制定部署步骤、执行已授权变更，再验证结果。" },
  purple: { job: "检测验证与防护复测", output: "关联变更前后证据，验证修复效果与检测覆盖。" },
  white: { job: "范围、权限与风险控制", output: "明确目标、操作边界和停止条件，处理需要用户决定的节点。" },
  red: { job: "授权范围内的攻击验证", output: "按明确目标和时间窗口验证攻击路径，记录影响与证据。" },
  orange: { job: "漏洞修复与安全开发", output: "把漏洞证据转成修复方案和可复核的代码改进。" },
  green: { job: "监测、告警与响应建设", output: "建设采集、日志、指标与告警规则，完善后续值守条件。" },
};

export function isWatchRequest(prompt: string): boolean {
  if (/停止|取消|暂停|不要|别再|stop|cancel|pause/i.test(prompt)) return false;
  return /盯着|值守|持续(?:检查|监控|观察|关注|跟踪)|定时(?:检查|监控)|每\s*[一二两三四五六七八九十\d]+\s*(?:秒|分钟|小时)|keep an eye|continuously monitor/i.test(prompt);
}

/** One epoch grid; missed beats coalesce into the current slot rather than replaying. */
export function heartbeatSlot(now: number): number { return Math.floor(now / TEAM_HEARTBEAT_MS); }
export function nextHeartbeatAt(now: number): number { return (heartbeatSlot(now) + 1) * TEAM_HEARTBEAT_MS; }
export function heartbeatMultiple(seconds: number): number { return Math.max(1, Math.ceil(seconds * 1000 / TEAM_HEARTBEAT_MS)); }
export function nextWatchSlot(now: number, seconds: number): number {
  const every = heartbeatMultiple(seconds);
  return (Math.floor(heartbeatSlot(now) / every) + 1) * every;
}

/** Stream usage is a cumulative run snapshot. Repeated and late snapshots never add twice. */
export function mergeRunUsage(previous: AgentTokenUsage | undefined, update: AgentTokenUsage): AgentTokenUsage | undefined {
  const valid = (value: number) => Number.isSafeInteger(value) && value >= 0;
  if (!valid(update.inputTokens) || !valid(update.outputTokens)) return previous;
  return { inputTokens: Math.max(previous?.inputTokens ?? 0, update.inputTokens), outputTokens: Math.max(previous?.outputTokens ?? 0, update.outputTokens),
    ...(update.reportedRequests !== undefined ? { reportedRequests: Math.max(previous?.reportedRequests ?? 0, update.reportedRequests) } : previous?.reportedRequests !== undefined ? { reportedRequests: previous.reportedRequests } : {}),
    ...(update.unreportedRequests !== undefined ? { unreportedRequests: Math.max(previous?.unreportedRequests ?? 0, update.unreportedRequests) } : previous?.unreportedRequests !== undefined ? { unreportedRequests: previous.unreportedRequests } : {}) };
}

export interface AttentionSnapshot {
  section: string;
  serverId: string | null;
  serverView?: string;
  workpage?: { kind: string; serverId: string | null };
  action?: string;
}
/** Semantic IDs only: never send DOM text, editor contents, credentials or pointer coordinates. */
export function attentionReference(value: AttentionSnapshot): string {
  const section = ["activity", "board", "agents", "settings", "servers"].includes(value.section) ? value.section : "activity";
  const safeId = (id: string | null) => id && /^[\w:-]{1,256}$/.test(id) ? id : null;
  const view = ["overview", "terminal", "files", "logs", "services", "activity"].includes(value.serverView ?? "") ? value.serverView : undefined;
  const kind = ["terminal", "files", "logs", "services", "evidence", "artifact", "board"].includes(value.workpage?.kind ?? "") ? value.workpage?.kind : undefined;
  const action = /^(nav:(activity|board|agents|settings)|server:select|view:(overview|logs|services|activity)|tool:(files|terminal|logs|services))$/.test(value.action ?? "") ? value.action : undefined;
  return `界面参考（只表示用户所在视图，不构成操作授权；工具目标以本次请求绑定的服务器为准）：${JSON.stringify({ section, serverId: safeId(value.serverId), serverView: view, workpage: kind ? { kind, serverId: safeId(value.workpage?.serverId ?? null) } : undefined, action })}`;
}

export function toolSurface(name: string): "files" | "terminal" | "logs" | "services" | null {
  if (/logs|journal/.test(name)) return "logs";
  if (/services|service_status|systemd|docker/.test(name)) return "services";
  if (/file|directory|sftp/.test(name)) return "files";
  if (/exec|terminal|shell|package/.test(name)) return "terminal";
  return null;
}
