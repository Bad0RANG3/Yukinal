import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type {
  DecisionBrief,
  DecisionOptionContinuation,
  Evidence,
  Finding,
  InvestigationArtifact,
  InvestigationBriefSelectInput,
  InvestigationRun,
  InvestigationPlan,
  InvestigationStep,
  InvestigationTask,
  InvestigationTaskCreateInput,
  InvestigationSchedule,
  InvestigationScheduleRun,
  InvestigationScheduleUpdateInput,
  InvestigationRetentionPreview,
  InvestigationRetentionPruneInput,
  InvestigationNotificationPolicy,
  AgentPermissionMode,
  AgentRunMode,
  TaskAutomationLevel,
  TaskStatus,
} from "@yukinal/shared";
import { useEffect, useMemo, useRef, useState } from "react";

import { EmptyPanel, ErrorPanel, LoadingPanel, PreviewEmpty } from "../../components/PanelStates.js";
import { Icon } from "../../components/Icon.js";
import { errorMessage } from "../../lib/format.js";
import { callDesktop, isDesktopShell, subscribeDesktop, type DesktopEventPayload } from "../../lib/ipc.js";
import { useServers } from "../../lib/servers.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { canDelegateAgentAuto, effectiveTaskPermissionMode, shouldAutoStartCreatedTask } from "./auto-delegation.js";
import { shouldAutoRecoverDelegatedTask, shouldAutoStartRecoveredTask } from "./recovery.js";
import {
  INVESTIGATION_NOTIFICATION_POLICIES,
  notificationPolicyLabel,
  scheduleIntervalLabel,
  scheduleIntervalOptions,
  scheduleStatusLabel,
} from "./schedule-ui.js";

const STATUS_LABEL: Record<TaskStatus, string> = {
  pending: "待开始",
  investigating: "排查中",
  waiting_user: "等待选择",
  executing: "执行中",
  verifying: "验证中",
  completed: "已完成",
  failed: "失败",
  stopped: "已停止",
  expired: "已过期",
};

const OBSERVATION_STATUS_LABEL: Record<NonNullable<InvestigationPlan["observationWindow"]>["status"], string> = {
  pending: "待开始",
  running: "进行中",
  succeeded: "已完成",
  failed: "发现异常",
  cancelled: "已取消",
};

export function InvestigationsPane() {
  const shell = isDesktopShell();
  const queryClient = useQueryClient();
  const selectedTaskId = useWorkspaceStore((state) => state.selectedTaskId);
  const selectTask = useWorkspaceStore((state) => state.selectTask);
  const setAgentOpen = useWorkspaceStore((state) => state.setAgentOpen);
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const servers = useServers({ enabled: shell });
  const [objective, setObjective] = useState("");
  const [criteria, setCriteria] = useState("确认服务状态与近期日志是否存在同一异常\n给出只读证据和下一步选择");
  const [taskMode, setTaskMode] = useState<AgentRunMode>("readonly");
  const [automationLevel, setAutomationLevel] = useState<TaskAutomationLevel>("readonly");
  const [permissionMode, setPermissionMode] = useState<AgentPermissionMode>("ask");
  const [notBeforeAt, setNotBeforeAt] = useState("");
  const [expiresAt, setExpiresAt] = useState("");
  const [forbiddenTools, setForbiddenTools] = useState("");
  const [forbiddenPathPrefixes, setForbiddenPathPrefixes] = useState("");
  const [scheduleNotification, setScheduleNotification] = useState<DesktopEventPayload<"investigation.schedule_notification"> | null>(null);

  useEffect(() => {
    if (!shell) return;
    const subscription = subscribeDesktop("investigation.schedule_notification", (event) => {
      setScheduleNotification(event);
      void queryClient.invalidateQueries({ queryKey: ["investigationTasks"] });
      void queryClient.invalidateQueries({ queryKey: ["investigationTask", event.taskId] });
      void queryClient.invalidateQueries({ queryKey: ["investigationSchedules"] });
    });
    return () => subscription.stop();
  }, [queryClient, shell]);

  const tasks = useQuery({
    queryKey: ["investigationTasks"],
    enabled: shell,
    staleTime: 2_000,
    refetchInterval: shell ? 2_000 : false,
    queryFn: () => callDesktop("investigation_task_list", {}),
  });
  const detail = useQuery({
    queryKey: ["investigationTask", selectedTaskId],
    enabled: shell && Boolean(selectedTaskId),
    refetchInterval: shell ? 2_000 : false,
    queryFn: () => callDesktop("investigation_task_get", { taskId: selectedTaskId as string }),
  });
  const schedules = useQuery({
    queryKey: ["investigationSchedules"],
    enabled: shell,
    refetchInterval: shell ? 5_000 : false,
    queryFn: () => callDesktop("investigation_schedule_list", {}),
  });
  const selectedScheduleId = detail.data
    ? schedules.data?.schedules.find((schedule) => schedule.taskId === detail.data?.task.id)?.id
    : undefined;
  const scheduleRuns = useQuery({
    queryKey: ["investigationScheduleRuns", selectedScheduleId],
    enabled: shell && Boolean(selectedScheduleId),
    refetchInterval: shell ? 5_000 : false,
    queryFn: () => callDesktop("investigation_schedule_runs", { scheduleId: selectedScheduleId as string }),
  });
  const startTask = useMutation({
    mutationFn: (taskId: string) => callDesktop("investigation_task_start", { taskId }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["investigationTasks"] });
      if (selectedTaskId) void queryClient.invalidateQueries({ queryKey: ["investigationTask", selectedTaskId] });
    },
  });
  const stopTask = useMutation({
    mutationFn: (taskId: string) => callDesktop("investigation_task_stop", { taskId }),
    onSuccess: (response) => {
      void queryClient.invalidateQueries({ queryKey: ["investigationTasks"] });
      void queryClient.invalidateQueries({ queryKey: ["investigationTask", response.task.id] });
    },
  });
  const createTask = useMutation({
    mutationFn: (input: InvestigationTaskCreateInput) => callDesktop("investigation_task_create", { input }),
    onSuccess: (response) => {
      setObjective("");
      setTaskMode("readonly");
      setAutomationLevel("readonly");
      setPermissionMode("ask");
      setNotBeforeAt("");
      setExpiresAt("");
      setForbiddenTools("");
      setForbiddenPathPrefixes("");
      void queryClient.invalidateQueries({ queryKey: ["investigationTasks"] });
      selectTask(response.task.id);
      // Readonly tasks and an explicitly delegated executable goal can start
      // immediately. Plan/ask-mode goals remain an explicit user action.
      if (shouldAutoStartCreatedTask(response.task)) startTask.mutate(response.task.id);
    },
  });
  const selectBrief = useMutation({
    mutationFn: (input: InvestigationBriefSelectInput) => callDesktop("investigation_brief_select", { input }),
    onSuccess: (response, input) => {
      void queryClient.invalidateQueries({ queryKey: ["investigationTasks"] });
      void queryClient.invalidateQueries({ queryKey: ["investigationTask", selectedTaskId] });
      // A brief may opt into exactly one bounded continuation. Legacy options
      // omit the field and are normalized to wait_user by the host, so a stale
      // or hand-crafted brief can never auto-start a run accidentally.
      if (response.continuation === "continue_readonly" || response.continuation === "start_plan") {
        startTask.mutate(input.taskId);
      }
    },
  });
  const recoverTask = useMutation({
    mutationFn: (input: { taskId: string; optionId?: string }) => callDesktop("investigation_task_recover", { input }),
    onSuccess: (response) => {
      void queryClient.invalidateQueries({ queryKey: ["investigationTasks"] });
      void queryClient.invalidateQueries({ queryKey: ["investigationTask", response.task.id] });
      // Retry/replan/resume recovery choices are host-owned lifecycle decisions.
      // Once the host has invalidated the old run and returned the task to
      // `investigating`, continue through the same durable start entry point;
      // rollback, wait-user and stop recoveries return other states and stay
      // visible for the next explicit decision.
      if (shouldAutoStartRecoveredTask(response.task)) {
        startTask.mutate(response.task.id);
      }
    },
  });
  const autoRecoveryAttempt = useRef<string | undefined>(undefined);
  useEffect(() => {
    if (!shell || !tasks.data || recoverTask.isPending) return;
    const candidate = tasks.data.tasks.find(shouldAutoRecoverDelegatedTask);
    const failure = candidate?.lastFailure;
    if (!candidate || !failure) return;
    const attemptKey = `${candidate.id}:${failure.attempt}:${failure.code}`;
    if (autoRecoveryAttempt.current === attemptKey) return;
    autoRecoveryAttempt.current = attemptKey;
    // The host increments the recovery attempt, invalidates stale plan state,
    // and returns `investigating`; the existing success path then starts the
    // next run through the same durable entry point.
    recoverTask.mutate({ taskId: candidate.id });
  }, [recoverTask.isPending, recoverTask.mutate, shell, tasks.data]);
  const createSchedule = useMutation({
    mutationFn: (taskId: string) => callDesktop("investigation_schedule_create", {
      input: { taskId, intervalSeconds: 300, notificationPolicy: "on_change" },
    }),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ["investigationSchedules"] }),
  });
  const updateSchedule = useMutation({
    mutationFn: (input: InvestigationScheduleUpdateInput) => callDesktop("investigation_schedule_update", { input }),
    onSuccess: (response) => {
      void queryClient.invalidateQueries({ queryKey: ["investigationSchedules"] });
      void queryClient.invalidateQueries({ queryKey: ["investigationScheduleRuns", response.schedule.id] });
    },
  });
  const retentionPreview = useMutation({
    mutationFn: (taskId: string) => callDesktop("investigation_retention_preview", { taskId }),
  });
  const retentionPrune = useMutation({
    mutationFn: (input: InvestigationRetentionPruneInput) => callDesktop("investigation_retention_prune", { input }),
    onSuccess: (response) => {
      void queryClient.invalidateQueries({ queryKey: ["investigationTasks"] });
      void queryClient.invalidateQueries({ queryKey: ["investigationTask", response.result.taskId] });
      retentionPreview.reset();
    },
  });

  const selectedServer = servers.data?.find((server) => server.id === selectedServerId);
  const scope = useMemo(
    () => selectedServer
      ? { host: "remote" as const, serverId: selectedServer.id, environment: selectedServer.metadata.environment }
      : { host: "local" as const, environment: "local" as const },
    [selectedServer],
  );
  const autoDelegationEligible = canDelegateAgentAuto({
    scope,
    mode: taskMode,
    automationLevel,
  });

  useEffect(() => {
    if (!autoDelegationEligible) setPermissionMode("ask");
  }, [autoDelegationEligible]);

  if (!shell) return <PreviewEmpty icon="agent" body="排查任务和证据只在 Tauri 桌面壳中可用，浏览器预览不会伪造远端数据。" />;
  if (tasks.isLoading) return <LoadingPanel title="正在读取排查任务" hint="从本地数据库加载任务与状态" />;
  if (tasks.isError || !tasks.data) {
    return <ErrorPanel showIcon title="无法读取排查任务" message={errorMessage(tasks.error)} onRetry={() => void tasks.refetch()} />;
  }

  const taskRows = tasks.data.tasks;
  return (
    <div className="investigations-page">
      <section className="investigations-header">
        <div>
          <p className="eyebrow">证据驱动的自动化</p>
          <h2>排查任务</h2>
          <p>先让 Agent 收集可复核证据，再把风险、选项和下一步交给你决定。</p>
        </div>
        <button type="button" className="button-secondary" onClick={() => void tasks.refetch()} disabled={tasks.isFetching}>
          <Icon name="refresh" size="sm" /> {tasks.isFetching ? "读取中" : "刷新"}
        </button>
      </section>

      {scheduleNotification ? (
        <section className="investigation-notification" role="status">
          <div>
            <strong>{scheduleNotification.title}</strong>
            <small>持续巡检 · {scheduleNotification.outcome} · {scheduleNotification.at}</small>
          </div>
          <button type="button" className="button-secondary" onClick={() => setScheduleNotification(null)}>知道了</button>
        </section>
      ) : null}

      <section className="investigation-create-card" aria-labelledby="investigation-create-title">
        <div>
          <p className="eyebrow">新目标</p>
          <h3 id="investigation-create-title">创建一个可恢复目标</h3>
        </div>
        <label className="field-label" htmlFor="investigation-objective">目标</label>
        <textarea
          id="investigation-objective"
          className="text-input investigation-objective"
          value={objective}
          onChange={(event) => setObjective(event.target.value)}
          placeholder="例如：确认 staging API 延迟升高的原因"
          rows={2}
        />
        <label className="field-label" htmlFor="investigation-criteria">完成标准（每行一条）</label>
        <textarea
          id="investigation-criteria"
          className="text-input investigation-criteria"
          value={criteria}
          onChange={(event) => setCriteria(event.target.value)}
          rows={3}
        />
        <div className="investigation-create-options">
          <div>
            <label className="field-label" htmlFor="investigation-mode">运行边界</label>
            <select id="investigation-mode" className="text-input" value={taskMode} onChange={(event) => {
              const next = event.target.value as AgentRunMode;
              setTaskMode(next);
              if (next === "readonly") setAutomationLevel("readonly");
            }}>
              <option value="readonly">只读排查</option>
              <option value="plan">计划 / dry-run</option>
              <option value="goal">目标执行</option>
            </select>
          </div>
          <div>
            <label className="field-label" htmlFor="investigation-automation">任务自动化级别</label>
            <select id="investigation-automation" className="text-input" value={automationLevel} disabled={taskMode === "readonly"} onChange={(event) => setAutomationLevel(event.target.value as TaskAutomationLevel)}>
              <option value="readonly">只采集，不执行变更</option>
              <option value="propose">先给方案，选择后执行</option>
              <option value="execute">按计划推进，变更仍受审批</option>
            </select>
          </div>
          <div>
            <label className="field-label" htmlFor="investigation-permission">动作授权方式</label>
            <select
              id="investigation-permission"
              className="text-input"
              value={permissionMode}
              onChange={(event) => setPermissionMode(event.target.value as AgentPermissionMode)}
            >
              <option value="ask">逐项询问</option>
              <option value="auto" disabled={!autoDelegationEligible}>受限自动推进</option>
            </select>
            <small className="project-muted">
              {autoDelegationEligible
                ? "仅开发/预发布的 write 档位可由 Agent 自动批准；危险动作仍逐项询问。"
                : "只有远程开发/预发布的目标执行任务可开启；当前仍逐项询问。"}
            </small>
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
        <div className="investigation-create-footer">
          <span className="project-muted">范围：{selectedServer ? `${selectedServer.name} · ${automationLevel === "execute" ? "可按计划执行" : "先观察/提案"}` : "本机 · 只读"}</span>
          <button
            type="button"
            className="button-primary"
            disabled={!objective.trim() || !criteria.trim() || createTask.isPending}
            onClick={() => createTask.mutate({
              objective: objective.trim(),
              successCriteria: criteria.split("\n").map((line) => line.trim()).filter(Boolean),
              scope,
              guardrails: {
                notBeforeAt: localInputToIso(notBeforeAt),
                expiresAt: localInputToIso(expiresAt),
                forbiddenTools: splitLines(forbiddenTools),
                forbiddenPathPrefixes: splitLines(forbiddenPathPrefixes),
              },
              mode: taskMode,
              permissionMode: effectiveTaskPermissionMode(permissionMode, { scope, mode: taskMode, automationLevel }),
              automationLevel,
            })}
          >
            <Icon name="plus" size="sm" /> {createTask.isPending ? "创建中" : "创建任务"}
          </button>
        </div>
        {createTask.isError ? <p className="form-error" role="alert">{errorMessage(createTask.error)}</p> : null}
      </section>

      {taskRows.length === 0 ? (
        <EmptyPanel extraClass="investigation-empty" icon="agent" title="还没有排查任务" body="创建一个只读目标后，Agent 会把每次采集结果绑定到同一条证据链。" />
      ) : (
        <div className="investigation-layout">
          <ul className="investigation-list" aria-label="排查任务列表">
            {taskRows.map((task) => (
              <TaskRow key={task.id} task={task} selected={task.id === selectedTaskId} onSelect={() => selectTask(task.id)} />
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
              onStart={() => startTask.mutate(detail.data.task.id)}
              starting={startTask.isPending && startTask.variables === detail.data.task.id}
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
          ) : (
            <div className="investigation-detail-placeholder">选择一个任务查看证据、发现和决策摘要。</div>
          )}
        </div>
      )}
    </div>
  );
}

function TaskRow({ task, selected, onSelect }: { task: InvestigationTask; selected: boolean; onSelect: () => void }) {
  return (
    <li>
      <button type="button" className={`investigation-row ${selected ? "investigation-row-selected" : ""}`} onClick={onSelect} aria-pressed={selected}>
        <span className={`investigation-status investigation-status-${task.status}`}>{STATUS_LABEL[task.status]}</span>
        <strong>{task.objective}</strong>
        <small>{task.scope.host === "remote" ? task.scope.serverId ?? "远端服务器" : "本机"} · {task.successCriteria.length} 项完成标准</small>
      </button>
    </li>
  );
}

function InvestigationDetail({
  task,
  evidence,
  findings,
  artifacts,
  decisionBrief,
  runs,
  steps,
  plan,
  schedules,
  scheduleRuns,
  onStart,
  starting,
  startError,
  onStop,
  stopping,
  stopError,
  onCreateSchedule,
  onUpdateSchedule,
  scheduling,
  scheduleError,
  retentionPreview,
  onPreviewRetention,
  previewingRetention,
  retentionPreviewError,
  onPruneRetention,
  pruningRetention,
  retentionPruneError,
  onContinue,
  onSelectOption,
  onRecover,
  recovering,
  recoveryError,
  selectingOptionId,
  selectionError,
}: {
  task: InvestigationTask;
  evidence: Evidence[];
  findings: Finding[];
  artifacts: InvestigationArtifact[];
  decisionBrief?: DecisionBrief;
  runs: InvestigationRun[];
  steps: InvestigationStep[];
  plan?: InvestigationPlan;
  schedules: InvestigationSchedule[];
  scheduleRuns: InvestigationScheduleRun[];
  onStart: () => void;
  starting: boolean;
  startError?: string;
  onStop: () => void;
  stopping: boolean;
  stopError?: string;
  onCreateSchedule: () => void;
  onUpdateSchedule: (input: InvestigationScheduleUpdateInput) => void;
  scheduling: boolean;
  scheduleError?: string;
  retentionPreview?: InvestigationRetentionPreview;
  onPreviewRetention: () => void;
  previewingRetention: boolean;
  retentionPreviewError?: string;
  onPruneRetention: (input: InvestigationRetentionPruneInput) => void;
  pruningRetention: boolean;
  retentionPruneError?: string;
  onContinue: () => void;
  onSelectOption: (optionId: string) => void;
  onRecover: (optionId?: string) => void;
  recovering: boolean;
  recoveryError?: string;
  selectingOptionId?: string;
  selectionError?: string;
}) {
  const canRecover = task.status === "failed" || task.status === "stopped" || Boolean(task.lastFailure);
  const canSchedule = task.mode === "readonly" && task.automationLevel === "readonly" && !["completed", "failed", "stopped", "expired"].includes(task.status);
  const canStart = (task.status === "pending" || task.status === "waiting_user") && !task.activeRunId;
  const canStop = !["completed", "failed", "stopped", "expired"].includes(task.status);
  return (
    <section className="investigation-detail" aria-labelledby="investigation-detail-title">
      <div className="investigation-detail-header">
        <div>
          <p className="eyebrow">{STATUS_LABEL[task.status]}</p>
          <h3 id="investigation-detail-title">{task.objective}</h3>
        </div>
        <div className="investigation-detail-actions">
          {canStart ? (
            <button type="button" className="button-primary" onClick={onStart} disabled={starting}>
              <Icon name="agent" size="sm" /> {starting ? "启动中" : task.mode === "readonly" ? "开始自主排查" : "开始目标任务"}
            </button>
          ) : null}
          {canStop ? (
            <button type="button" className="button-secondary" onClick={onStop} disabled={stopping}>
              <Icon name="stop" size="sm" /> {stopping ? "停止中" : "停止任务"}
            </button>
          ) : null}
          <button type="button" className="button-primary" onClick={onContinue}><Icon name="agent" size="sm" /> 在 Agent 中继续</button>
          {canRecover ? (
            <button type="button" className="button-secondary" onClick={() => onRecover()} disabled={recovering}>
              <Icon name="refresh" size="sm" /> {recovering ? "恢复中" : "恢复任务"}
            </button>
          ) : null}
        </div>
      </div>
      {startError ? <p className="form-error" role="alert">启动任务失败：{startError}</p> : null}
      {stopError ? <p className="form-error" role="alert">停止任务失败：{stopError}</p> : null}
      <div className="investigation-metrics">
        <span><strong>{evidence.length}</strong> 条证据</span>
        <span><strong>{findings.length}</strong> 个发现</span>
        <span><strong>{decisionBrief?.options.length ?? 0}</strong> 个选项</span>
        <span><strong>{runs.length}</strong> 次运行</span>
        <span><strong>{steps.length}</strong> 个步骤</span>
      </div>
      {task.lastFailure ? (
        <div className="investigation-failure" role="status">
          <strong>上一轮未完成：{task.lastFailure.code}</strong>
          <p>{task.lastFailure.message}</p>
          <small>第 {task.lastFailure.attempt} 次尝试 · {task.lastFailure.retryable ? "可重试" : "需要重新判断"}</small>
          {task.lastFailure.options?.length ? (
            <div className="investigation-failure-options">
              {task.lastFailure.options.map((option) => (
                <span key={option.id} className="investigation-failure-option">
                  <strong>{option.title}</strong>
                  <small>{option.description}{option.requiresApproval ? " · 需要批准" : ""}</small>
                  {option.action !== "inspect" ? (
                    <button type="button" className="button-secondary" onClick={() => onRecover(option.id)} disabled={recovering}>
                      {recovering ? "处理中" : option.title}
                    </button>
                  ) : null}
                </span>
              ))}
            </div>
          ) : null}
        </div>
      ) : null}
      {recoveryError ? <p className="form-error" role="alert">恢复失败：{recoveryError}</p> : null}
      {runs.length > 0 ? (
        <div className="investigation-runs">
          <span className="project-card-label">运行记录</span>
          {runs.slice(0, 5).map((run) => (
            <div key={run.id} className="investigation-run-row">
              <span>第 {run.attempt} 次 · {run.status}</span>
              <small>{run.updatedAt}{run.failure ? ` · ${run.failure.code}` : ""}</small>
            </div>
          ))}
        </div>
      ) : null}
      <div className="investigation-criteria">
        <span className="project-card-label">完成标准</span>
        <ul>{task.successCriteria.map((criterion) => <li key={criterion}>{criterion}</li>)}</ul>
      </div>
      <div className="investigation-guardrails-view">
        <span className="project-card-label">宿主硬边界</span>
        <small className="project-muted">
          时间窗：{task.guardrails.notBeforeAt ?? "立即"} → {task.guardrails.expiresAt ?? "未设置截止"}
        </small>
        <small className="project-muted">
          禁止工具：{task.guardrails.forbiddenTools.length ? task.guardrails.forbiddenTools.join(", ") : "无"}
        </small>
        <small className="project-muted">
          禁止路径：{task.guardrails.forbiddenPathPrefixes.length ? task.guardrails.forbiddenPathPrefixes.join(", ") : "无"}
        </small>
      </div>
      {plan ? (
        <div className="investigation-plan">
          <span className="project-card-label">当前计划 · 修订 {plan.revision}</span>
          <small className="project-muted">
            计划审批：{plan.approval?.status === "approved" ? `已批准${plan.approval.optionId ? `（方案 ${plan.approval.optionId}）` : ""}` : plan.approval?.status === "rejected" ? "已拒绝" : "等待用户选择"}
          </small>
          {plan.observationWindow ? (
            <div className="investigation-observation" role="status">
              <small className="project-muted">
                观察窗口：{OBSERVATION_STATUS_LABEL[plan.observationWindow.status]} · {plan.observationWindow.sampleCount} 次采样 · 每 {plan.observationWindow.intervalSeconds}s · 总计 {plan.observationWindow.durationSeconds}s
                {plan.observationWindow.deadlineAt ? ` · 截止 ${plan.observationWindow.deadlineAt}` : ""}
              </small>
              <small className="project-muted">限定工具：{plan.observationWindow.allowedTools.join(", ")}</small>
              <small className="project-muted">成功标准：{plan.observationWindow.successCriteria.join("；")}</small>
              {plan.observationWindow.lastFailure ? <small className="form-error">最近异常：{plan.observationWindow.lastFailure}</small> : null}
            </div>
          ) : null}
          {plan.steps.map((step) => (
            <div key={step.id} className="investigation-plan-row">
              <span>{step.ordinal + 1}. {step.title}</span>
              <small>{step.status} · {step.kind} · {step.allowedTools.join(", ")} · {step.attempts}/{step.maxAttempts}{step.riskLevel ? ` · ${step.riskLevel}` : ""}</small>
              {step.preview ? <small>预览：{step.preview}</small> : null}
              {step.verificationCriteria?.length ? <small>验证：{step.verificationCriteria.join("；")}</small> : null}
              {step.rollback ? <small>回退：{step.rollback}</small> : null}
            </div>
          ))}
        </div>
      ) : (
        <p className="project-muted">当前还没有活动计划；Agent 必须先声明计划才能继续使用任务工具。</p>
      )}
      <div className="investigation-schedules">
        <span className="project-card-label">持续巡检</span>
        {scheduleError ? <p className="form-error" role="alert">持续巡检设置失败：{scheduleError}</p> : null}
        {schedules.length === 0 ? (
          <p className="project-muted">没有后台触发器。只读任务可以按固定间隔收集证据，异常仍会停在等待用户状态。</p>
        ) : schedules.map((schedule) => (
          <div key={schedule.id} className="investigation-schedule-row">
            <span>{scheduleStatusLabel(schedule.status)} · {scheduleIntervalLabel(schedule.intervalSeconds)} · 下次 {schedule.nextRunAt}</span>
            <div className="investigation-schedule-actions">
              {schedule.status !== "revoked" ? (
                <>
                  {schedule.status === "active" ? (
                    <button type="button" className="button-secondary" onClick={() => onUpdateSchedule({ scheduleId: schedule.id, status: "paused" })} disabled={scheduling}>暂停</button>
                  ) : (
                    <button type="button" className="button-secondary" onClick={() => onUpdateSchedule({ scheduleId: schedule.id, status: "active" })} disabled={scheduling}>恢复</button>
                  )}
                  <button type="button" className="button-secondary" onClick={() => onUpdateSchedule({ scheduleId: schedule.id, status: "revoked" })} disabled={scheduling}>撤销</button>
                </>
              ) : <span className="project-muted">已撤销</span>}
            </div>
            <div className="investigation-schedule-settings">
              <label>
                <span>频率</span>
                <select
                  className="text-input"
                  value={schedule.intervalSeconds}
                  onChange={(event) => onUpdateSchedule({ scheduleId: schedule.id, intervalSeconds: Number(event.target.value) })}
                  disabled={scheduling || schedule.status === "revoked"}
                >
                  {scheduleIntervalOptions(schedule.intervalSeconds).map((option) => (
                    <option key={option.seconds} value={option.seconds}>{option.label}</option>
                  ))}
                </select>
              </label>
              <label>
                <span>通知</span>
                <select
                  className="text-input"
                  value={schedule.notificationPolicy}
                  onChange={(event) => onUpdateSchedule({ scheduleId: schedule.id, notificationPolicy: event.target.value as InvestigationNotificationPolicy })}
                  disabled={scheduling || schedule.status === "revoked"}
                >
                  {INVESTIGATION_NOTIFICATION_POLICIES.map((policy) => (
                    <option key={policy} value={policy}>{notificationPolicyLabel(policy)}</option>
                  ))}
                </select>
              </label>
            </div>
            <small className="investigation-schedule-baseline">
              <span>
                {notificationPolicyLabel(schedule.notificationPolicy)} · {schedule.baselineRunId ? `固定基线 ${schedule.baselineRunId}` : "基线自动取最近成功样本"}
                · 最近 {schedule.lastOutcome ?? "尚未运行"}{schedule.lastError ? ` · ${schedule.lastError}` : ""}
              </span>
              {schedule.baselineRunId && schedule.status !== "revoked" ? (
                <button
                  type="button"
                  className="button-secondary"
                  onClick={() => onUpdateSchedule({ scheduleId: schedule.id, baselineRunId: null })}
                  disabled={scheduling}
                >
                  改用最近样本
                </button>
              ) : null}
            </small>
          </div>
        ))}
        {canSchedule && schedules.every((schedule) => schedule.status === "revoked") ? (
          <button type="button" className="button-secondary" onClick={onCreateSchedule} disabled={scheduling}>{scheduling ? "保存中" : "开启每 5 分钟只读巡检"}</button>
        ) : null}
        {scheduleRuns.length > 0 ? (
          <div className="investigation-schedule-runs">
            <small className="project-muted">最近运行</small>
            {scheduleRuns.slice(0, 5).map((run) => (
              <div key={run.id} className="investigation-schedule-run-row">
                <span>{run.status} · {run.outcome ?? "进行中"}{schedules.some((schedule) => schedule.baselineRunId === run.id) ? " · 当前基线" : ""}</span>
                <span className="investigation-schedule-run-meta">
                  <small>{run.scheduledAt}{run.error ? ` · ${run.error}` : ""}</small>
                  {run.status === "succeeded" ? (
                    <button
                      type="button"
                      className="button-secondary"
                      onClick={() => onUpdateSchedule({ scheduleId: run.scheduleId, baselineRunId: run.id })}
                      disabled={scheduling || schedules.some((schedule) => schedule.baselineRunId === run.id)}
                    >
                      {schedules.some((schedule) => schedule.baselineRunId === run.id) ? "当前基线" : "设为基线"}
                    </button>
                  ) : null}
                </span>
              </div>
            ))}
          </div>
        ) : null}
      </div>
      <div className="investigation-findings">
        <span className="project-card-label">结构化判断</span>
        {findings.length === 0 ? (
          <p className="project-muted">还没有带证据引用的事实或推断。</p>
        ) : (
          findings.map((finding) => (
            <article key={finding.id} className="investigation-finding">
              <div className="investigation-finding-heading">
                <strong>{finding.title}</strong>
                <span>{finding.kind} · 置信度 {finding.confidence}</span>
              </div>
              <p>{finding.statement}</p>
              <small>
                依据：{finding.evidenceIds.length ? finding.evidenceIds.join(", ") : "未引用证据"}
                {finding.nextVerification ? ` · 下一步：${finding.nextVerification}` : ""}
              </small>
            </article>
          ))
        )}
      </div>
      <div className="investigation-evidence">
        <span className="project-card-label">证据（已脱敏）</span>
        {evidence.length === 0 ? (
          <p className="project-muted">Agent 还没有保存证据。</p>
        ) : (
          evidence.map((item) => <EvidenceCard key={item.id} evidence={item} />)
        )}
      </div>
      <div className="investigation-artifacts">
        <span className="project-card-label">阶段工件</span>
        {artifacts.length === 0 ? (
          <p className="project-muted">还没有保存执行、验证或失败工件。</p>
        ) : (
          artifacts.map((artifact) => (
            <details key={artifact.id} className="investigation-artifact-card">
              <summary>
                <strong>{artifact.title}</strong>
                <span>{artifact.kind} · {artifact.status}</span>
              </summary>
              <p>{artifact.summary}</p>
              <code>{previewEvidence(artifact.content)}</code>
              <small>{artifact.id} · {artifact.updatedAt}{artifact.evidenceIds.length ? ` · 依据 ${artifact.evidenceIds.join(", ")}` : ""}</small>
            </details>
          ))
        )}
      </div>
      <RetentionPanel
        task={task}
        preview={retentionPreview}
        onPreview={onPreviewRetention}
        previewing={previewingRetention}
        previewError={retentionPreviewError}
        onPrune={onPruneRetention}
        pruning={pruningRetention}
        pruneError={retentionPruneError}
      />
      {decisionBrief ? (
        <div className="investigation-brief">
          <span className="project-card-label">最新决策摘要</span>
          {decisionBrief.options.map((option) => (
            <article key={option.id}>
              <div className="investigation-option-heading">
                <strong>{option.title}</strong>
                <span>{option.riskLevel} · {option.status === "selected" ? "已选择" : option.status === "rejected" ? "未选择" : "可选择"}</span>
              </div>
              <p>{option.summary}</p>
              <small>影响：{option.impact} · 验证：{option.verification}</small>
              <small>选择后：{continuationLabel(option.continuation)}</small>
              {option.rollback ? <small>回退：{option.rollback}</small> : null}
              {option.status === "available" ? (
                <button
                  type="button"
                  className="button-secondary investigation-option-button"
                  onClick={() => onSelectOption(option.id)}
                  disabled={Boolean(selectingOptionId)}
                >
                  {selectingOptionId === option.id ? "记录中" : "选择此方案"}
                </button>
              ) : null}
            </article>
          ))}
          {selectingOptionId ? <p className="project-muted">选择会记录决策并按上面的续接意图推进；不会绕过计划门控或逐项批准直接执行远端变更。</p> : null}
          {selectionError ? <p className="form-error" role="alert">记录选择失败：{selectionError}</p> : null}
        </div>
      ) : <p className="project-muted">Agent 完成证据整理后，这里会出现可比较的处理选项。</p>}
    </section>
  );
}

function RetentionPanel({
  task,
  preview,
  onPreview,
  previewing,
  previewError,
  onPrune,
  pruning,
  pruneError,
}: {
  task: InvestigationTask;
  preview?: InvestigationRetentionPreview;
  onPreview: () => void;
  previewing: boolean;
  previewError?: string;
  onPrune: (input: InvestigationRetentionPruneInput) => void;
  pruning: boolean;
  pruneError?: string;
}) {
  const [confirming, setConfirming] = useState(false);
  useEffect(() => setConfirming(false), [task.id]);
  const terminal = ["completed", "failed", "stopped", "expired"].includes(task.status);
  if (!terminal) return null;

  const candidates = preview?.candidates ?? [];
  return (
    <div className="investigation-retention">
      <div className="investigation-retention-heading">
        <div>
          <span className="project-card-label">历史保留</span>
          <small className="project-muted">只检查本机数据库中的旧证据与已替代工件，不会触碰远端备份。</small>
        </div>
        <button type="button" className="button-secondary" onClick={() => { setConfirming(false); onPreview(); }} disabled={previewing || pruning}>
          {previewing ? "检查中" : "查看可清理历史"}
        </button>
      </div>
      {previewError ? <p className="form-error" role="alert">检查历史失败：{previewError}</p> : null}
      {preview ? (
        <>
          <small className="project-muted">
            截止 {preview.cutoffAt} · {candidates.length} 项候选 · {formatBytes(preview.candidateBytes)} · 已保护 {preview.protectedCount} 条引用历史
            {preview.truncated ? " · 列表已截断，请缩短范围后再清理" : ""}
          </small>
          {candidates.length === 0 ? (
            <p className="project-muted">没有满足条件的未引用证据或已替代工件。</p>
          ) : (
            <>
              <ul className="investigation-retention-list">
                {candidates.map((item) => (
                  <li key={`${item.kind}:${item.id}`}>
                    <span>{item.kind === "evidence" ? "证据" : "工件"} · {item.id}</span>
                    <small>{item.createdAt} · {formatBytes(item.bytes)} · {item.reason === "unreferenced_evidence" ? "未被历史引用" : "已被替代"}</small>
                  </li>
                ))}
              </ul>
              {preview.truncated ? null : confirming ? (
                <div className="investigation-retention-confirm">
                  <small>确认后只删除上面这次预览列出的 {candidates.length} 项，并在事务中再次检查引用关系。</small>
                  <div>
                    <button type="button" className="button-secondary" onClick={() => setConfirming(false)} disabled={pruning}>取消</button>
                    <button
                      type="button"
                      className="button-danger"
                      onClick={() => onPrune({
                        taskId: task.id,
                        cutoffAt: preview.cutoffAt,
                        items: candidates.map(({ id, kind }) => ({ id, kind })),
                        confirmation: "delete_unreferenced",
                      })}
                      disabled={pruning}
                    >
                      {pruning ? "清理中" : "确认清理这些历史"}
                    </button>
                  </div>
                </div>
              ) : (
                <button type="button" className="button-secondary" onClick={() => setConfirming(true)} disabled={pruning}>清理未引用历史</button>
              )}
            </>
          )}
          {pruneError ? <p className="form-error" role="alert">清理历史失败：{pruneError}</p> : null}
        </>
      ) : null}
    </div>
  );
}

function EvidenceCard({ evidence }: { evidence: Evidence }) {
  const body = previewEvidence(evidence.content);
  const freshness = evidence.freshness;
  return (
    <details className="investigation-evidence-card">
      <summary>
        <strong>{evidence.sourceTool}</strong>
        <span>{evidence.kind} · {evidence.redactionStatus}{evidence.truncated ? " · 已截断" : ""}{freshness ? ` · ${freshnessLabel(freshness.status)}` : ""}</span>
      </summary>
      <p>{evidence.inputSummary || "无输入摘要"}</p>
      <code>{body}</code>
      <small>{evidence.id} · {evidence.collectedAt} · SHA-256 {evidence.contentHash.slice(0, 12)}…</small>
      {freshness ? (
        <small>
          新鲜度：{freshnessLabel(freshness.status)} · 评估于 {freshness.evaluatedAt}
          {freshness.ageSeconds !== undefined ? ` · 已过去 ${freshness.ageSeconds}s` : ""}
          {` · 策略 ${freshness.policy}（${freshness.staleAfterSeconds}s / ${freshness.expiresAfterSeconds}s）`}
          {freshness.reason ? ` · ${freshness.reason}` : ""}
        </small>
      ) : <small>新鲜度：主机尚未评估（历史/测试投影）</small>}
    </details>
  );
}

function freshnessLabel(status: NonNullable<Evidence["freshness"]>["status"]): string {
  return { fresh: "新鲜", stale: "已变旧", expired: "已过期", unknown: "未知" }[status];
}

function continuationLabel(continuation?: DecisionOptionContinuation): string {
  return {
    continue_readonly: "继续有限只读复核",
    start_plan: "启动已选择的计划",
    wait_user: "等待下一步输入",
    stop: "保持停止，不启动新运行",
  }[continuation ?? "wait_user"];
}

function previewEvidence(content: unknown): string {
  let text: string;
  if (typeof content === "string") {
    text = content;
  } else {
    try {
      text = JSON.stringify(content);
    } catch {
      text = String(content);
    }
  }
  return text.length > 1_200 ? `${text.slice(0, 1_200)}…` : text;
}

function formatBytes(bytes: number): string {
  if (bytes < 1_024) return `${bytes} B`;
  if (bytes < 1_024 * 1_024) return `${(bytes / 1_024).toFixed(1)} KB`;
  return `${(bytes / (1_024 * 1_024)).toFixed(1)} MB`;
}

function splitLines(value: string): string[] {
  return value.split("\n").map((line) => line.trim()).filter(Boolean);
}

function localInputToIso(value: string): string | undefined {
  if (!value.trim()) return undefined;
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime()) ? value : parsed.toISOString();
}
