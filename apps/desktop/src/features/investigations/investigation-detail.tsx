import type {
  DecisionBrief,
  Evidence,
  Finding,
  InvestigationArtifact,
  InvestigationNotificationPolicy,
  InvestigationPlan,
  InvestigationRetentionPreview,
  InvestigationRetentionPruneInput,
  InvestigationRun,
  InvestigationSchedule,
  InvestigationScheduleRun,
  InvestigationScheduleUpdateInput,
  InvestigationStep,
  InvestigationTask,
} from "@yukinal/shared";
import { useEffect, useState } from "react";

import { Icon } from "../../components/Icon.js";
import { NativeTaskApprovals } from "../agent/NativeTaskApprovals.js";
import { describeInvestigationFailure } from "./error-display.js";
import {
  OBSERVATION_STATUS_LABEL,
  STATUS_LABEL,
  STEP_KIND_LABEL,
  STEP_STATUS_LABEL,
  continuationLabel,
  formatBytes,
  freshnessLabel,
  previewEvidence,
} from "./investigations-format.js";
import {
  INVESTIGATION_NOTIFICATION_POLICIES,
  notificationPolicyLabel,
  scheduleIntervalLabel,
  scheduleIntervalOptions,
  scheduleStatusLabel,
} from "./schedule-ui.js";

export function InvestigationDetail({
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
  startDeferred,
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
  /** A start was requested while the task still owned a live run. */
  startDeferred?: boolean;
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
  const canRecover = (task.status === "failed" || task.status === "stopped" || Boolean(task.lastFailure))
    && task.lastFailure?.code !== "outcome_unknown";
  const canSchedule = task.mode === "readonly" && task.automationLevel === "readonly" && !["completed", "failed", "stopped", "expired"].includes(task.status);
  const canStart = (task.status === "pending" || task.status === "waiting_user") && !task.activeRunId;
  const canStop = !["completed", "failed", "stopped", "expired"].includes(task.status);
  const failureDisplay = task.lastFailure ? describeInvestigationFailure(task.lastFailure) : undefined;
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
          <button type="button" className="button-primary" onClick={onContinue}><Icon name="agent" size="sm" /> 继续对话</button>
          {canRecover ? (
            <button type="button" className="button-secondary" onClick={() => onRecover()} disabled={recovering}>
              <Icon name="refresh" size="sm" /> {recovering ? "恢复中" : "恢复任务"}
            </button>
          ) : null}
        </div>
      </div>
      {startError ? <p className="form-error" role="alert">启动任务失败：{startError}</p> : null}
      {startDeferred && !startError ? (
        <p className="form-hint" role="status">当前运行仍在进行，结束后会自动继续，无需再次点击。</p>
      ) : null}
      <NativeTaskApprovals taskId={task.id} />
      {stopError ? <p className="form-error" role="alert">停止任务失败：{stopError}</p> : null}
      <div className="investigation-metrics">
        <span><strong>{evidence.length}</strong> 条证据</span>
        <span><strong>{findings.length}</strong> 个发现</span>
        <span><strong>{decisionBrief?.options.length ?? 0}</strong> 个选项</span>
        <span><strong>{runs.length}</strong> 次运行</span>
        <span><strong>{steps.length}</strong> 个步骤</span>
      </div>
      {task.lastFailure && failureDisplay ? (
        <div className="investigation-failure" role="status">
          <strong>上一轮未完成：{failureDisplay.label}</strong>
          <p>{task.lastFailure.message}</p>
          <small>第 {task.lastFailure.attempt} 次尝试 · {task.lastFailure.retryable ? "可重试" : "需要重新判断"} · 下一步：{failureDisplay.nextStep}</small>
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
        <details className="investigation-runs investigation-section">
          <summary>运行记录 · {runs.length}</summary>
          {runs.slice(0, 5).map((run) => (
            <div key={run.id} className="investigation-run-row">
              <span>第 {run.attempt} 次 · {run.status}</span>
              <small>{run.updatedAt}{run.failure ? ` · ${run.failure.code}` : ""}</small>
            </div>
          ))}
        </details>
      ) : null}
      <div className="investigation-criteria">
        <span className="project-card-label">完成标准</span>
        <ul>{task.successCriteria.map((criterion) => <li key={criterion}>{criterion}</li>)}</ul>
      </div>
      <details className="investigation-guardrails-view investigation-section">
        <summary>执行边界</summary>
        <small className="project-muted">
          时间窗：{task.guardrails.notBeforeAt ?? "立即"} → {task.guardrails.expiresAt ?? "未设置截止"}
        </small>
        <small className="project-muted">
          禁止工具：{task.guardrails.forbiddenTools.length ? task.guardrails.forbiddenTools.join(", ") : "无"}
        </small>
        <small className="project-muted">
          禁止路径：{task.guardrails.forbiddenPathPrefixes.length ? task.guardrails.forbiddenPathPrefixes.join(", ") : "无"}
        </small>
        {task.guardrails.commandGrant ? (
          <small className="project-muted">
            命令授权：{task.guardrails.commandGrant.callsUsed}/{task.guardrails.commandGrant.maxCalls} 次 · 剩余时长 {Math.max(0, task.guardrails.commandGrant.maxTotalDurationMs - task.guardrails.commandGrant.totalDurationMs)} ms · 剩余输出 {Math.max(0, task.guardrails.commandGrant.maxTotalOutputBytes - task.guardrails.commandGrant.totalOutputBytes)} 字节 · 截止 {task.guardrails.commandGrant.expiresAt}
          </small>
        ) : null}
      </details>
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
        <p className="project-muted">暂无计划</p>
      )}
      {steps.length > 0 ? <InvestigationExecutionTimeline steps={steps} /> : null}
      <details className="investigation-schedules investigation-section" open={Boolean(scheduleError) || schedules.length > 0}>
        <summary>持续巡检 · {schedules.length}</summary>
        {scheduleError ? <p className="form-error" role="alert">持续巡检设置失败：{scheduleError}</p> : null}
        {schedules.length === 0 ? (
          <p className="project-muted">暂无巡检</p>
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
      </details>
      <details className="investigation-findings investigation-section" open={findings.length > 0}>
        <summary>发现 · {findings.length}</summary>
        {findings.length === 0 ? (
          <p className="project-muted">暂无发现</p>
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
      </details>
      <details className="investigation-evidence investigation-section" open={evidence.length > 0}>
        <summary>证据 · {evidence.length}</summary>
        {evidence.length === 0 ? (
          <p className="project-muted">暂无证据</p>
        ) : (
          evidence.map((item) => <EvidenceCard key={item.id} evidence={item} />)
        )}
      </details>
      <details className="investigation-artifacts investigation-section">
        <summary>阶段记录 · {artifacts.length}</summary>
        {artifacts.length === 0 ? (
          <p className="project-muted">暂无阶段记录</p>
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
      </details>
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
      ) : <p className="project-muted">暂无处理选项</p>}
    </section>
  );
}

function InvestigationExecutionTimeline({ steps }: { steps: InvestigationStep[] }) {
  return (
    <section className="investigation-execution-timeline">
      <span className="project-card-label">实际执行记录 · {steps.length} 步</span>
      <ol>
        {steps.slice(-40).map((step) => (
          <li key={`${step.runId}:${step.id}`} className={`investigation-execution-step investigation-execution-step-${step.status}`}>
            <div className="investigation-execution-heading">
              <strong>{step.ordinal + 1}. {step.toolName ?? step.title}</strong>
              <span>{STEP_STATUS_LABEL[step.status]} · {STEP_KIND_LABEL[step.kind]}</span>
            </div>
            <small>
              {step.target?.serverId ? `目标 ${step.target.serverId} · ` : ""}
              {step.startedAt ?? "开始时间未知"}{step.endedAt ? ` → ${step.endedAt}` : ""}
              {step.planStepId ? ` · 计划步骤 ${step.planStepId}` : ""}
            </small>
            {step.failure?.code === "outcome_unknown" ? (
              <p className="investigation-unknown-outcome" role="status">命令或变更可能已经生效。先核对远端状态，不要直接重试。</p>
            ) : null}
            {step.inputSummary ? <details><summary>实际输入摘要</summary><pre>{step.inputSummary}</pre></details> : null}
            {step.outputSummary ? <details open={step.status === "failed"}><summary>实际结果摘要</summary><pre>{step.outputSummary}</pre></details> : null}
            {step.failure && step.failure.code !== "outcome_unknown" ? <p className="investigation-step-failure">{step.failure.message}</p> : null}
          </li>
        ))}
      </ol>
      {steps.length > 40 ? <small className="project-muted">仅显示最近 40 步；完整活动记录保存在任务历史中。</small> : null}
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
