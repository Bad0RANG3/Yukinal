import {
  INVESTIGATION_NOTIFICATION_POLICIES,
  type InvestigationNotificationPolicy,
  type InvestigationSchedule,
} from "@yukinal/shared";

export const SCHEDULE_INTERVAL_OPTIONS = [
  { seconds: 60, label: "每 1 分钟" },
  { seconds: 300, label: "每 5 分钟" },
  { seconds: 900, label: "每 15 分钟" },
  { seconds: 1_800, label: "每 30 分钟" },
  { seconds: 3_600, label: "每 1 小时" },
  { seconds: 21_600, label: "每 6 小时" },
  { seconds: 86_400, label: "每天" },
] as const;

export const NOTIFICATION_POLICY_LABEL: Record<InvestigationNotificationPolicy, string> = {
  silent: "静默",
  on_change: "仅有变化",
  always: "每次运行",
  failed_runs_only: "仅失败",
};

export const SCHEDULE_STATUS_LABEL: Record<InvestigationSchedule["status"], string> = {
  active: "运行中",
  paused: "已暂停",
  revoked: "已撤销",
};

/** Keep a persisted custom interval selectable while offering the common choices. */
export function scheduleIntervalOptions(currentSeconds: number) {
  return SCHEDULE_INTERVAL_OPTIONS.some(({ seconds }) => seconds === currentSeconds)
    ? SCHEDULE_INTERVAL_OPTIONS
    : [{ seconds: currentSeconds, label: `${scheduleIntervalLabel(currentSeconds)}（自定义）` }, ...SCHEDULE_INTERVAL_OPTIONS];
}

export function scheduleIntervalLabel(seconds: number): string {
  if (seconds % 86_400 === 0) return `每 ${seconds / 86_400} 天`;
  if (seconds % 3_600 === 0) return `每 ${seconds / 3_600} 小时`;
  if (seconds % 60 === 0) return `每 ${seconds / 60} 分钟`;
  return `每 ${seconds} 秒`;
}

export function notificationPolicyLabel(policy: InvestigationNotificationPolicy): string {
  return NOTIFICATION_POLICY_LABEL[policy];
}

export function scheduleStatusLabel(status: InvestigationSchedule["status"]): string {
  return SCHEDULE_STATUS_LABEL[status];
}

export { INVESTIGATION_NOTIFICATION_POLICIES };
