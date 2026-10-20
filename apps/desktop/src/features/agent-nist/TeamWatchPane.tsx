import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { IPC_COMMANDS } from "@yukinal/shared";
import { useEffect, useState } from "react";
import { Icon } from "../../components/Icon.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { isPreviewFixture } from "../../lib/preview-fixtures.js";
import { errorMessage, formatTimestamp } from "../../lib/format.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { scheduleIntervalLabel, notificationPolicyLabel } from "../investigations/schedule-ui.js";
import { TEAM_STAGE_LABEL, teamRunActive, useTeamRuntimeStore } from "./team-runtime-store.js";

export function useTeamWatch() {
  return useQuery({ queryKey: ["team-watch"], enabled: isDesktopShell(), staleTime: 2_000, refetchInterval: 2_000,
    queryFn: async () => {
      const [tasks, schedules] = await Promise.all([callDesktop(IPC_COMMANDS.investigationTaskList, {}), callDesktop(IPC_COMMANDS.investigationScheduleList, {})]);
      return { tasks: tasks.tasks, schedules: schedules.schedules };
    },
  });
}

export function TeamWatchPane() {
  const query = useTeamWatch();
  const serverId = useWorkspaceStore((state) => state.selectedServerId);
  const totals = useTeamRuntimeStore((state) => state.totals);
  const runs = useTeamRuntimeStore((state) => state.runs);
  const updates = useTeamRuntimeStore((state) => state.watchUpdates);
  const queryClient = useQueryClient();
  const [notice, setNotice] = useState<string | null>(null);
  const update = useMutation({ mutationFn: (input: { scheduleId: string; status: "active" | "paused" }) => callDesktop(IPC_COMMANDS.investigationScheduleUpdate, { input }), onMutate: () => setNotice(null), onSuccess: () => { void queryClient.invalidateQueries({ queryKey: ["team-watch"] }); }, onError: (error) => setNotice(errorMessage(error)) });
  const tasks = new Map(query.data?.tasks.map((task) => [task.id, task]));
  const rows = query.data?.schedules.filter((schedule) => !serverId || tasks.get(schedule.taskId)?.scope.serverId === serverId) ?? [];
  const visibleUpdates = updates.filter((update) => rows.some((row) => row.id === update.scheduleId));
  const readKey = visibleUpdates.map((update) => update.scheduleId).join("\n");
  useEffect(() => {
    if (readKey) {
      void queryClient.invalidateQueries({ queryKey: ["team-watch"] });
      useTeamRuntimeStore.getState().readWatchUpdates(readKey.split("\n"));
    }
  }, [readKey, queryClient]);
  return <div className="team-watch-pane">
    <div className="team-heartbeat"><span><Icon name="activity" size="sm" />值守状态</span><strong>独立更新</strong><small>{query.dataUpdatedAt ? `更新 ${formatTimestamp(new Date(query.dataUpdatedAt).toISOString())}` : "等待数据"}</small></div>
    <div className="team-watch-intro"><strong>持续值守</strong><span>进度与最新结果在此更新，主对话可继续使用。</span></div>
    {query.isLoading ? <p className="agent-nist-notice" role="status">读取值守计划…</p> : null}
    {query.isError ? <div className="agent-nist-notice is-error" role="alert">无法读取值守计划：{errorMessage(query.error)}<button type="button" className="agent-nist-text-action" onClick={() => void query.refetch()}>重试</button></div> : null}
    {notice ? <p className="agent-nist-notice is-error" role="alert">{notice}</p> : null}
    {!query.isLoading && !query.isError && !rows.length ? <p className="team-empty">当前范围没有值守计划。</p> : null}
    <ul className="team-watch-list">{rows.map((schedule) => {
      const run = [...runs].reverse().find((run) => run.source === "watch" && run.scheduleId === schedule.id && teamRunActive(run));
      const working = schedule.status === "active" && (Boolean(run) || schedule.lastRunStatus === "running" || schedule.lastRunStatus === "claimed");
      return <li key={schedule.id} className="team-watch-operation team-color-blue">
      {working ? <span className="team-operation-outline" data-operation="working" aria-hidden="true" /> : null}
      <details className="team-watch-item"><summary className="team-watch-summary">
        <div className="team-watch-heading"><strong>{scheduleIntervalLabel(schedule.intervalSeconds)}</strong><span className={working ? "is-active" : ""}>{schedule.status === "active" ? (run ? TEAM_STAGE_LABEL[run.stage] : schedule.lastRunStatus === "running" ? "AI 巡检中" : schedule.lastRunStatus === "claimed" ? "等待启动" : "等待下轮") : schedule.status === "paused" ? "已暂停" : "已撤销"}<Icon name="chevronDown" size="xs" /></span></div>
        <p className="team-watch-goal">{tasks.get(schedule.taskId)?.objective ?? "原任务不可用"}</p>
        <span className="team-watch-policy">{notificationPolicyLabel(schedule.notificationPolicy)}</span>
        <p className={`team-watch-preview ${schedule.lastError ? "team-watch-error" : "team-watch-outcome"}`}>{watchResult(schedule.lastError, schedule.lastOutcome)}</p>
      </summary><div className="team-watch-expanded">
        <span className="team-watch-policy">{schedule.status === "active" ? `下次 AI 巡检 ${formatTimestamp(schedule.nextRunAt)}` : "AI 巡检已暂停"}</span>
        <p className={schedule.lastError ? "team-watch-error" : "team-watch-outcome"}>{watchResult(schedule.lastError, schedule.lastOutcome)}</p>
      </div></details>
      <div className="team-watch-meta"><time dateTime={schedule.lastRunAt}>{schedule.lastRunAt ? `上次开始 ${formatTimestamp(schedule.lastRunAt)}` : "尚未执行"}</time>{schedule.status !== "revoked" ? <button type="button" className="agent-nist-text-action" disabled={isPreviewFixture() || update.isPending} title={isPreviewFixture() ? "模拟模式不修改值守计划" : undefined} onClick={() => update.mutate({ scheduleId: schedule.id, status: schedule.status === "active" ? "paused" : "active" })}>{schedule.status === "active" ? "暂停" : "恢复"}</button> : null}</div>
    </li>; })}</ul>
    <div className="team-usage"><strong>本窗口已上报用量</strong><span>{totals.reportedRuns ? `${(totals.inputTokens + totals.outputTokens).toLocaleString()} token` : "暂无用量上报"}</span><small>{totals.reportedRuns ? `输入 ${totals.inputTokens.toLocaleString()} · 输出 ${totals.outputTokens.toLocaleString()}` : "按运行去重，随心跳汇总"}{totals.unreportedRuns ? ` · ${totals.unreportedRuns} 次尚未上报` : ""}</small></div>
    <p className="agent-nist-footnote">应用运行时自动巡检。资源、日志独立更新；AI 频率以各计划为准。</p>
  </div>;
}

export function watchResult(error?: string, outcome?: string): string {
  if (error) return ({ app_restarted: "上轮因重启中断，等待下次巡检", inspection_failed: "巡检失败", inspection_start_failed: "巡检启动失败", inspection_timeout: "巡检连接或执行超时，未能完成本轮取证", inspection_tool_denied: "巡检工具未通过任务范围检查，本轮未完成", inspection_plan_invalid: "巡检计划包含无法识别的工具，本轮未完成", schedule_paused: "值守已暂停", task_stopped: "原任务已停止" } as Record<string, string>)[error] ?? error;
  if (!outcome) return "尚无执行结果";
  try { const report: unknown = JSON.parse(outcome); if (report && typeof report === "object" && "summary" in report && typeof report.summary === "string") return report.summary; } catch { /* Older runs contain plain text. */ }
  return outcome;
}

export function ConversationWatchStatus({ onOpen }: { onOpen?: () => void }) {
  const query = useTeamWatch();
  const serverId = useWorkspaceStore((state) => state.selectedServerId);
  const schedules = query.data?.schedules.filter((schedule) => serverId && query.data.tasks.find((task) => task.id === schedule.taskId)?.scope.serverId === serverId && schedule.status !== "revoked") ?? [];
  if (!schedules.length) return null;
  return <button type="button" className="conversation-watch-status" onClick={() => { useTeamRuntimeStore.getState().setPanelView("watch"); if (onOpen) onOpen(); else useWorkspaceStore.getState().setPrimary("agents"); }} title="在团队面板查看已登记的值守计划"><Icon name="activity" size="sm" /><span>值守计划 · {schedules.length}</span><span>{schedules.filter((schedule) => schedule.status === "active").length} 已启用</span><Icon name="chevronRight" size="xs" /></button>;
}
