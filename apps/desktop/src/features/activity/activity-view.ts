import type { Activity, ToolExecutionRecord } from "@yukinal/shared";

export interface ActivityDayGroup {
  key: string;
  label: string;
  activities: Activity[];
}

const DAY_FORMAT = new Intl.DateTimeFormat("zh-CN", { month: "long", day: "numeric" });
const YEAR_DAY_FORMAT = new Intl.DateTimeFormat("zh-CN", { year: "numeric", month: "long", day: "numeric" });
const TIME_FORMAT = new Intl.DateTimeFormat("zh-CN", { hour: "2-digit", minute: "2-digit", hour12: false });

export function isActivityIssue(activity: Activity): boolean {
  return activity.outcome === "failure" || activity.outcome === "denied";
}

/** Calendar groups follow the user's local day, including daylight-saving changes. */
export function groupActivitiesByDay(activities: readonly Activity[], now = new Date()): ActivityDayGroup[] {
  const yesterday = new Date(now);
  yesterday.setDate(yesterday.getDate() - 1);
  const groups = new Map<string, ActivityDayGroup>();
  const sorted = [...activities].sort((left, right) => timestamp(right.createdAt) - timestamp(left.createdAt));
  for (const activity of sorted) {
    const date = new Date(activity.createdAt);
    const valid = !Number.isNaN(date.getTime());
    const key = valid ? dayKey(date) : "unknown";
    let group = groups.get(key);
    if (!group) {
      const label = !valid ? "时间未知" : key === dayKey(now) ? "今天" : key === dayKey(yesterday) ? "昨天"
        : (date.getFullYear() === now.getFullYear() ? DAY_FORMAT : YEAR_DAY_FORMAT).format(date);
      group = { key, label, activities: [] };
      groups.set(key, group);
    }
    group.activities.push(activity);
  }
  return [...groups.values()];
}

export function activityClock(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? "—" : TIME_FORMAT.format(date);
}

function dayKey(date: Date): string {
  return `${date.getFullYear()}-${date.getMonth() + 1}-${date.getDate()}`;
}

function timestamp(value: string): number {
  const time = new Date(value).getTime();
  return Number.isNaN(time) ? -Infinity : time;
}

/** Keep uncertainty visible in the preview; raw payloads belong to the evidence page. */
export function activityExecutionSummary(execution: ToolExecutionRecord): string {
  const output = execution.output;
  const record = output && typeof output === "object" && !Array.isArray(output) ? output as Record<string, unknown> : null;
  const effect = record?.executionState;
  const warning = effect === "result_unknown" ? "执行结果未知" : effect === "timed_out" ? "执行超时，效果可能已发生" : effect === "cancelled" ? "执行已取消，效果可能已发生" : null;
  const summary = execution.error || (typeof record?.summary === "string" ? record.summary : typeof output === "string" ? output : "");
  return [warning, summary].filter(Boolean).join(" · ") || "查看输入与完整结果";
}
