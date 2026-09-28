//! Projection of sidecar events into durable investigation state.
//!
//! This module owns the event fence and task/run transition table. It is kept separate from
//! sidecar transport and audit redaction so lifecycle bugs cannot silently change persistence.

use super::audit::*;
use super::*;

/// `agent.stream` 通知 → Tauri event（事件名 = AgentStreamEvent.type）。
pub(crate) fn forward_agent_frame(app: &AppHandle, frame: &Value) {
    let Some(method) = frame.get("method").and_then(Value::as_str) else {
        return;
    };
    if method != "agent.stream" {
        return;
    }
    let Some(params) = frame.get("params") else {
        return;
    };
    let Some(event_type) = params.get("type").and_then(Value::as_str) else {
        return;
    };
    if !matches!(
        event_type,
        "agent.started"
            | "agent.thinking"
            | "agent.text"
            | "agent.usage"
            | "agent.tool_call"
            | "agent.tool_result"
            | "agent.waiting_approval"
            | "agent.approval_expired"
            | "agent.completed"
            | "agent.failed"
    ) {
        return;
    }
    let Some(run_id) = params.get("runId").and_then(Value::as_str) else {
        return;
    };
    if run_id.trim().is_empty() || run_id.len() > 256 {
        return;
    }
    // The sidecar transport already caps a frame at 8 MiB. Keep a malformed
    // event from becoming a similarly large Tauri/UI allocation.
    if serde_json::to_vec(params)
        .map(|payload| payload.len() > 1_000_000)
        .unwrap_or(true)
    {
        return;
    }
    sync_investigation_task_status(app, event_type, params);
    persist_investigation_event(app, event_type, params);
    if event_type == "agent.tool_result" {
        persist_agent_tool_result(app, params);
    }
    let _ = app.emit(&tauri_event_name(event_type), params.clone());
}

/// Keep the durable task row in step with the terminal/approval events emitted by a run.
///
/// The sidecar reports what happened; the host still owns the state transition. A malformed or
/// stale event is ignored, and an event can never jump across the transition table in
/// `commands::investigation`.
pub(crate) fn sync_investigation_task_status(app: &AppHandle, event_type: &str, params: &Value) {
    let state = app.state::<AppState>();
    let _ = sync_investigation_task_status_state(&state, event_type, params);
}

/// Apply the host-owned task transition without needing a Tauri runtime. The
/// production event forwarder and the offline cross-layer tests both use this
/// exact state path; UI emission remains outside the helper.
pub(crate) fn sync_investigation_task_status_state(
    state: &AppState,
    event_type: &str,
    params: &Value,
) -> bool {
    let Some(task_id) = params.get("taskId").and_then(Value::as_str) else {
        return false;
    };
    if task_id.trim().is_empty() || task_id.len() > 256 {
        return false;
    }

    let current = match state.database.investigations().get_task(task_id) {
        Ok(task) => task,
        Err(error) => {
            eprintln!("[yukinal] investigation status lookup failed for {task_id}: {error}");
            return false;
        }
    };
    let Some(run_id) = params.get("runId").and_then(Value::as_str) else {
        return false;
    };
    // A task can only be advanced by the run currently admitted for it.  A
    // sidecar may flush a frame after the host has stopped/recovered the old
    // run; accepting that frame here would let stale completion/approval
    // events rewind a newer task state before the finalisation fence below.
    if !is_current_investigation_run(current.active_run_id.as_deref(), run_id) {
        return false;
    }
    let result_state = params
        .get("result")
        .and_then(|result| result.get("state"))
        .and_then(Value::as_str);
    let Some(next) = next_investigation_status(current.status, event_type, result_state) else {
        return false;
    };
    let completed_at = matches!(
        next,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Stopped | TaskStatus::Expired
    )
    .then(yukinal_core::sidecar::iso8601_now);
    if let Err(error) = state
        .database
        .investigations()
        .update_task_status_if_active(
            task_id,
            next,
            &yukinal_core::sidecar::iso8601_now(),
            completed_at.as_deref(),
            run_id,
        )
    {
        eprintln!("[yukinal] investigation status update failed for {task_id}: {error}");
        return false;
    }
    true
}

pub(crate) fn next_investigation_status(
    current: TaskStatus,
    event_type: &str,
    result_state: Option<&str>,
) -> Option<TaskStatus> {
    let candidate = match event_type {
        "agent.waiting_approval" => Some(TaskStatus::WaitingUser),
        "agent.failed" => Some(TaskStatus::Failed),
        "agent.completed" => match result_state {
            Some("cancelled") => Some(TaskStatus::Stopped),
            Some("failed") => Some(TaskStatus::Failed),
            Some("completed") if current == TaskStatus::Executing => Some(TaskStatus::Verifying),
            Some("completed")
                if matches!(
                    current,
                    TaskStatus::Investigating | TaskStatus::WaitingUser | TaskStatus::Verifying
                ) =>
            {
                Some(TaskStatus::WaitingUser)
            }
            _ => None,
        },
        _ => None,
    }?;
    investigation::can_transition(current, candidate).then_some(candidate)
}

/// Persist the durable run/step ledger independently of the UI event stream. Ordinary chat
/// runs have no row and are ignored; investigation runs are updated even when the window is
/// disconnected, so a later task detail request can explain what happened.
pub(crate) fn persist_investigation_event(app: &AppHandle, event_type: &str, params: &Value) {
    let state = app.state::<AppState>();
    let Some(run) = persist_investigation_event_state(&state, event_type, params) else {
        return;
    };
    if !matches!(
        run.status,
        InvestigationRunStatus::Completed
            | InvestigationRunStatus::Failed
            | InvestigationRunStatus::Cancelled
    ) {
        return;
    }

    let schedule = match state
        .database
        .investigations()
        .get_schedule_for_run(&run.id)
    {
        Ok(schedule) => Some(schedule),
        Err(yukinal_database::DatabaseError::NotFound) => None,
        Err(error) => {
            eprintln!(
                "[yukinal] scheduled comparison lookup failed for {}: {error}",
                run.id
            );
            None
        }
    };
    let comparison = if schedule.is_some() && run.status == InvestigationRunStatus::Completed {
        match state
            .database
            .investigations()
            .compare_schedule_run(&run.id)
        {
            Ok(comparison) => Some(comparison),
            Err(error) => {
                eprintln!(
                    "[yukinal] scheduled evidence comparison failed for {}: {error}",
                    run.id
                );
                None
            }
        }
    } else {
        None
    };
    let change_material = if let Some(comparison) = comparison
        .as_ref()
        .filter(|value| value.status == InvestigationScheduleComparisonStatus::Changed)
    {
        persist_schedule_change_material(&state, &run, comparison, &run.updated_at)
    } else {
        None
    };
    let schedule_status = match run.status {
        InvestigationRunStatus::Completed => {
            yukinal_database::models::InvestigationScheduleRunStatus::Succeeded
        }
        InvestigationRunStatus::Failed => {
            yukinal_database::models::InvestigationScheduleRunStatus::Failed
        }
        InvestigationRunStatus::Cancelled => {
            yukinal_database::models::InvestigationScheduleRunStatus::Interrupted
        }
        _ => unreachable!(),
    };
    let outcome = match (run.status, comparison.as_ref()) {
        (InvestigationRunStatus::Completed, Some(comparison)) => Some(comparison.status.as_str()),
        (InvestigationRunStatus::Completed, None) => Some("completed"),
        (InvestigationRunStatus::Failed, _) => Some("failed"),
        (InvestigationRunStatus::Cancelled, _) => Some("cancelled"),
        _ => None,
    };
    let error = run.failure.as_ref().map(|failure| failure.message.as_str());
    // Ordinary user runs have no schedule row; the repository returns NotFound/
    // validation and we intentionally leave those rows alone.
    let _ = state.database.investigations().finish_schedule_run(
        &run.id,
        schedule_status,
        outcome,
        error,
        &run.updated_at,
    );
    if let Some(schedule) = schedule.as_ref() {
        let outcome = outcome.unwrap_or("completed");
        if schedule_notification_allowed(
            schedule.notification_policy,
            run.status,
            comparison.as_ref().map(|value| value.status),
        ) {
            let (evidence_ids, finding_id, brief_id, title) = change_material
                .as_ref()
                .map(|material| {
                    (
                        material.evidence_ids.clone(),
                        Some(material.finding_id.clone()),
                        Some(material.brief_id.clone()),
                        material.title.clone(),
                    )
                })
                .unwrap_or_else(|| {
                    (
                        Vec::new(),
                        None,
                        None,
                        if run.status == InvestigationRunStatus::Failed {
                            "持续巡检运行失败".to_string()
                        } else {
                            "持续巡检结果".to_string()
                        },
                    )
                });
            let _ = app.emit(
                &tauri_event_name("investigation.schedule_notification"),
                json!({
                    "scheduleId": schedule.id,
                    "scheduleRunId": run.id,
                    "taskId": run.task_id,
                    "outcome": outcome,
                    "notificationPolicy": schedule.notification_policy.as_str(),
                    "title": title,
                    "evidenceIds": evidence_ids,
                    "findingId": finding_id,
                    "briefId": brief_id,
                    "at": run.updated_at,
                }),
            );
        }
    }
}

/// Persist one sidecar event using only the host state. Keeping the run/step
/// transition here makes the same logic executable in a real forwarder and a
/// SQLite cross-layer fixture without constructing a Tauri window.
pub(crate) fn persist_investigation_event_state(
    state: &AppState,
    event_type: &str,
    params: &Value,
) -> Option<yukinal_database::models::InvestigationRun> {
    let run_id = params.get("runId").and_then(Value::as_str)?;
    let mut run = match state.database.investigations().get_run(run_id) {
        Ok(run) => run,
        Err(yukinal_database::DatabaseError::NotFound) => return None,
        Err(error) => {
            eprintln!("[yukinal] investigation run lookup failed for {run_id}: {error}");
            return None;
        }
    };
    // Recovery and terminal finalisation are host decisions. Once one of
    // those states is durable, a late sidecar frame is audit noise, not a new
    // fact; in particular it must never resurrect an interrupted run.
    if is_terminal_investigation_run(run.status) {
        return None;
    }
    if let Some(event_task_id) = params.get("taskId").and_then(Value::as_str) {
        if event_task_id != run.task_id {
            return None;
        }
    }
    let now = yukinal_core::sidecar::iso8601_now();
    if let Some(trace_id) = params.get("traceId").and_then(Value::as_str).or_else(|| {
        params
            .get("result")
            .and_then(|value| value.get("traceId"))
            .and_then(Value::as_str)
    }) {
        run.trace_id = Some(bounded_audit_text(trace_id, 256));
    }

    match event_type {
        "agent.started" => run.status = InvestigationRunStatus::Running,
        "agent.waiting_approval" => run.status = InvestigationRunStatus::WaitingUser,
        "agent.tool_call" => {
            if let Some(step) = investigation_step_from_tool_call(&run, params, state) {
                if let Err(error) = state.database.investigations().upsert_step(&step) {
                    eprintln!("[yukinal] investigation step insert failed: {error}");
                }
            }
            run.status = InvestigationRunStatus::Running;
        }
        "agent.tool_result" => {
            if let Some(step) = investigation_step_from_tool_result(&run, params, state) {
                if let Err(error) = state.database.investigations().upsert_step(&step) {
                    eprintln!("[yukinal] investigation step update failed: {error}");
                }
            }
        }
        "agent.completed" => {
            let result_state = params
                .get("result")
                .and_then(|result| result.get("state"))
                .and_then(Value::as_str);
            run.status = match result_state {
                Some("cancelled") => InvestigationRunStatus::Cancelled,
                Some("failed") => InvestigationRunStatus::Failed,
                _ => InvestigationRunStatus::Completed,
            };
            if matches!(run.status, InvestigationRunStatus::Failed) {
                run.failure = failure_from_event(params, run.attempt);
            } else if matches!(run.status, InvestigationRunStatus::Cancelled) {
                run.failure = Some(InvestigationFailure {
                    code: TaskFailureCode::Cancelled,
                    message: "运行被用户停止".into(),
                    retryable: true,
                    attempt: run.attempt,
                    at: params
                        .get("at")
                        .and_then(Value::as_str)
                        .map(|value| bounded_audit_text(value, 80))
                        .unwrap_or_else(yukinal_core::sidecar::iso8601_now),
                    detail: None,
                    options: Some(failure_options(TaskFailureCode::Cancelled, true)),
                });
            }
        }
        "agent.failed" => {
            run.status = InvestigationRunStatus::Failed;
            run.failure = failure_from_event(params, run.attempt);
        }
        _ => return None,
    }

    run.updated_at = now.clone();
    if matches!(
        run.status,
        InvestigationRunStatus::Completed
            | InvestigationRunStatus::Failed
            | InvestigationRunStatus::Cancelled
            | InvestigationRunStatus::Interrupted
    ) {
        run.ended_at = Some(now.clone());
    }
    run.checkpoint = Some(json!({
        "lastEvent": event_type,
        "at": now,
        "stepId": params.get("stepId").and_then(Value::as_str),
    }));
    if let Err(error) = state.database.investigations().update_run(&run) {
        eprintln!("[yukinal] investigation run update failed for {run_id}: {error}");
        return None;
    }

    // Close the task's active-run fence before emitting any schedule notification.
    // The UI may immediately select the notification's decision brief; leaving the
    // completed run active until after `app.emit` creates a race where the explicit
    // continuation is rejected as "already active".
    if matches!(
        run.status,
        InvestigationRunStatus::Completed
            | InvestigationRunStatus::Failed
            | InvestigationRunStatus::Cancelled
    ) {
        finalize_investigation_task_if_active(state, &run);
    }
    Some(run)
}

fn finalize_investigation_task_if_active(
    state: &AppState,
    run: &yukinal_database::models::InvestigationRun,
) {
    let Ok(task) = state.database.investigations().get_task(&run.task_id) else {
        return;
    };
    if task.active_run_id.as_deref() != Some(run.id.as_str()) {
        // A retry may have superseded this run. Its own ledger is still updated above,
        // but a late terminal frame must never rewind the task to an older outcome.
        return;
    }
    let phase = task_phase_for_status(task.status);
    let failure = run.failure.as_ref();
    if let Err(error) = state
        .database
        .investigations()
        .update_task_progress_if_active(
            &TaskProgressUpdate {
                id: &task.id,
                status: task.status,
                phase,
                active_run_id: None,
                last_failure: failure,
                updated_at: &run.updated_at,
                completed_at: task.completed_at.as_deref(),
            },
            &run.id,
        )
        .map(|_| ())
    {
        eprintln!(
            "[yukinal] investigation task finalisation failed for {}: {error}",
            task.id
        );
    }
}

pub(crate) fn is_terminal_investigation_run(status: InvestigationRunStatus) -> bool {
    matches!(
        status,
        InvestigationRunStatus::Completed
            | InvestigationRunStatus::Failed
            | InvestigationRunStatus::Cancelled
            | InvestigationRunStatus::Interrupted
    )
}

pub(crate) fn is_current_investigation_run(
    active_run_id: Option<&str>,
    event_run_id: &str,
) -> bool {
    active_run_id == Some(event_run_id)
}

#[derive(Debug, Clone)]
struct ScheduleChangeMaterial {
    finding_id: String,
    brief_id: String,
    evidence_ids: Vec<String>,
    title: String,
}

fn persist_schedule_change_material(
    state: &AppState,
    run: &yukinal_database::models::InvestigationRun,
    comparison: &yukinal_database::models::InvestigationScheduleComparison,
    now: &str,
) -> Option<ScheduleChangeMaterial> {
    let finding_id = format!("finding_schedule_{}", run.id);
    let brief_id = format!("brief_schedule_{}", run.id);
    let artifact_id = format!("artifact_schedule_{}", run.id);
    let title = "持续巡检发现数据变化".to_string();
    let statement = "本次只读巡检的证据指纹与上一轮成功样本不同；这只证明观测数据发生变化，不足以单独证明根因或授权变更。";
    let finding = Finding {
        id: finding_id.clone(),
        task_id: run.task_id.clone(),
        title: title.clone(),
        kind: FindingKind::Inference,
        statement: statement.into(),
        evidence_ids: comparison.current_evidence_ids.clone(),
        confidence: FindingConfidence::Low,
        next_verification: Some(
            "在相同范围内继续一次只读复核，并将变化与上一轮证据逐项对照".into(),
        ),
        created_at: now.into(),
    };
    match state.database.investigations().get_finding(&finding_id) {
        Ok(_) => {}
        Err(yukinal_database::DatabaseError::NotFound) => {
            if let Err(error) = state.database.investigations().add_finding(&finding) {
                eprintln!("[yukinal] scheduled finding persistence failed: {error}");
                return None;
            }
        }
        Err(error) => {
            eprintln!("[yukinal] scheduled finding lookup failed: {error}");
            return None;
        }
    }

    let brief = DecisionBrief {
        id: brief_id.clone(),
        task_id: run.task_id.clone(),
        plan_id: state
            .database
            .investigations()
            .latest_plan(&run.task_id)
            .ok()
            .flatten()
            .map(|plan| plan.id),
        generated_at: now.into(),
        status: DecisionBriefStatus::Presented,
        finding_ids: vec![finding_id.clone()],
        options: vec![
            DecisionOption {
                id: format!("option_schedule_inspect_{}", run.id),
                title: "继续有限只读复核".into(),
                summary: "保持原范围和只读策略，再收集一轮证据确认变化是否持续。".into(),
                impact: "不改变目标主机状态；会消耗本任务的只读预算。".into(),
                risk_level: RiskLevel::Read,
                evidence_ids: comparison.current_evidence_ids.clone(),
                finding_ids: vec![finding_id.clone()],
                preview: Some("只调用已保存任务允许的只读工具".into()),
                verification: "下一轮样本应能说明变化是否仍然存在".into(),
                rollback: None,
                requires_approval: false,
                status: DecisionOptionStatus::Available,
                continuation: Some(DecisionOptionContinuation::ContinueReadonly),
            },
            DecisionOption {
                id: format!("option_schedule_plan_{}", run.id),
                title: "等待人工决定是否规划变更".into(),
                summary: "暂不执行任何写入；如果只读复核确认问题，再由用户要求 Agent 生成新的 dry-run 方案。".into(),
                impact: "维持现状，避免把观测差异误当成变更授权。".into(),
                risk_level: RiskLevel::Low,
                evidence_ids: comparison.current_evidence_ids.clone(),
                finding_ids: vec![finding_id.clone()],
                preview: Some("仅生成方案，不直接执行远端写操作".into()),
                verification: "用户明确要求后，新的方案必须重新通过宿主审批".into(),
                rollback: Some("没有写入动作，因此当前没有回退操作".into()),
                requires_approval: true,
                status: DecisionOptionStatus::Available,
                continuation: Some(DecisionOptionContinuation::WaitUser),
            },
        ],
        selected_option_id: None,
    };
    let already_saved = state
        .database
        .investigations()
        .latest_decision_brief(&run.task_id)
        .ok()
        .flatten()
        .is_some_and(|saved| saved.id == brief_id);
    if !already_saved {
        if let Err(error) = state.database.investigations().save_decision_brief(&brief) {
            eprintln!("[yukinal] scheduled decision brief persistence failed: {error}");
            return None;
        }
    }

    let artifact = yukinal_database::models::InvestigationArtifact {
        id: artifact_id,
        task_id: run.task_id.clone(),
        run_id: Some(run.id.clone()),
        plan_id: brief.plan_id.clone(),
        plan_step_id: None,
        phase: yukinal_database::models::TaskPhase::Decision,
        kind: TaskArtifactKind::EvidenceSet,
        status: TaskArtifactStatus::Ready,
        title: title.clone(),
        summary: statement.into(),
        content: json!({
            "comparison": comparison,
            "source": "host.scheduler",
            "decisionBriefId": brief_id,
        }),
        evidence_ids: comparison.current_evidence_ids.clone(),
        created_at: now.into(),
        updated_at: now.into(),
    };
    if let Err(error) = state.database.investigations().upsert_artifact(&artifact) {
        eprintln!("[yukinal] scheduled evidence artifact persistence failed: {error}");
        return None;
    }
    Some(ScheduleChangeMaterial {
        finding_id,
        brief_id,
        evidence_ids: comparison.current_evidence_ids.clone(),
        title,
    })
}

fn schedule_notification_allowed(
    policy: yukinal_database::models::InvestigationNotificationPolicy,
    run_status: InvestigationRunStatus,
    comparison: Option<InvestigationScheduleComparisonStatus>,
) -> bool {
    match policy {
        yukinal_database::models::InvestigationNotificationPolicy::Silent => false,
        yukinal_database::models::InvestigationNotificationPolicy::Always => true,
        yukinal_database::models::InvestigationNotificationPolicy::FailedRunsOnly => {
            matches!(
                run_status,
                InvestigationRunStatus::Failed | InvestigationRunStatus::Cancelled
            )
        }
        yukinal_database::models::InvestigationNotificationPolicy::OnChange => {
            comparison == Some(InvestigationScheduleComparisonStatus::Changed)
                || matches!(
                    run_status,
                    InvestigationRunStatus::Failed | InvestigationRunStatus::Cancelled
                )
        }
    }
}

fn investigation_step_from_tool_call(
    run: &yukinal_database::models::InvestigationRun,
    params: &Value,
    state: &AppState,
) -> Option<InvestigationStep> {
    let id = params.get("stepId").and_then(Value::as_str)?.trim();
    let title = params.get("toolName").and_then(Value::as_str)?.trim();
    if id.is_empty() || title.is_empty() {
        return None;
    }
    let ordinal = state
        .database
        .investigations()
        .list_steps(&run.task_id, 512)
        .ok()
        .and_then(|steps| steps.iter().map(|step| step.ordinal).max())
        .unwrap_or(0)
        .saturating_add(1);
    let target = params
        .get("target")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok());
    let input_summary = params.get("input").map(|value| {
        safe_audit_summary(
            &serde_json::to_string(&sanitize_audit_input(value.clone())).unwrap_or_default(),
            4_096,
        )
    });
    let kind = if params.get("riskLevel").and_then(Value::as_str) == Some("read") {
        InvestigationStepKind::Evidence
    } else {
        InvestigationStepKind::Action
    };
    Some(InvestigationStep {
        id: bounded_audit_text(id, 256),
        task_id: run.task_id.clone(),
        run_id: run.id.clone(),
        ordinal,
        kind,
        title: bounded_audit_text(title, 512),
        status: InvestigationStepStatus::Running,
        attempt: 1,
        tool_name: Some(bounded_audit_text(title, 256)),
        plan_id: params
            .get("planId")
            .and_then(Value::as_str)
            .map(|value| bounded_audit_text(value, 256)),
        plan_step_id: params
            .get("planStepId")
            .and_then(Value::as_str)
            .map(|value| bounded_audit_text(value, 256)),
        target,
        input_summary,
        output_summary: None,
        evidence_ids: plan_evidence_ids_from_event(params).unwrap_or_default(),
        started_at: Some(
            params
                .get("at")
                .and_then(Value::as_str)
                .map(|value| bounded_audit_text(value, 80))
                .unwrap_or_else(yukinal_core::sidecar::iso8601_now),
        ),
        ended_at: None,
        failure: None,
    })
}

fn investigation_step_from_tool_result(
    run: &yukinal_database::models::InvestigationRun,
    params: &Value,
    state: &AppState,
) -> Option<InvestigationStep> {
    let id = params.get("stepId").and_then(Value::as_str)?.trim();
    if id.is_empty() {
        return None;
    }
    let mut step = state
        .database
        .investigations()
        .list_steps(&run.task_id, 512)
        .ok()
        .and_then(|steps| steps.into_iter().find(|candidate| candidate.id == id))
        .unwrap_or(InvestigationStep {
            id: bounded_audit_text(id, 256),
            task_id: run.task_id.clone(),
            run_id: run.id.clone(),
            ordinal: 0,
            kind: InvestigationStepKind::Evidence,
            title: bounded_audit_text(
                params
                    .get("toolName")
                    .and_then(Value::as_str)
                    .unwrap_or("tool"),
                512,
            ),
            status: InvestigationStepStatus::Pending,
            attempt: 1,
            tool_name: params
                .get("toolName")
                .and_then(Value::as_str)
                .map(|value| bounded_audit_text(value, 256)),
            plan_id: params
                .get("planId")
                .and_then(Value::as_str)
                .map(|value| bounded_audit_text(value, 256)),
            plan_step_id: params
                .get("planStepId")
                .and_then(Value::as_str)
                .map(|value| bounded_audit_text(value, 256)),
            target: params
                .get("target")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok()),
            input_summary: None,
            output_summary: None,
            evidence_ids: plan_evidence_ids_from_event(params).unwrap_or_default(),
            started_at: None,
            ended_at: None,
            failure: None,
        });
    let status = params
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("failed");
    step.status = match status {
        "success" => InvestigationStepStatus::Succeeded,
        "cancelled" => InvestigationStepStatus::Skipped,
        _ => InvestigationStepStatus::Failed,
    };
    step.output_summary = params
        .get("outputSummary")
        .and_then(Value::as_str)
        .map(|value| safe_audit_summary(value, 4_096));
    if let Some(evidence_ids) = plan_evidence_ids_from_event(params) {
        step.evidence_ids = evidence_ids;
    }
    if let Some(plan_id) = params
        .get("planId")
        .and_then(Value::as_str)
        .map(|value| bounded_audit_text(value, 256))
    {
        step.plan_id = Some(plan_id);
    }
    if let Some(plan_step_id) = params
        .get("planStepId")
        .and_then(Value::as_str)
        .map(|value| bounded_audit_text(value, 256))
    {
        step.plan_step_id = Some(plan_step_id);
    }
    step.ended_at = params
        .get("endedAt")
        .and_then(Value::as_str)
        .map(|value| bounded_audit_text(value, 80));
    if matches!(
        step.status,
        InvestigationStepStatus::Failed | InvestigationStepStatus::Skipped
    ) {
        step.failure = failure_from_event(params, step.attempt);
    }
    Some(step)
}

fn plan_evidence_ids_from_event(params: &Value) -> Option<Vec<String>> {
    let raw = params.get("evidenceIds")?.as_array()?;
    Some(
        raw.iter()
            .filter_map(Value::as_str)
            .map(|value| bounded_audit_text(value, 256))
            .filter(|value| !value.is_empty())
            .take(256)
            .collect(),
    )
}

fn failure_from_event(params: &Value, attempt: u32) -> Option<InvestigationFailure> {
    let message = params
        .get("error")
        .and_then(Value::as_str)
        .or_else(|| {
            params
                .get("result")
                .and_then(|result| result.get("error"))
                .and_then(Value::as_str)
        })
        .or_else(|| params.get("outputSummary").and_then(Value::as_str))?
        .trim();
    let message = bounded_audit_text(message, 4_096);
    let code = params
        .get("errorCode")
        .and_then(Value::as_str)
        .and_then(task_failure_code_from_tool_error)
        .unwrap_or_else(|| classify_task_failure(&message));
    Some(InvestigationFailure {
        code,
        message,
        retryable: code.retryable(),
        attempt,
        at: params
            .get("at")
            .and_then(Value::as_str)
            .or_else(|| params.get("endedAt").and_then(Value::as_str))
            .map(|value| bounded_audit_text(value, 80))
            .unwrap_or_else(yukinal_core::sidecar::iso8601_now),
        detail: None,
        options: Some(failure_options(code, code.retryable())),
    })
}

pub(crate) fn failure_options(
    code: TaskFailureCode,
    retryable: bool,
) -> Vec<InvestigationFailureOption> {
    let mut options = Vec::new();
    if retryable {
        options.push(InvestigationFailureOption {
            id: "retry".into(),
            action: FailureOptionAction::Retry,
            title: "重试当前阶段".into(),
            description: "重新校验目标后再尝试一次；不会自动重放不可安全重试的写入。".into(),
            requires_approval: false,
        });
    }
    match code {
        TaskFailureCode::PlanDeviation | TaskFailureCode::EvidenceMissing => {
            options.push(InvestigationFailureOption {
                id: "replan".into(),
                action: FailureOptionAction::Replan,
                title: "要求 Agent 重新规划".into(),
                description: "保留现有证据与失败现场，要求生成新的计划修订。".into(),
                requires_approval: false,
            });
        }
        TaskFailureCode::ApprovalRequired
        | TaskFailureCode::ApprovalRejected
        | TaskFailureCode::PermissionDenied => {
            options.push(InvestigationFailureOption {
                id: "wait_user".into(),
                action: FailureOptionAction::WaitUser,
                title: "等待我重新决定".into(),
                description: "不自动重试；由用户补充批准、缩小范围或终止任务。".into(),
                requires_approval: true,
            });
        }
        TaskFailureCode::Cancelled => {
            options.push(InvestigationFailureOption {
                id: "resume".into(),
                action: FailureOptionAction::Resume,
                title: "从检查点恢复".into(),
                description: "重新检查连接、计划和目标后从未完成阶段继续。".into(),
                requires_approval: false,
            });
        }
        TaskFailureCode::BudgetExhausted => {
            options.push(InvestigationFailureOption {
                id: "replan".into(),
                action: FailureOptionAction::Replan,
                title: "缩小范围后重新规划".into(),
                description: "预算已耗尽，必须由用户确认新的范围或预算。".into(),
                requires_approval: true,
            });
        }
        _ => {}
    }
    options.push(InvestigationFailureOption {
        id: "inspect".into(),
        action: FailureOptionAction::Inspect,
        title: "查看现场证据".into(),
        description: "保持任务停止，先查看已保存的证据、执行结果和失败工件。".into(),
        requires_approval: false,
    });
    options.push(InvestigationFailureOption {
        id: "stop".into(),
        action: FailureOptionAction::Stop,
        title: "结束任务".into(),
        description: "保留审计与现场证据，不再继续自动化。".into(),
        requires_approval: false,
    });
    options
}

pub(crate) fn task_failure_code_from_tool_error(value: &str) -> Option<TaskFailureCode> {
    Some(match value {
        "invalid_input" => TaskFailureCode::InvalidInput,
        "plan_deviation" => TaskFailureCode::PlanDeviation,
        "denied_by_policy" | "permission_denied" => TaskFailureCode::PermissionDenied,
        "approval_rejected" => TaskFailureCode::ApprovalRejected,
        "approval_timeout" => TaskFailureCode::ApprovalRequired,
        "timeout" => TaskFailureCode::Timeout,
        "cancelled" => TaskFailureCode::Cancelled,
        "not_found" => TaskFailureCode::TargetNotFound,
        "transport" => TaskFailureCode::Transport,
        "unsupported" => TaskFailureCode::Unsupported,
        "execution_failed" => TaskFailureCode::CommandFailed,
        "internal" => TaskFailureCode::Internal,
        _ => return None,
    })
}

fn classify_task_failure(message: &str) -> TaskFailureCode {
    let value = message.to_ascii_lowercase();
    if value.contains("maxsteps") || value.contains("budget") {
        TaskFailureCode::BudgetExhausted
    } else if value.contains("timeout") || value.contains("timed out") {
        TaskFailureCode::Timeout
    } else if value.contains("cancel") || value.contains("stopped") {
        TaskFailureCode::Cancelled
    } else if value.contains("approval") {
        TaskFailureCode::ApprovalRejected
    } else if value.contains("auth") || value.contains("credential") {
        TaskFailureCode::Authentication
    } else if value.contains("transport") || value.contains("connect") || value.contains("network")
    {
        TaskFailureCode::Transport
    } else if value.contains("not found") || value.contains("不存在") {
        TaskFailureCode::TargetNotFound
    } else if value.contains("permission") || value.contains("policy") || value.contains("denied") {
        TaskFailureCode::PermissionDenied
    } else if value.contains("invalid") {
        TaskFailureCode::InvalidInput
    } else if value.contains("unsupported") {
        TaskFailureCode::Unsupported
    } else if value.contains("truncat") {
        TaskFailureCode::OutputTruncated
    } else if value.contains("evidence") {
        TaskFailureCode::EvidenceMissing
    } else if value.contains("revision") || value.contains("stale") {
        TaskFailureCode::StaleTarget
    } else if value.contains("command") || value.contains("exit code") {
        TaskFailureCode::CommandFailed
    } else {
        TaskFailureCode::Internal
    }
}

fn task_phase_for_status(status: TaskStatus) -> TaskPhase {
    match status {
        TaskStatus::Pending | TaskStatus::Investigating => TaskPhase::Investigating,
        TaskStatus::WaitingUser => TaskPhase::Decision,
        TaskStatus::Executing => TaskPhase::Execution,
        TaskStatus::Verifying => TaskPhase::Verification,
        TaskStatus::Completed => TaskPhase::Completed,
        TaskStatus::Failed | TaskStatus::Stopped | TaskStatus::Expired => TaskPhase::Recovery,
    }
}
