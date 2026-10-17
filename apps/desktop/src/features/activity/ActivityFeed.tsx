import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  IPC_COMMANDS,
  type Activity,
  type ActivityType,
  type Server,
  type ToolExecutionRecord,
} from "@yukinal/shared";
import { useEffect, useId, useState } from "react";

import { errorMessage, formatTimestamp } from "../../lib/format.js";
import { callDesktop, isDesktopShell, subscribeDesktop } from "../../lib/ipc.js";
import { EXECUTION_STATUS_LABEL } from "../../lib/labels.js";
import { useWorkpagesStore } from "../workpages/workpages-store.js";
import { useServers } from "../../lib/servers.js";
import { EmptyPanel, ErrorPanel, LoadingPanel, PreviewEmpty } from "../../components/PanelStates.js";
import { Icon, type IconName } from "../../components/Icon.js";
import { usePresence } from "../../hooks/usePresence.js";
import { PageToolbar } from "../../components/PageToolbar.js";
import { activityClock, activityExecutionSummary, groupActivitiesByDay, isActivityIssue } from "./activity-view.js";

const ACTIVITY_TYPE_META: Record<ActivityType, { label: string; icon: IconName }> = {
  connection: { label: "连接", icon: "connect" },
  authentication: { label: "认证", icon: "settings" },
  configuration: { label: "配置", icon: "settings" },
  deployment: { label: "部署", icon: "arrowUp" },
  service: { label: "服务", icon: "services" },
  container: { label: "容器", icon: "servers" },
  file_change: { label: "文件", icon: "file" },
  agent_action: { label: "运行", icon: "activity" },
  approval: { label: "审批", icon: "warning" },
  health: { label: "健康", icon: "activity" },
};

const OUTCOME_LABEL = {
  success: "成功",
  failure: "失败",
  cancelled: "已取消",
  denied: "已拒绝",
} as const;

export function ActivityFeed({ serverId }: { serverId?: string | null }) {
  const feedId = useId();
  const [filter, setFilter] = useState<"all" | "issues">("all");
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
    // 「退订函数比清理晚到」这件事由 `subscribeDesktop` 负责；这里只剩业务逻辑。
    const subscription = subscribeDesktop("activity.created", (activity) => {
      if (scoped && activity.serverId !== serverId) return;
      queryClient.setQueryData<Activity[]>(activityQueryKey, (current) => {
        if (!current) return current;
        return [activity, ...current.filter((item) => item.id !== activity.id)].slice(0, scoped ? 50 : 100);
      });
    });
    return subscription.stop;
  }, [queryClient, scoped, serverId, shell]);
  const servers = useServers({ enabled: shell && !scoped });

  if (!shell) return <PreviewEmpty body="桌面功能不可用" />;
  if (scoped && !serverId) return <EmptyPanel title="选择一台服务器" />;

  const rows = activities.data ?? [];
  const issues = rows.filter(isActivityIssue);
  const groups = groupActivitiesByDay(filter === "issues" ? issues : rows);
  const serverNames = new Map((servers.data ?? []).map((server: Server) => [server.id, server.name]));
  return (
    <section className="activity-page" aria-label="动态时间流">
      <PageToolbar actions={
        <button type="button" className="icon-button activity-refresh" onClick={() => void activities.refetch()} disabled={activities.isFetching} title="刷新动态" aria-label="刷新动态"><Icon name="refresh" size="sm" /></button>
      }><div className="activity-filters" role="group" aria-label="动态筛选">
        <button type="button" aria-pressed={filter === "all"} onClick={() => setFilter("all")}>全部<span>{rows.length}</span></button>
        <button type="button" aria-pressed={filter === "issues"} onClick={() => setFilter("issues")}>异常<span>{issues.length}</span></button>
      </div></PageToolbar>
      {activities.isError ? (
        <ErrorPanel
          title="无法读取动态"
          message={errorMessage(activities.error)}
          onRetry={() => void activities.refetch()}
        />
      ) : null}
      {activities.isLoading ? <LoadingPanel title="正在读取动态" /> : null}
      {!activities.isLoading && !activities.isError && rows.length === 0 ? <EmptyPanel icon="activity" title="暂无动态" /> : null}
      {rows.length && !groups.length ? <EmptyPanel icon="check" title="暂无异常" /> : null}
      {groups.length ? <div className="activity-timeline">{groups.map((group) => <section className="activity-day" key={group.key} aria-labelledby={`${feedId}-${group.key}`}>
        <h3 id={`${feedId}-${group.key}`}>{group.label}</h3>
        <ol className="activity-events">{group.activities.map((activity) => <li key={activity.id}><ActivityRow activity={activity} serverName={!scoped && activity.serverId ? serverNames.get(activity.serverId) ?? activity.serverId : undefined} scoped={scoped} /></li>)}</ol>
      </section>)}</div> : null}
    </section>
  );
}

function ActivityRow({ activity, serverName, scoped }: { activity: Activity; serverName?: string; scoped: boolean }) {
  const [expanded, setExpanded] = useState(false);
  const detailId = useId();
  const traceId = activity.traceId;
  const executions = useQuery({
    queryKey: ["tool-executions", traceId],
    enabled: expanded && Boolean(traceId),
    queryFn: async () =>
      // `callDesktop` binds this command to the response schema registered in
      // `IPC_SCHEMAS`, so the command→schema pair has exactly one home.
      callDesktop(IPC_COMMANDS.toolExecutionList, { traceId: traceId as string, limit: 50 }),
  });
  const meta = ACTIVITY_TYPE_META[activity.type];
  const outcome = activity.outcome ? OUTCOME_LABEL[activity.outcome] : null;
  const target = serverName ?? (scoped ? meta.label : activity.serverId ? activity.serverId : "全局");
  const detailPresence = usePresence(expanded, { exitAnimation: "expand-exit" });
  return (
    <article className={`activity-event ${expanded ? "is-expanded" : ""}`}>
      <button type="button" className="activity-event-toggle" aria-expanded={expanded} aria-controls={detailPresence.mounted ? detailId : undefined} aria-label={`${expanded ? "收起" : "查看"}动态：${activity.title}${outcome ? `，${outcome}` : ""}，${target}`} onClick={() => setExpanded((current) => !current)}>
        <span className={`activity-event-mark activity-outcome-${activity.outcome ?? "none"}`} aria-hidden="true"><Icon name={isActivityIssue(activity) ? "warning" : meta.icon} size="sm" /></span>
        <span className="activity-event-copy">
          <span className="activity-event-heading"><strong title={activity.title}>{activity.title}</strong>{outcome ? <span className={`activity-outcome activity-outcome-${activity.outcome}`}>{outcome}</span> : null}</span>
          <span className="activity-event-meta"><span title={target}>{target}</span><time dateTime={activity.createdAt} title={formatTimestamp(activity.createdAt)}>{activityClock(activity.createdAt)}</time></span>
        </span>
        <span className="activity-event-chevron" aria-hidden="true"><Icon name={expanded ? "chevronDown" : "chevronRight"} size="xs" /></span>
      </button>
      {detailPresence.mounted ? (
          <div
            id={detailId}
            className={`activity-trace-detail ${detailPresence.closing ? "is-closing" : ""}`}
            aria-label={`${activity.title}的详情`}
            aria-hidden={detailPresence.closing || undefined}
            onAnimationEnd={detailPresence.onAnimationEnd}
          >
            {activity.description ? <p>{activity.description}</p> : null}
            {activity.reason && activity.reason !== activity.description ? <p><span className="activity-detail-label">原因</span>{activity.reason}</p> : null}
            <dl className="activity-detail-facts">
              <div><dt>类型</dt><dd>{meta.label}</dd></div>
              <div><dt>执行者</dt><dd>{actorLabel(activity.actor)}</dd></div>
              <div><dt>时间</dt><dd><time dateTime={activity.createdAt}>{formatTimestamp(activity.createdAt)}</time></dd></div>
            </dl>
            {traceId ? <section className="activity-trace-steps" aria-label="工具执行步骤">
              <h4>执行步骤</h4>
              {executions.isLoading ? <span className="muted-copy">正在读取步骤…</span> : null}
              {executions.isError ? <div className="activity-trace-error"><span className="error-copy">无法读取步骤：{errorMessage(executions.error)}</span><button type="button" className="text-button" onClick={() => void executions.refetch()}>重试</button></div> : null}
              {executions.data?.executions.map((execution) => <ExecutionStep key={`${execution.traceId}:${execution.stepId}`} execution={execution} actor={actorLabel(activity.actor)} target={serverName ?? activity.serverId ?? "全局"} />)}
              {executions.data && executions.data.executions.length === 0 ? <span className="muted-copy">暂无已保存步骤</span> : null}
            </section> : null}
          </div>
        ) : null}
    </article>
  );
}

function ExecutionStep({ execution, actor, target }: { execution: ToolExecutionRecord; actor: string; target: string }) {
  const openEvidence = useWorkpagesStore((state) => state.openEvidence);
  const summary = activityExecutionSummary(execution);
  return <button type="button" className="activity-trace-step" aria-label={`查看执行记录：${execution.toolName}`} title="在中间工作页查看完整记录" onClick={() => openEvidence({ id: `${execution.traceId}:${execution.stepId}`, title: execution.toolName, actor, target, execution })}>
    <span className="activity-trace-step-top"><strong>{execution.toolName}</strong><span className={`activity-execution-status activity-execution-status-${execution.status}`}>{EXECUTION_STATUS_LABEL[execution.status]}</span></span>
    <span className={`activity-step-summary ${execution.error ? "is-error" : ""}`} title={summary}>{summary}</span>
    <span className="activity-step-open">完整记录<Icon name="chevronRight" size="xs" /></span>
  </button>;
}

function actorLabel(actor: string): string {
  if (actor === "user") return "用户";
  if (actor === "collector") return "采集器";
  if (actor === "core") return "Core";
  return actor;
}
