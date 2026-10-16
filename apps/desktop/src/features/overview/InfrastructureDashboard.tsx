import { useQuery, useQueryClient } from "@tanstack/react-query";
import { IPC_COMMANDS, type Activity } from "@yukinal/shared";
import { useEffect } from "react";

import { Icon } from "../../components/Icon.js";
import { errorMessage, formatRelativeTime, formatTimestamp } from "../../lib/format.js";
import { callDesktop, isDesktopShell, subscribeDesktop } from "../../lib/ipc.js";
import { useServers } from "../../lib/servers.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { scheduleIntervalLabel } from "../investigations/schedule-ui.js";

const OUTCOMES = { success: "成功", failure: "失败", cancelled: "已取消", denied: "已拒绝" };
const SCHEDULE_STATES = { active: "已启用", paused: "已暂停", revoked: "已撤销" };

export function InfrastructureDashboard() {
  const shell = isDesktopShell();
  const queryClient = useQueryClient();
  const servers = useServers();
  const setPrimary = useWorkspaceStore((state) => state.setPrimary);
  const selectTask = useWorkspaceStore((state) => state.selectTask);
  const activities = useQuery({
    queryKey: ["activities", "all"], enabled: shell,
    queryFn: async () => (await callDesktop(IPC_COMMANDS.activityList, { limit: 100 })).activities,
  });
  const tasks = useQuery({
    queryKey: ["investigationTasks"], enabled: shell,
    queryFn: () => callDesktop("investigation_task_list", {}),
  });
  const schedules = useQuery({
    queryKey: ["investigationSchedules"], enabled: shell,
    queryFn: () => callDesktop("investigation_schedule_list", {}), refetchInterval: 60_000,
  });
  useEffect(() => {
    if (!shell) return;
    const subscription = subscribeDesktop("activity.created", (activity) => {
      queryClient.setQueryData<Activity[]>(["activities", "all"], (current) => current
        ? [activity, ...current.filter((item) => item.id !== activity.id)].slice(0, 100) : current);
    });
    return subscription.stop;
  }, [shell, queryClient]);
  const recent = [...(activities.data ?? [])].sort((a, b) => Date.parse(b.createdAt) - Date.parse(a.createdAt));
  const scheduled = [...(schedules.data?.schedules ?? [])].sort((a, b) =>
    Number(b.status === "active") - Number(a.status === "active") || Date.parse(a.nextRunAt) - Date.parse(b.nextRunAt));
  const serverNames = new Map((servers.data ?? []).map((server) => [server.id, server.name]));
  const taskNames = new Map((tasks.data?.tasks ?? []).map((task) => [task.id, task.objective]));
  const refreshing = activities.isFetching || schedules.isFetching || tasks.isFetching;

  return <section className="workspace-dashboard" aria-label="仪表台">
    <section className="dashboard-section" aria-label="最新记录">
      <header><h3>最新记录</h3><button type="button" className="icon-button" aria-label="刷新仪表台" title="刷新" disabled={!shell || refreshing} onClick={() => { void activities.refetch(); void schedules.refetch(); void tasks.refetch(); }}><Icon name="refresh" size="sm" /></button><button type="button" className="text-button" onClick={() => setPrimary("activity")}>全部<Icon name="chevronRight" size="xs" /></button></header>
      {activities.isError ? <p className="dashboard-query-error" role="alert">读取失败：{errorMessage(activities.error)}</p> : null}
      {activities.isLoading ? <p className="dashboard-placeholder" role="status">正在读取…</p> : null}
      {activities.isSuccess && !recent.length ? <p className="dashboard-placeholder">暂无记录</p> : null}
      <div className="dashboard-records">{recent.slice(0, 4).map((activity) => <details key={activity.id} className="dashboard-record">
        <summary><span className="dashboard-record-copy"><strong>{activity.title}</strong><small>{activity.serverId ? serverNames.get(activity.serverId) ?? activity.serverId : "全局"}<span> · </span><time dateTime={activity.createdAt} title={formatTimestamp(activity.createdAt)}>{formatRelativeTime(activity.createdAt)}</time></small></span><span className={`dashboard-outcome outcome-${activity.outcome ?? "unknown"}`}>{activity.outcome ? OUTCOMES[activity.outcome] : "记录"}</span></summary>
        <p>{activity.description ?? activity.reason ?? activity.title}</p>
      </details>)}</div>
    </section>
    <section className="dashboard-section" aria-label="定时任务">
      <header><h3>定时任务</h3><button type="button" className="text-button" onClick={() => setPrimary("tasks")}>全部<Icon name="chevronRight" size="xs" /></button></header>
      {schedules.isError ? <p className="dashboard-query-error" role="alert">读取失败：{errorMessage(schedules.error)}</p> : null}
      {tasks.isError ? <p className="dashboard-query-error" role="status">任务名称读取失败：{errorMessage(tasks.error)}</p> : null}
      {schedules.isLoading ? <p className="dashboard-placeholder" role="status">正在读取…</p> : null}
      {schedules.isSuccess && !scheduled.length ? <p className="dashboard-placeholder">暂无定时任务</p> : null}
      <div className="dashboard-schedules">{scheduled.slice(0, 3).map((schedule) => <button type="button" className="dashboard-schedule" key={schedule.id} title={taskNames.get(schedule.taskId) ?? schedule.taskId} onClick={() => { selectTask(schedule.taskId); setPrimary("tasks"); }}>
        <span className="dashboard-record-copy"><strong>{taskNames.get(schedule.taskId) ?? "定时巡检"}</strong><small>{scheduleIntervalLabel(schedule.intervalSeconds)} · {SCHEDULE_STATES[schedule.status]}</small><small>{schedule.status === "active" ? Date.parse(schedule.nextRunAt) <= Date.now() ? <span title={`计划时间：${formatTimestamp(schedule.nextRunAt)}`}>等待调度</span> : <>下次 <time dateTime={schedule.nextRunAt}>{formatTimestamp(schedule.nextRunAt)}</time></> : schedule.lastRunAt ? <>上次 {formatTimestamp(schedule.lastRunAt)}</> : "尚未执行"}</small></span>
        <Icon name="chevronRight" size="sm" />
        {schedule.lastError ? <span className="dashboard-schedule-error" title={schedule.lastError}>{schedule.lastError}</span> : schedule.lastOutcome ? <span className="dashboard-schedule-result" title={schedule.lastOutcome}>{schedule.lastOutcome}</span> : null}
      </button>)}</div>
    </section>
  </section>;
}
