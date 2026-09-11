import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  IPC_COMMANDS,
  type Activity,
  type ActivityType,
  type Server,
  type ToolExecutionRecord,
} from "@yukinal/shared";
import { useEffect, useState } from "react";

import { errorMessage, formatTimestamp } from "../../lib/format.js";
import { callDesktop, isDesktopShell, listenDesktop } from "../../lib/ipc.js";
import {
  DECISION_LABEL,
  EXECUTION_STATUS_LABEL,
  approvalSourceLabel,
  environmentLabel,
  riskLabel,
} from "../../lib/labels.js";
import { useServers } from "../../lib/servers.js";
import { Icon, type IconName } from "../../components/Icon.js";
import { KeywordText } from "../../components/KeywordText.js";
import { usePresence } from "../../hooks/usePresence.js";

const ACTIVITY_TYPE_META: Record<ActivityType, { label: string; icon: IconName }> = {
  connection: { label: "连接", icon: "connect" },
  authentication: { label: "认证", icon: "settings" },
  configuration: { label: "配置", icon: "settings" },
  deployment: { label: "部署", icon: "arrowUp" },
  service: { label: "服务", icon: "services" },
  container: { label: "容器", icon: "servers" },
  file_change: { label: "文件", icon: "file" },
  agent_action: { label: "Agent", icon: "agent" },
  approval: { label: "审批", icon: "warning" },
  health: { label: "健康", icon: "activity" },
};

const OUTCOME_LABEL = {
  success: "成功",
  failure: "失败",
  cancelled: "已取消",
  denied: "已拒绝",
} as const;

/* `EXECUTION_STATUS_LABEL`、`DECISION_LABEL` 和本文件末尾的 `riskLabel` 都已删除：
   它们与 `features/agent/transcript.ts` 里的同名实现逐字节相同，现统一由
   `lib/labels.js` 提供。审批用词（自动批准 / 需审批 / 策略禁止）同时出现在 Agent
   面板和这张审计表里，两处各写一份就等于允许它们互相矛盾。 */


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
    void listenDesktop("activity.created", (activity) => {
      if (scoped && activity.serverId !== serverId) return;
      queryClient.setQueryData<Activity[]>(activityQueryKey, (current) => {
        if (!current) return current;
        return [activity, ...current.filter((item) => item.id !== activity.id)].slice(0, scoped ? 50 : 100);
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
  const servers = useServers({ enabled: shell && !scoped });

  if (!shell) return <div className="empty-state page-empty"><h2>浏览器预览</h2><p>动态记录需要 Tauri 桌面壳中的本地数据库。</p></div>;
  if (scoped && !serverId) return <div className="empty-state page-empty"><h2>选择一台服务器</h2><p>选择服务器后查看它的连接、配置和 Agent 活动。</p></div>;

  const rows = activities.data ?? [];
  const serverNames = new Map((servers.data ?? []).map((server: Server) => [server.id, server.name]));
  return (
    <section className="activity-page">
      <div className="activity-page-header">
        <div><p className="eyebrow">审计流</p><h2>{scoped ? "服务器动态" : "全局动态"}</h2></div>
        <button type="button" className="secondary-button" onClick={() => void activities.refetch()} disabled={activities.isFetching} title="刷新动态" aria-label="刷新动态"><Icon name="refresh" size="sm" />刷新</button>
      </div>
      {activities.isError ? (
        <div className="error-panel"><div><strong>无法读取动态</strong><p>{errorMessage(activities.error)}</p></div><button type="button" className="secondary-button" onClick={() => void activities.refetch()}>重试</button></div>
      ) : null}
      {activities.isLoading ? <div className="loading-panel"><div className="loading-spinner" /><strong>正在读取动态</strong><span>从本地审计记录加载</span></div> : null}
      {!activities.isLoading && !activities.isError && rows.length === 0 ? <div className="empty-state page-empty"><Icon name="activity" size="xl" /><h2>暂无动态</h2><p>服务器连接、配置变更和 Agent 操作会记录在这里。</p></div> : null}
      {rows.length ? <div className="activity-list">{rows.map((activity) => <ActivityRow key={activity.id} activity={activity} serverName={activity.serverId ? serverNames.get(activity.serverId) : undefined} />)}</div> : null}
    </section>
  );
}

function ActivityRow({ activity, serverName }: { activity: Activity; serverName?: string }) {
  const [expanded, setExpanded] = useState(false);
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
  // 展开/收起是一对动作，就该有一对动画。没有 presence 的话收起是瞬移：
  // 整块步骤直接消失，用户看不出它到底是「被收回去了」还是「没了」。
  const detailPresence = usePresence(expanded && Boolean(traceId), { exitAnimation: "expand-exit" });
  return (
    <article className="activity-row">
      <div className={`activity-type-icon activity-type-${activity.type}`} aria-hidden="true"><Icon name={meta.icon} size="md" /></div>
      <div className="activity-row-body">
        <div className="activity-row-title">
          <strong>{activity.title}</strong>
          {outcome ? <span className={`activity-outcome activity-outcome-${activity.outcome}`}>{outcome}</span> : null}
          {traceId ? (
            <button
              type="button"
              className="activity-detail-toggle"
              aria-expanded={expanded}
              onClick={() => setExpanded((current) => !current)}
            >
              {expanded ? "收起步骤" : "查看步骤"}
            </button>
          ) : null}
        </div>
        <div className="activity-row-meta"><span>{meta.label}</span><span>·</span><span>{actorLabel(activity.actor)}</span>{serverName ? <><span>·</span><span>{serverName}</span></> : null}<time dateTime={activity.createdAt}>{formatTimestamp(activity.createdAt)}</time></div>
        {activity.description || activity.reason ? <p>{activity.description ?? activity.reason}</p> : null}
        {detailPresence.mounted ? (
          <div
            className={`activity-trace-detail ${detailPresence.closing ? "is-closing" : ""}`}
            aria-label="工具执行步骤"
            onAnimationEnd={detailPresence.onAnimationEnd}
          >
            {executions.isLoading ? <span className="muted-copy">正在读取步骤…</span> : null}
            {executions.isError ? <span className="error-copy">无法读取步骤：{errorMessage(executions.error)}</span> : null}
            {executions.data?.executions.map((execution) => <ExecutionStep key={`${execution.traceId}:${execution.stepId}`} execution={execution} />)}
            {executions.data && executions.data.executions.length === 0 ? <span className="muted-copy">该动态没有已保存的工具步骤。</span> : null}
          </div>
        ) : null}
      </div>
    </article>
  );
}

function ExecutionStep({ execution }: { execution: ToolExecutionRecord }) {
  const output = execution.error ?? executionOutput(execution);
  const approval = execution.approvedBy ? approvalSourceLabel(execution.approvedBy) : null;
  return (
    <div className="activity-trace-step">
      <div className="activity-trace-step-top">
        <strong><KeywordText text={execution.toolName} /></strong>
        <span className={`activity-execution-status activity-execution-status-${execution.status}`}>{EXECUTION_STATUS_LABEL[execution.status]}</span>
        <code>{execution.stepId}</code>
      </div>
      <div className="activity-trace-step-meta">
        <span>{environmentLabel(execution.environment)}</span>
        <span>·</span>
        <span>风险 {riskLabel(execution.riskLevel)}</span>
        <span>·</span>
        <span>{DECISION_LABEL[execution.decision]}</span>
        {approval ? <><span>·</span><span>{approval}</span></> : null}
        {execution.durationMs !== undefined ? <><span>·</span><span>{execution.durationMs}ms</span></> : null}
      </div>
      <code className="activity-trace-step-input">输入：<KeywordText text={formatAuditValue(execution.input, 240)} /></code>
      <code className={`activity-trace-step-output${execution.error ? " activity-trace-step-error" : ""}`}>{execution.error ? "错误" : "结果"}：<KeywordText text={formatAuditValue(output, 400)} /></code>
    </div>
  );
}

function executionOutput(execution: ToolExecutionRecord): unknown {
  if (isRecord(execution.output) && typeof execution.output.summary === "string") return execution.output.summary;
  return execution.output ?? "无摘要";
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function formatAuditValue(value: unknown, maxChars: number): string {
  const text = typeof value === "string" ? value : JSON.stringify(value) ?? "无";
  return text.length > maxChars ? `${text.slice(0, maxChars)}…` : text;
}

/* `formatTimestamp` 曾定义在这里，与 `AgentHistoryPane.tsx` 的 `formatHistoryTime`
   逐字节相同（包括 `Intl.DateTimeFormat` 的选项对象）。现已统一到 `lib/format.js`。 */

function actorLabel(actor: string): string {
  if (actor === "user") return "用户";
  if (actor === "core") return "Core";
  return actor;
}
