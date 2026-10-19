import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { IPC_COMMANDS } from "@yukinal/shared";
import { useState } from "react";
import { Icon } from "../../components/Icon.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { isPreviewFixture } from "../../lib/preview-fixtures.js";
import { errorMessage, formatTimestamp } from "../../lib/format.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { scheduleIntervalLabel, notificationPolicyLabel } from "../investigations/schedule-ui.js";
import { heartbeatMultiple, TEAM_HEARTBEAT_MS } from "./team-operations.js";
import { useTeamRuntimeStore } from "./team-runtime-store.js";

export function useTeamWatch() {
  return useQuery({ queryKey: ["team-watch"], enabled: isDesktopShell(), staleTime: TEAM_HEARTBEAT_MS,
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
  const at = useTeamRuntimeStore((state) => state.lastBeatAt);
  const queryClient = useQueryClient();
  const [notice, setNotice] = useState<string | null>(null);
  const update = useMutation({ mutationFn: (input: { scheduleId: string; status: "active" | "paused" }) => callDesktop(IPC_COMMANDS.investigationScheduleUpdate, { input }), onMutate: () => setNotice(null), onSuccess: () => { void queryClient.invalidateQueries({ queryKey: ["team-watch"] }); }, onError: (error) => setNotice(errorMessage(error)) });
  const tasks = new Map(query.data?.tasks.map((task) => [task.id, task]));
  const rows = query.data?.schedules.filter((schedule) => !serverId || tasks.get(schedule.taskId)?.scope.serverId === serverId) ?? [];
  return <div className="team-watch-pane">
    <div className="team-heartbeat"><span><Icon name="activity" size="sm" />统一刷新</span><strong>30 秒 / 拍</strong><small>{at ? `最近 ${formatTimestamp(new Date(at).toISOString())}` : "等待刷新"}</small></div>
    <div className="team-watch-intro"><strong>已登记的值守</strong><span>显示已保存的频率与最近结果。</span></div>
    {query.isLoading ? <p className="agent-nist-notice" role="status">读取值守计划…</p> : null}
    {query.isError ? <div className="agent-nist-notice is-error" role="alert">无法读取值守计划：{errorMessage(query.error)}<button type="button" className="agent-nist-text-action" onClick={() => void query.refetch()}>重试</button></div> : null}
    {notice ? <p className="agent-nist-notice is-error" role="alert">{notice}</p> : null}
    {!query.isLoading && !query.isError && !rows.length ? <p className="team-empty">当前范围没有值守计划。</p> : null}
    <ul className="team-watch-list">{rows.map((schedule) => <li key={schedule.id}>
      <details className="team-watch-item"><summary className="team-watch-summary">
        <div className="team-watch-heading"><strong>{scheduleIntervalLabel(schedule.intervalSeconds)}</strong><span className={schedule.status === "active" ? "is-active" : ""}>{schedule.status === "active" ? "已启用" : schedule.status === "paused" ? "已暂停" : "已撤销"}<Icon name="chevronDown" size="xs" /></span></div>
        <p className="team-watch-goal">{tasks.get(schedule.taskId)?.objective ?? "原任务不可用"}</p>
        <span className="team-watch-policy">{notificationPolicyLabel(schedule.notificationPolicy)}</span>
        <p className={`team-watch-preview ${schedule.lastError ? "team-watch-error" : "team-watch-outcome"}`}>{schedule.lastError || schedule.lastOutcome || "尚无执行结果"}</p>
      </summary><div className="team-watch-expanded">
        <span className="team-watch-policy">心跳换算 {heartbeatMultiple(schedule.intervalSeconds)} 拍</span>
        {schedule.lastError ? <p className="team-watch-error">{schedule.lastError}</p> : <p className="team-watch-outcome">{schedule.lastOutcome || "尚无执行结果"}</p>}
      </div></details>
      <div className="team-watch-meta"><time dateTime={schedule.lastRunAt}>{schedule.lastRunAt ? `上次 ${formatTimestamp(schedule.lastRunAt)}` : "尚未执行"}</time>{schedule.status !== "revoked" ? <button type="button" className="agent-nist-text-action" disabled={isPreviewFixture() || update.isPending} title={isPreviewFixture() ? "模拟模式不修改值守计划" : undefined} onClick={() => update.mutate({ scheduleId: schedule.id, status: schedule.status === "active" ? "paused" : "active" })}>{schedule.status === "active" ? "暂停" : "恢复"}</button> : null}</div>
    </li>)}</ul>
    <div className="team-usage"><strong>本窗口已上报用量</strong><span>{totals.reportedRuns ? `${(totals.inputTokens + totals.outputTokens).toLocaleString()} token` : "暂无用量上报"}</span><small>{totals.reportedRuns ? `输入 ${totals.inputTokens.toLocaleString()} · 输出 ${totals.outputTokens.toLocaleString()}` : "按运行去重，随心跳汇总"}{totals.unreportedRuns ? ` · ${totals.unreportedRuns} 次尚未上报` : ""}</small></div>
    <p className="agent-nist-footnote">当前心跳同步状态与用量；自动触发执行、异常唤醒团队仍待接通。</p>
  </div>;
}

export function ConversationWatchStatus({ onOpen }: { onOpen?: () => void }) {
  const query = useTeamWatch();
  const serverId = useWorkspaceStore((state) => state.selectedServerId);
  const schedules = query.data?.schedules.filter((schedule) => serverId && query.data.tasks.find((task) => task.id === schedule.taskId)?.scope.serverId === serverId && schedule.status !== "revoked") ?? [];
  if (!schedules.length) return null;
  return <button type="button" className="conversation-watch-status" onClick={() => { useTeamRuntimeStore.getState().setPanelView("watch"); if (onOpen) onOpen(); else useWorkspaceStore.getState().setPrimary("agents"); }} title="在团队面板查看已登记的值守计划"><Icon name="activity" size="sm" /><span>值守计划 · {schedules.length}</span><span>{schedules.filter((schedule) => schedule.status === "active").length} 已启用</span><Icon name="chevronRight" size="xs" /></button>;
}
