import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type {
  AgentRunMode,
  InvestigationBriefSelectInput,
  InvestigationRetentionPruneInput,
  InvestigationScheduleUpdateInput,
  InvestigationTask,
  InvestigationTaskCreateInput,
  TaskAutomationLevel,
} from "@yukinal/shared";
import { useEffect, useMemo, useState } from "react";

import { EmptyPanel, ErrorPanel, LoadingPanel, PreviewEmpty } from "../../components/PanelStates.js";
import { Icon } from "../../components/Icon.js";
import { PageToolbar } from "../../components/PageToolbar.js";
import { errorMessage } from "../../lib/format.js";
import { callDesktop, isDesktopShell, subscribeDesktop, type DesktopEventPayload } from "../../lib/ipc.js";
import { useServers } from "../../lib/servers.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { nativeTaskRuns } from "../agent/native-task-runs.js";
import { resolveTaskDelegation } from "./auto-delegation.js";
import { InvestigationDetail } from "./investigation-detail.js";
import { INVESTIGATION_QUERY_KEYS, useInvestigationOverview } from "./investigation-overview.js";
import { STATUS_LABEL, localInputToIso, splitLines } from "./investigations-format.js";
import {
  canStartTaskNow,
  isActiveRunConflict,
  shouldAutoStartRecoveredTask,
} from "./recovery.js";
import { LOCAL_TASK_TARGET, resolveInvestigationTaskTarget } from "./task-target.js";

export function InvestigationsPane() {
  const shell = isDesktopShell();
  const queryClient = useQueryClient();
  const selectedTaskId = useWorkspaceStore((state) => state.selectedTaskId);
  const selectTask = useWorkspaceStore((state) => state.selectTask);
  const setAgentOpen = useWorkspaceStore((state) => state.setAgentOpen);
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const servers = useServers({ enabled: shell });
  const [targetSelection, setTargetSelection] = useState(selectedServerId ?? "");
  const [objective, setObjective] = useState("");
  const [createOpen, setCreateOpen] = useState(false);
  const [criteria, setCriteria] = useState("");
  const [taskMode, setTaskMode] = useState<AgentRunMode>("goal");
  const [automationLevel, setAutomationLevel] = useState<TaskAutomationLevel>("execute");
  const [boundedDelegationRequested, setBoundedDelegationRequested] = useState(false);
  const [notBeforeAt, setNotBeforeAt] = useState("");
  const [expiresAt, setExpiresAt] = useState("");
  const [forbiddenTools, setForbiddenTools] = useState("");
  const [forbiddenPathPrefixes, setForbiddenPathPrefixes] = useState("");
  const [scheduleNotification, setScheduleNotification] = useState<DesktopEventPayload<"investigation.schedule_notification"> | null>(null);
  // A task whose next run was requested while its previous run was still live.
  // The host allows one run per task, so the start waits for that run to end.
  const [deferredStartTaskId, setDeferredStartTaskId] = useState<string | null>(null);

  useEffect(() => {
    if (!shell) return;
    const subscription = subscribeDesktop("investigation.schedule_notification", (event) => {
      setScheduleNotification(event);
      void queryClient.invalidateQueries({ queryKey: INVESTIGATION_QUERY_KEYS.tasks });
      void queryClient.invalidateQueries({ queryKey: INVESTIGATION_QUERY_KEYS.task(event.taskId) });
      void queryClient.invalidateQueries({ queryKey: INVESTIGATION_QUERY_KEYS.schedules });
    });
    return () => subscription.stop();
  }, [queryClient, shell]);

  const { tasks, schedules } = useInvestigationOverview();
  const detail = useQuery({
    queryKey: INVESTIGATION_QUERY_KEYS.task(selectedTaskId),
    enabled: shell && Boolean(selectedTaskId),
    refetchInterval: shell ? 2_000 : false,
    queryFn: () => callDesktop("investigation_task_get", { taskId: selectedTaskId as string }),
  });
  const selectedScheduleId = detail.data
    ? schedules.data?.schedules.find((schedule) => schedule.taskId === detail.data?.task.id)?.id
    : undefined;
  const scheduleRuns = useQuery({
    queryKey: INVESTIGATION_QUERY_KEYS.scheduleRuns(selectedScheduleId),
    enabled: shell && Boolean(selectedScheduleId),
    refetchInterval: shell ? 5_000 : false,
    queryFn: () => callDesktop("investigation_schedule_runs", { scheduleId: selectedScheduleId as string }),
  });
  const startTask = useMutation({
    mutationFn: (taskId: string) => nativeTaskRuns.start(taskId),
    // A host run may start before its metadata read fails. Refresh the real
    // task on either outcome so its independent Stop action remains visible.
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: INVESTIGATION_QUERY_KEYS.tasks });
      if (selectedTaskId) void queryClient.invalidateQueries({ queryKey: INVESTIGATION_QUERY_KEYS.task(selectedTaskId) });
    },
  });
  const stopTask = useMutation({
    mutationFn: (taskId: string) => nativeTaskRuns.stop(taskId),
    onSuccess: (response) => {
      void queryClient.invalidateQueries({ queryKey: INVESTIGATION_QUERY_KEYS.tasks });
      void queryClient.invalidateQueries({ queryKey: INVESTIGATION_QUERY_KEYS.task(response.task.id) });
    },
  });
  /**
   * Start a run, or defer it until the task's live run ends.
   *
   * The host allows one live run per task, and choosing a decision option only
   * records the choice: the follow-up run is started from here while the previous
   * run is often still finishing its step. That race is not a user mistake, so it
   * is remembered and retried from the poll that first sees the task idle; every
   * other failure stays a visible error.
   */
  const requestStart = async (taskId: string) => {
    try {
      await startTask.mutateAsync(taskId);
      setDeferredStartTaskId(null);
    } catch (error) {
      if (isActiveRunConflict(error)) setDeferredStartTaskId(taskId);
    }
  };
  const createTask = useMutation({
    mutationFn: (input: InvestigationTaskCreateInput) => callDesktop("investigation_task_create", { input }),
    onSuccess: (response) => {
      // Keep the submit control visible until the user closes the form.
      setCreateOpen(true);
      setObjective("");
      setCriteria("");
      setTaskMode("goal");
      setAutomationLevel("execute");
      setBoundedDelegationRequested(false);
      setNotBeforeAt("");
      setExpiresAt("");
      setForbiddenTools("");
      setForbiddenPathPrefixes("");
      void queryClient.invalidateQueries({ queryKey: INVESTIGATION_QUERY_KEYS.tasks });
      selectTask(response.task.id);
      // The create button is an explicit request for Yukinal to start the goal now.
      // Tool-level permission checks still pause for approval before any gated action.
      void requestStart(response.task.id);
    },
  });
  const selectBrief = useMutation({
    mutationFn: (input: InvestigationBriefSelectInput) => callDesktop("investigation_brief_select", { input }),
    onSuccess: (response, input) => {
      void queryClient.invalidateQueries({ queryKey: INVESTIGATION_QUERY_KEYS.tasks });
      void queryClient.invalidateQueries({ queryKey: INVESTIGATION_QUERY_KEYS.task(selectedTaskId) });
      // A brief may opt into exactly one bounded continuation. Legacy options
      // omit the field and are normalized to wait_user by the host, so a stale
      // or hand-crafted brief can never auto-start a run accidentally.
      if (response.continuation === "continue_readonly" || response.continuation === "start_plan") {
        void requestStart(input.taskId);
      }
    },
  });
  const recoverTask = useMutation({
    mutationFn: (input: { taskId: string; optionId?: string }) => callDesktop("investigation_task_recover", { input }),
    onSuccess: (response) => {
      void queryClient.invalidateQueries({ queryKey: INVESTIGATION_QUERY_KEYS.tasks });
      void queryClient.invalidateQueries({ queryKey: INVESTIGATION_QUERY_KEYS.task(response.task.id) });
      // Retry/replan/resume recovery choices are host-owned lifecycle decisions.
      // Once the host has invalidated the old run and returned the task to
      // `investigating`, continue through the same durable start entry point;
      // rollback, wait-user and stop recoveries return other states and stay
      // visible for the next explicit decision.
      if (shouldAutoStartRecoveredTask(response.task)) {
        void requestStart(response.task.id);
      }
    },
  });
  const createSchedule = useMutation({
    mutationFn: (taskId: string) => callDesktop("investigation_schedule_create", {
      input: { taskId, intervalSeconds: 300, notificationPolicy: "on_change" },
    }),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: INVESTIGATION_QUERY_KEYS.schedules }),
  });
  const updateSchedule = useMutation({
    mutationFn: (input: InvestigationScheduleUpdateInput) => callDesktop("investigation_schedule_update", { input }),
    onSuccess: (response) => {
      void queryClient.invalidateQueries({ queryKey: INVESTIGATION_QUERY_KEYS.schedules });
      void queryClient.invalidateQueries({ queryKey: INVESTIGATION_QUERY_KEYS.scheduleRuns(response.schedule.id) });
    },
  });
  const retentionPreview = useMutation({
    mutationFn: (taskId: string) => callDesktop("investigation_retention_preview", { taskId }),
  });
  const retentionPrune = useMutation({
    mutationFn: (input: InvestigationRetentionPruneInput) => callDesktop("investigation_retention_prune", { input }),
    onSuccess: (response) => {
      void queryClient.invalidateQueries({ queryKey: INVESTIGATION_QUERY_KEYS.tasks });
      void queryClient.invalidateQueries({ queryKey: INVESTIGATION_QUERY_KEYS.task(response.result.taskId) });
      retentionPreview.reset();
    },
  });

  const serverRows = servers.data ?? [];
  const scope = useMemo(
    () => resolveInvestigationTaskTarget(targetSelection, serverRows),
    [serverRows, targetSelection],
  );
  const selectedServer = scope?.host === "remote"
    ? serverRows.find((server) => server.id === scope.serverId)
    : undefined;
  const taskDelegation = resolveTaskDelegation({
    requested: boundedDelegationRequested,
    scope,
    mode: taskMode,
    automationLevel,
  });

  useEffect(() => {
    if (!taskDelegation.eligible) setBoundedDelegationRequested(false);
  }, [taskDelegation.eligible]);

  // A start that lost the single-live-run race is retried from the first poll that
  // reports the task idle, so choosing a decision option never needs a second click.
  // The poll keeps arriving while the old run finishes; once it is gone the task is
  // started exactly once and the pending request is cleared.
  const deferredDetail = detail.data;
  const startDeferred = startTask.mutateAsync;
  useEffect(() => {
    if (!deferredStartTaskId || !deferredDetail) return;
    if (deferredDetail.task.id !== deferredStartTaskId) return;
    if (!canStartTaskNow(deferredDetail.task, deferredDetail.runs)) return;
    setDeferredStartTaskId(null);
    void startDeferred(deferredStartTaskId).catch(() => undefined);
  }, [deferredDetail, deferredStartTaskId, startDeferred]);

  if (!shell) return <PreviewEmpty icon="agent" body="桌面功能不可用" />;
  if (tasks.isLoading) return <LoadingPanel title="正在读取排查任务" />;
  if (tasks.isError || !tasks.data) {
    return <ErrorPanel showIcon title="无法读取排查任务" message={errorMessage(tasks.error)} onRetry={() => void tasks.refetch()} />;
  }

  const taskRows = tasks.data.tasks;
  const showCreate = createOpen || taskRows.length === 0;
  return (
    <div className="investigations-page">
      <PageToolbar actions={<>
        <button type="button" className="button-secondary" onClick={() => void tasks.refetch()} disabled={tasks.isFetching}>
          <Icon name="refresh" size="sm" /> {tasks.isFetching ? "读取中" : "刷新"}
        </button>
        {taskRows.length > 0 ? <button type="button" className="button-secondary" aria-expanded={showCreate} aria-controls="investigation-create" onClick={() => setCreateOpen((open) => !open)}>{showCreate ? "收起新建" : "新建任务"}</button> : null}
      </>}><span>{taskRows.length} 个任务</span></PageToolbar>

      {scheduleNotification ? (
        <section className="investigation-notification" role="status">
          <div>
            <strong>{scheduleNotification.title}</strong>
            <small>持续巡检 · {scheduleNotification.outcome} · {scheduleNotification.at}</small>
          </div>
          <button type="button" className="button-secondary" onClick={() => setScheduleNotification(null)}>知道了</button>
        </section>
      ) : null}

      <section id="investigation-create" className="investigation-create-card" hidden={!showCreate} aria-labelledby="investigation-create-title">
        <div>
          <h3 id="investigation-create-title">新建任务</h3>
        </div>
        <label className="field-label" htmlFor="investigation-server">目标服务器 / 运行范围</label>
        <select
          id="investigation-server"
          className="text-input investigation-server-select"
          value={targetSelection}
          onChange={(event) => {
            setTargetSelection(event.target.value);
            setBoundedDelegationRequested(false);
          }}
          aria-describedby="investigation-server-hint"
        >
          <option value="">请选择已添加的服务器</option>
          {serverRows.map((server) => (
            <option key={server.id} value={server.id}>
              {server.name} · {server.connection.host}:{server.connection.port} · {server.metadata.environment}
            </option>
          ))}
          <option value={LOCAL_TASK_TARGET}>本机范围（不连接远端）</option>
        </select>
        <div className="investigation-target" id="investigation-server-hint" role="status">
          <span>本次任务范围</span>
          <strong>
            {selectedServer
              ? `${selectedServer.name} · ${selectedServer.connection.host}:${selectedServer.connection.port} · ${selectedServer.metadata.environment}`
              : scope?.host === "local"
                ? "本机（不会调用远端服务器）"
                : servers.isLoading
                  ? "正在读取服务器列表…"
                  : servers.isError
                    ? "服务器列表读取失败，请重试或重新选择"
                    : "请选择服务器；不会默认把目标发送到本机或其他服务器"}
          </strong>
        </div>
        {servers.isError ? <p className="form-error" role="alert">无法读取服务器列表：{errorMessage(servers.error)}</p> : null}
        <label className="field-label" htmlFor="investigation-objective">目标</label>
        <textarea
          id="investigation-objective"
          className="text-input investigation-objective"
          value={objective}
          onChange={(event) => setObjective(event.target.value)}
          placeholder="例如：检查 staging API 的 502 错误，修复配置并验证服务恢复"
          rows={2}
        />
        <label className="investigation-task-delegation">
          <input
            type="checkbox"
            checked={taskDelegation.grantTaskCommands}
            disabled={!taskDelegation.eligible}
            onChange={(event) => setBoundedDelegationRequested(event.target.checked)}
          />
          <span>
            <strong>允许本次任务自动执行</strong>
            <small>
              {taskDelegation.eligible
                ? "开发 / 预发布 · 最多 12 次命令 / 15 分钟 / 1 MiB · 授权 4 小时 · 高危操作逐项审批"
                : "当前目标或运行模式不支持自动执行"}
            </small>
          </span>
        </label>
        <details className="investigation-advanced">
          <summary>执行与验证选项</summary>
          <div className="investigation-advanced-content">
            <label className="field-label" htmlFor="investigation-criteria">自定义完成标准（可选，每行一条）</label>
            <textarea
              id="investigation-criteria"
              className="text-input investigation-criteria"
              value={criteria}
              onChange={(event) => setCriteria(event.target.value)}
              placeholder="完成标准（可选）"
              rows={2}
            />
            <div className="investigation-create-options">
              <div>
                <label className="field-label" htmlFor="investigation-mode">运行边界</label>
                <select id="investigation-mode" className="text-input" value={taskMode} onChange={(event) => {
                  const next = event.target.value as AgentRunMode;
                  setTaskMode(next);
                  setBoundedDelegationRequested(false);
                  if (next === "readonly") setAutomationLevel("readonly");
                }}>
                  <option value="readonly">只读排查</option>
                  <option value="plan">计划 / dry-run</option>
                  <option value="goal">目标执行</option>
                </select>
              </div>
              <div>
                <label className="field-label" htmlFor="investigation-automation">任务自动化级别</label>
                <select id="investigation-automation" className="text-input" value={automationLevel} disabled={taskMode === "readonly"} onChange={(event) => {
                  setAutomationLevel(event.target.value as TaskAutomationLevel);
                  setBoundedDelegationRequested(false);
                }}>
                  <option value="readonly">只采集，不执行变更</option>
                  <option value="propose">先给方案，选择后执行</option>
                  <option value="execute">按计划推进，变更仍受审批</option>
                </select>
              </div>
            </div>
            <div className="investigation-guardrails-editor">
              <div>
                <span className="field-label">允许的时间窗（可选）</span>
                <div className="investigation-time-window">
                  <label>
                    <span className="project-muted">开始</span>
                    <input
                      type="datetime-local"
                      className="text-input"
                      value={notBeforeAt}
                      onChange={(event) => setNotBeforeAt(event.target.value)}
                      aria-label="任务时间窗开始"
                    />
                  </label>
                  <label>
                    <span className="project-muted">截止</span>
                    <input
                      type="datetime-local"
                      className="text-input"
                      value={expiresAt}
                      onChange={(event) => setExpiresAt(event.target.value)}
                      aria-label="任务时间窗截止"
                    />
                  </label>
                </div>
                <small className="project-muted">宿主按 UTC 记录并在每次运行前复核；留空表示不额外限制时间。</small>
              </div>
              <div className="investigation-guardrail-lists">
                <label>
                  <span className="field-label">禁止工具（每行一个内部名）</span>
                  <textarea
                    className="text-input"
                    value={forbiddenTools}
                    onChange={(event) => setForbiddenTools(event.target.value)}
                    placeholder="例如：docker.restart\npackage.install"
                    rows={2}
                  />
                </label>
                <label>
                  <span className="field-label">禁止路径前缀（每行一个绝对路径）</span>
                  <textarea
                    className="text-input"
                    value={forbiddenPathPrefixes}
                    onChange={(event) => setForbiddenPathPrefixes(event.target.value)}
                    placeholder="例如：/srv/app/private\n/etc/production"
                    rows={2}
                  />
                </label>
              </div>
            </div>
          </div>
        </details>
        <div className="investigation-create-footer">
          <span className="project-muted">
            {selectedServer
              ? `${selectedServer.name} · ${automationLevel === "execute" ? "按目标推进，变更受权限约束" : "先观察或生成方案"}`
              : scope?.host === "local"
                ? "本机范围 · 不会连接远端服务器"
                : "请选择目标服务器"}
          </span>
          <button
            type="button"
            className="button-primary"
            disabled={!objective.trim() || !scope || createTask.isPending || startTask.isPending}
            onClick={() => {
              if (!scope) return;
              const customCriteria = criteria.split("\n").map((line) => line.trim()).filter(Boolean);
              createTask.mutate({
                objective: objective.trim(),
                successCriteria: customCriteria.length > 0 ? customCriteria : ["目标已通过可复核的证据验证"],
                scope,
                guardrails: {
                  notBeforeAt: localInputToIso(notBeforeAt),
                  expiresAt: localInputToIso(expiresAt),
                  forbiddenTools: splitLines(forbiddenTools),
                  forbiddenPathPrefixes: splitLines(forbiddenPathPrefixes),
                },
                mode: taskMode,
                permissionMode: taskDelegation.permissionMode,
                automationLevel,
                grantTaskCommands: taskDelegation.grantTaskCommands,
              });
            }}
          >
            <Icon name="agent" size="sm" /> {createTask.isPending || startTask.isPending ? "启动中" : "开始任务"}
          </button>
        </div>
        {createTask.isError ? <p className="form-error" role="alert">{errorMessage(createTask.error)}</p> : null}
      </section>

      {taskRows.length === 0 ? (
        <EmptyPanel extraClass="investigation-empty" icon="activity" title="暂无任务" />
      ) : (
        <div className="investigation-layout">
          <ul className="investigation-list" aria-label="排查任务列表">
            {taskRows.map((task) => (
              <TaskRow key={task.id} task={task} serverName={serverRows.find((server) => server.id === task.scope.serverId)?.name} selected={task.id === selectedTaskId} onSelect={() => selectTask(task.id)} />
            ))}
          </ul>
          {selectedTaskId && detail.data ? (
            <InvestigationDetail
              task={detail.data.task}
              evidence={detail.data.evidence}
              findings={detail.data.findings}
              artifacts={detail.data.artifacts}
              decisionBrief={detail.data.decisionBrief}
              runs={detail.data.runs}
              steps={detail.data.steps}
              plan={detail.data.plan}
              schedules={schedules.data?.schedules.filter((schedule) => schedule.taskId === detail.data?.task.id) ?? []}
              scheduleRuns={scheduleRuns.data?.runs ?? []}
              onStart={() => void requestStart(detail.data.task.id)}
              starting={startTask.isPending && startTask.variables === detail.data.task.id}
              startDeferred={deferredStartTaskId === detail.data.task.id}
              startError={startTask.isError && startTask.variables === detail.data.task.id ? errorMessage(startTask.error) : undefined}
              onStop={() => stopTask.mutate(detail.data.task.id)}
              stopping={stopTask.isPending && stopTask.variables === detail.data.task.id}
              stopError={stopTask.isError && stopTask.variables === detail.data.task.id ? errorMessage(stopTask.error) : undefined}
              onCreateSchedule={() => createSchedule.mutate(detail.data.task.id)}
              onUpdateSchedule={(input) => updateSchedule.mutate(input)}
              scheduling={createSchedule.isPending || updateSchedule.isPending}
              scheduleError={updateSchedule.isError ? errorMessage(updateSchedule.error) : createSchedule.isError ? errorMessage(createSchedule.error) : undefined}
              retentionPreview={retentionPreview.data?.preview.taskId === detail.data.task.id ? retentionPreview.data.preview : undefined}
              onPreviewRetention={() => retentionPreview.mutate(detail.data.task.id)}
              previewingRetention={retentionPreview.isPending}
              retentionPreviewError={retentionPreview.isError ? errorMessage(retentionPreview.error) : undefined}
              onPruneRetention={(input) => retentionPrune.mutate(input)}
              pruningRetention={retentionPrune.isPending}
              retentionPruneError={retentionPrune.isError ? errorMessage(retentionPrune.error) : undefined}
              onContinue={() => setAgentOpen(true)}
              onRecover={(optionId) => recoverTask.mutate({ taskId: detail.data.task.id, ...(optionId ? { optionId } : {}) })}
              recovering={recoverTask.isPending}
              recoveryError={recoverTask.isError ? errorMessage(recoverTask.error) : undefined}
              onSelectOption={(optionId) => {
                if (!detail.data.decisionBrief) return;
                selectBrief.mutate({
                  taskId: detail.data.task.id,
                  briefId: detail.data.decisionBrief.id,
                  optionId,
                });
              }}
              selectingOptionId={selectBrief.isPending ? selectBrief.variables?.optionId : undefined}
              selectionError={selectBrief.isError ? errorMessage(selectBrief.error) : undefined}
            />
          ) : null}
        </div>
      )}
    </div>
  );
}


function TaskRow({ task, serverName, selected, onSelect }: { task: InvestigationTask; serverName?: string; selected: boolean; onSelect: () => void }) {
  return (
    <li>
      <button type="button" className={`investigation-row ${selected ? "investigation-row-selected" : ""}`} onClick={onSelect} aria-pressed={selected}>
        <span className={`investigation-status investigation-status-${task.status}`}>{STATUS_LABEL[task.status]}</span>
        <strong>{task.objective}</strong>
        <small>{task.scope.host === "remote" ? serverName ?? task.scope.serverId ?? "远端服务器" : "本机"} · {task.successCriteria.length} 项完成标准</small>
      </button>
    </li>
  );
}
