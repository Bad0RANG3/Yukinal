import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ActivitySchema, IPC_COMMANDS, type Activity, type ActivityType, type Server } from "@yukinal/shared";
import { listen } from "@tauri-apps/api/event";
import { useEffect } from "react";

import { callDesktop, isDesktopShell } from "../../lib/ipc.js";

const ACTIVITY_TYPE_META: Record<ActivityType, { label: string; icon: string }> = {
  connection: { label: "连接", icon: "↔" },
  authentication: { label: "认证", icon: "⌁" },
  configuration: { label: "配置", icon: "⚙" },
  deployment: { label: "部署", icon: "↑" },
  service: { label: "服务", icon: "◇" },
  container: { label: "容器", icon: "▰" },
  file_change: { label: "文件", icon: "·" },
  agent_action: { label: "Agent", icon: "✦" },
  approval: { label: "审批", icon: "!" },
  health: { label: "健康", icon: "♥" },
};

const OUTCOME_LABEL = {
  success: "成功",
  failure: "失败",
  cancelled: "已取消",
  denied: "已拒绝",
} as const;

export function ActivityFeed({ serverId }: { serverId?: string | null }) {
  const shell = isDesktopShell();
  const scoped = serverId !== undefined;
  const queryClient = useQueryClient();
  const activityQueryKey = ["activities", scoped ? serverId : "all"] as const;
  const activities = useQuery({
    queryKey: activityQueryKey,
    enabled: shell && (!scoped || Boolean(serverId)),
    queryFn: async () =>
      (
        await callDesktop(IPC_COMMANDS.activityList, scoped ? { serverId: serverId as string, limit: 50 } : { limit: 100 })
      ).activities,
  });

  useEffect(() => {
    if (!shell) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen("activity.created", (event) => {
      const parsed = ActivitySchema.safeParse(event.payload);
      if (!parsed.success || (scoped && parsed.data.serverId !== serverId)) return;
      queryClient.setQueryData<Activity[]>(activityQueryKey, (current) => {
        if (!current) return current;
        return [parsed.data, ...current.filter((item) => item.id !== parsed.data.id)].slice(0, scoped ? 50 : 100);
      });
    }).then((stop) => {
      if (disposed) stop();
      else unlisten = stop;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [queryClient, scoped, serverId, shell]);
  const servers = useQuery({
    queryKey: ["servers"],
    enabled: shell && !scoped,
    queryFn: async () => (await callDesktop(IPC_COMMANDS.serverList, {})).servers,
  });

  if (!shell) return <div className="empty-state page-empty"><h2>浏览器预览</h2><p>动态记录需要 Tauri 桌面壳中的本地数据库。</p></div>;
  if (scoped && !serverId) return <div className="empty-state page-empty"><h2>选择一台服务器</h2><p>选择服务器后查看它的连接、配置和 Agent 活动。</p></div>;

  const rows = activities.data ?? [];
  const serverNames = new Map((servers.data ?? []).map((server: Server) => [server.id, server.name]));
  return (
    <section className="activity-page">
      <div className="activity-page-header">
        <div><p className="eyebrow">审计流</p><h2>{scoped ? "服务器动态" : "全局动态"}</h2></div>
        <button type="button" className="secondary-button" onClick={() => void activities.refetch()} disabled={activities.isFetching}>↻ 刷新</button>
      </div>
      {activities.isError ? (
        <div className="error-panel"><div><strong>无法读取动态</strong><p>{activities.error instanceof Error ? activities.error.message : String(activities.error)}</p></div><button type="button" className="secondary-button" onClick={() => void activities.refetch()}>重试</button></div>
      ) : null}
      {activities.isLoading ? <div className="loading-panel"><div className="loading-spinner" /><strong>正在读取动态</strong><span>从本地审计记录加载</span></div> : null}
      {!activities.isLoading && !activities.isError && rows.length === 0 ? <div className="empty-state page-empty"><span className="empty-state-mark">◷</span><h2>暂无动态</h2><p>服务器连接、配置变更和 Agent 操作会记录在这里。</p></div> : null}
      {rows.length ? <div className="activity-list">{rows.map((activity) => <ActivityRow key={activity.id} activity={activity} serverName={activity.serverId ? serverNames.get(activity.serverId) : undefined} />)}</div> : null}
    </section>
  );
}

function ActivityRow({ activity, serverName }: { activity: Activity; serverName?: string }) {
  const meta = ACTIVITY_TYPE_META[activity.type];
  const outcome = activity.outcome ? OUTCOME_LABEL[activity.outcome] : null;
  return (
    <article className="activity-row">
      <div className={`activity-type-icon activity-type-${activity.type}`} aria-hidden="true">{meta.icon}</div>
      <div className="activity-row-body">
        <div className="activity-row-title"><strong>{activity.title}</strong>{outcome ? <span className={`activity-outcome activity-outcome-${activity.outcome}`}>{outcome}</span> : null}</div>
        <div className="activity-row-meta"><span>{meta.label}</span><span>·</span><span>{activity.actor}</span>{serverName ? <><span>·</span><span>{serverName}</span></> : null}<time dateTime={activity.createdAt}>{formatTimestamp(activity.createdAt)}</time></div>
        {activity.description || activity.reason ? <p>{activity.description ?? activity.reason}</p> : null}
      </div>
    </article>
  );
}

function formatTimestamp(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat("zh-CN", { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" }).format(date);
}
