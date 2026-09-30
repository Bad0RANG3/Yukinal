//! Investigation plan, artifact and guardrail handlers.
//!
//! 从 `host.rs` 拆出来的理由：这一段是 durable ChangePlan 的宿主侧规则 —— 计划记录、
//! 计划检查、步骤结果、工件记录，以及观察窗口、playbook 步骤、任务 guardrail 的校验。
//! 这些规则共同决定「哪个副作用工具此刻被允许执行」，因此必须放在一起读。
//!
//! `record_failure` 等被证据/上下文共用的记录辅助仍留在 `host.rs` 根模块。

use super::*;

pub(super) fn handle_plan_record(state: &AppState, params: Value) -> Result<Value, String> {
    let mut plan = match serde_json::from_value::<InvestigationPlan>(
        params.get("plan").cloned().unwrap_or(Value::Null),
    ) {
        Ok(plan) => plan,
        Err(error) => {
            return Ok(record_failure(
                "invalid_input",
                format!("invalid investigation plan: {error}"),
                false,
            ))
        }
    };
    let task = match state.database.investigations().get_task(&plan.task_id) {
        Ok(task) => task,
        Err(DatabaseError::NotFound) => {
            return Ok(record_failure(
                "not_found",
                "investigation task was not found",
                false,
            ))
        }
        Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
    };
    if plan.steps.is_empty() {
        return Ok(record_failure(
            "invalid_input",
            "investigation plan needs at least one step",
            false,
        ));
    }
    if plan.steps.len() > 64 {
        return Ok(record_failure(
            "invalid_input",
            "investigation plan has too many steps",
            false,
        ));
    }
    let previous = state
        .database
        .investigations()
        .latest_plan(&task.id)
        .map_err(|error| error.to_string())?;
    // A plan id is an idempotency key, not a mutable approval handle. If an old
    // (possibly superseded) id is replayed, reject it instead of reopening that
    // row behind the current active plan and risking a unique-index collision.
    let existing_by_id = match state.database.investigations().get_plan(&plan.id) {
        Ok(existing) => Some(existing),
        Err(DatabaseError::NotFound) => None,
        Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
    };
    let same_plan = previous
        .as_ref()
        .is_some_and(|candidate| candidate.id == plan.id);
    if existing_by_id.is_some() && !same_plan {
        return Ok(record_failure(
            "plan_deviation",
            "plan id already belongs to an older revision; create a new plan id",
            false,
        ));
    }
    if let (Some(previous_plan), Some(existing)) = (previous.as_ref(), existing_by_id.as_ref()) {
        if previous_plan.id != existing.id
            || plan_definition(previous_plan)? != plan_definition(&plan)?
        {
            return Ok(record_failure(
                "plan_deviation",
                "an existing plan id cannot be reused with a different definition; create a new plan id",
                false,
            ));
        }
        merge_plan_runtime(&mut plan, previous_plan);
    } else {
        plan.revision = previous
            .as_ref()
            .map(|candidate| candidate.revision.saturating_add(1))
            .unwrap_or(1);
        plan.status = yukinal_database::models::PlanStatus::Active;
        plan.approval = Some(InvestigationPlanApproval {
            status: PlanApprovalStatus::Pending,
            source: None,
            option_id: None,
            approved_at: None,
            note: None,
        });
    }
    if let Some(window) = plan.observation_window.as_mut() {
        if let Some(error) = validate_observation_window(window, &plan.steps) {
            return Ok(record_failure("invalid_input", error, false));
        }
        let preserve_state = previous
            .as_ref()
            .and_then(|candidate| candidate.observation_window.as_ref())
            .filter(|candidate| observation_configuration_matches(candidate, window))
            .cloned();
        if let Some(previous_window) = preserve_state {
            *window = previous_window;
        } else {
            reset_observation_window(window);
        }
    }
    if plan.current_step_id.is_none() && !same_plan {
        plan.current_step_id = plan.steps.first().map(|step| step.id.clone());
    }
    let mut ids = std::collections::HashSet::new();
    for (index, step) in plan.steps.iter().enumerate() {
        if !ids.insert(step.id.as_str()) {
            return Ok(record_failure(
                "invalid_input",
                "investigation plan step ids must be unique",
                false,
            ));
        }
        if step.ordinal != index as u32 {
            return Ok(record_failure(
                "invalid_input",
                "investigation plan step ordinals must be consecutive",
                false,
            ));
        }
        if step.allowed_tools.is_empty() {
            return Ok(record_failure(
                "invalid_input",
                format!("plan step `{}` must allow at least one tool", step.id),
                false,
            ));
        }
        if let Some(error) = validate_playbook_step(step) {
            return Ok(record_failure("invalid_input", error, false));
        }
        if step.kind == PlanStepKind::Action
            && !step.requires_approval
            && !task_allows_auto_medium_action(&task, step)
        {
            return Ok(record_failure(
                "denied_by_policy",
                format!(
                    "action step `{}` may omit approval only for an auto executable goal on a remote development or staging target, and only for a medium-risk action using one of {{filesystem.backup, filesystem.edit, filesystem.write}} (or the backup+write pair)",
                    step.id
                ),
                false,
            ));
        }
        if let Some(error) = validate_input_bindings(step) {
            return Ok(record_failure("invalid_input", error, false));
        }
        if let Some(error) = validate_task_guardrails_for_plan_step(&task, step) {
            return Ok(record_failure("denied_by_policy", error, false));
        }
        if let Some(target) = &step.target {
            if !same_investigation_target(&task.scope, target) {
                return Ok(record_failure(
                    "denied_by_policy",
                    format!("plan step `{}` expands the task target scope", step.id),
                    false,
                ));
            }
        }
        for evidence_id in &step.evidence_ids {
            match state.database.investigations().get_evidence(evidence_id) {
                Ok(evidence) if evidence.task_id == task.id => {}
                Ok(_) => {
                    return Ok(record_failure(
                        "denied_by_policy",
                        format!(
                            "plan step `{}` references evidence from another task",
                            step.id
                        ),
                        false,
                    ))
                }
                Err(DatabaseError::NotFound) => {
                    return Ok(record_failure(
                        "evidence_missing",
                        format!("evidence `{evidence_id}` was not found",),
                        true,
                    ))
                }
                Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
            }
        }
    }
    if let Some(current_step_id) = plan.current_step_id.as_deref() {
        if !ids.contains(current_step_id) {
            return Ok(record_failure(
                "invalid_input",
                "currentStepId is not a plan step",
                false,
            ));
        }
    }
    if !same_plan {
        normalize_new_plan_runtime(&mut plan, &yukinal_core::sidecar::iso8601_now());
    }
    match state.database.investigations().save_plan(&plan) {
        Ok(()) => {
            let evidence_ids = plan
                .steps
                .iter()
                .flat_map(|step| step.evidence_ids.iter().cloned())
                .collect::<Vec<_>>();
            let change_plan = InvestigationArtifact {
                id: format!("artifact_change_plan_{}", plan.id),
                task_id: task.id.clone(),
                run_id: task.active_run_id.clone(),
                plan_id: Some(plan.id.clone()),
                plan_step_id: plan.current_step_id.clone(),
                phase: TaskPhase::Decision,
                kind: TaskArtifactKind::ChangePlan,
                status: TaskArtifactStatus::Ready,
                title: "待审批的变更计划".into(),
                summary: format!(
                    "计划修订 {} 已保存；行动步骤只会在范围、基线和批准条件满足后执行。",
                    plan.revision
                ),
                content: serde_json::to_value(&plan).map_err(|error| error.to_string())?,
                evidence_ids,
                created_at: plan.created_at.clone(),
                updated_at: plan.updated_at.clone(),
            };
            state
                .database
                .investigations()
                .upsert_artifact(&change_plan)
                .map_err(|error| error.to_string())?;
            Ok(json!({ "recorded": true, "plan": plan }))
        }
        Err(DatabaseError::Validation(message)) => {
            Ok(record_failure("invalid_input", message, false))
        }
        Err(error) => Ok(record_failure("internal", error.to_string(), false)),
    }
}

/// Return only the parts of a plan that require a fresh approval. Runtime
/// progress (attempts, step status, timestamps, deviations and observation
/// samples) is host-owned and deliberately excluded so an idempotent replay
/// cannot reset or widen an already-approved plan.
pub(super) fn plan_definition(plan: &InvestigationPlan) -> Result<Value, String> {
    let mut value = serde_json::to_value(plan).map_err(|error| error.to_string())?;
    let Some(object) = value.as_object_mut() else {
        return Err("investigation plan must serialize as an object".into());
    };
    for key in [
        "revision",
        "status",
        "createdAt",
        "updatedAt",
        "currentStepId",
        "approval",
    ] {
        object.remove(key);
    }
    if let Some(steps) = object.get_mut("steps").and_then(Value::as_array_mut) {
        for step in steps {
            if let Some(step_object) = step.as_object_mut() {
                for key in [
                    "attempts",
                    "status",
                    "startedAt",
                    "endedAt",
                    "lastDeviation",
                ] {
                    step_object.remove(key);
                }
            }
        }
    }
    if let Some(window) = object
        .get_mut("observationWindow")
        .and_then(Value::as_object_mut)
    {
        for key in [
            "status",
            "sampleCount",
            "startedAt",
            "deadlineAt",
            "deadlineEpochSeconds",
            "lastSampleAt",
            "lastSampleEpochSeconds",
            "lastFailure",
        ] {
            window.remove(key);
        }
    }
    Ok(value)
}

/// Merge only host-owned progress back into an idempotent plan replay. The
/// caller has already proved that the declarative definition is byte-equivalent
/// (as structured JSON), so preserving the persisted state cannot hide a
/// changed target, command binding or approval-relevant field.
pub(super) fn merge_plan_runtime(plan: &mut InvestigationPlan, previous: &InvestigationPlan) {
    plan.revision = previous.revision;
    plan.status = previous.status;
    plan.created_at = previous.created_at.clone();
    plan.current_step_id = previous.current_step_id.clone();
    plan.approval = previous.approval.clone();
    for step in &mut plan.steps {
        if let Some(previous_step) = previous
            .steps
            .iter()
            .find(|candidate| candidate.id == step.id)
        {
            step.attempts = previous_step.attempts;
            step.status = previous_step.status;
            step.started_at = previous_step.started_at.clone();
            step.ended_at = previous_step.ended_at.clone();
            step.last_deviation = previous_step.last_deviation.clone();
        }
    }
}

/// Runtime fields on a newly proposed plan are untrusted input. The host starts
/// the first declared step and keeps every later step pending; the model cannot
/// jump directly to an action by claiming that evidence already succeeded.
pub(super) fn normalize_new_plan_runtime(plan: &mut InvestigationPlan, now: &str) {
    plan.status = yukinal_database::models::PlanStatus::Active;
    plan.current_step_id = plan.steps.first().map(|step| step.id.clone());
    plan.created_at = now.to_string();
    plan.updated_at = now.to_string();
    for (index, step) in plan.steps.iter_mut().enumerate() {
        step.attempts = 0;
        step.status = if index == 0 {
            PlanStepStatus::Running
        } else {
            PlanStepStatus::Pending
        };
        step.started_at = (index == 0).then(|| now.to_string());
        step.ended_at = None;
        step.last_deviation = None;
    }
}

pub(super) fn handle_plan_check(state: &AppState, params: Value) -> Result<Value, String> {
    let request = serde_json::from_value::<HostPlanCheckRequest>(params)
        .map_err(|error| format!("invalid plan check request: {error}"))?;
    match check_plan_for_tool(
        state,
        &request.task_id,
        &request.tool_name,
        &request.target,
        PlanCheckBinding {
            input: &request.input,
            plan_id: None,
            step_id: None,
            evidence_ids: None,
        },
    )? {
        Ok(()) => {
            let plan = state
                .database
                .investigations()
                .latest_plan(&request.task_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "active plan disappeared during check".to_string())?;
            let step_id = plan
                .current_step_id
                .ok_or_else(|| "active plan has no current step".to_string())?;
            let current_step = plan
                .steps
                .iter()
                .find(|step| step.id == step_id)
                .ok_or_else(|| "active plan step disappeared during check".to_string())?;
            Ok(json!({
                "status": "allowed",
                "planId": plan.id,
                "stepId": current_step.id,
                "stepKind": current_step.kind,
                "evidenceIds": current_step.evidence_ids,
                "requiresApproval": current_step.requires_approval,
            }))
        }
        Err(deviation) => Ok(json!({ "status": "deviation", "deviation": deviation })),
    }
}

pub(super) fn handle_plan_step_result(state: &AppState, params: Value) -> Result<Value, String> {
    let request = serde_json::from_value::<HostPlanStepResultRequest>(params)
        .map_err(|error| format!("invalid plan step result request: {error}"))?;
    let mut plan = match state.database.investigations().get_plan(&request.plan_id) {
        Ok(plan) => plan,
        Err(DatabaseError::NotFound) => {
            return Ok(record_failure(
                "not_found",
                "investigation plan was not found",
                false,
            ))
        }
        Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
    };
    if plan.task_id != request.task_id {
        return Ok(record_failure(
            "denied_by_policy",
            "plan does not belong to the current investigation task",
            false,
        ));
    }
    let task = match state.database.investigations().get_task(&request.task_id) {
        Ok(task) => task,
        Err(DatabaseError::NotFound) => {
            return Ok(record_failure(
                "not_found",
                "investigation task was not found",
                false,
            ))
        }
        Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
    };
    if plan.current_step_id.as_deref() != Some(request.step_id.as_str()) {
        return Ok(record_failure(
            "plan_deviation",
            "plan step result does not match the current active step",
            false,
        ));
    }
    if !matches!(request.status.as_str(), "success" | "failed" | "cancelled") {
        return Ok(record_failure(
            "invalid_input",
            "unknown plan step result status",
            false,
        ));
    }
    let now = yukinal_core::sidecar::iso8601_now();
    let Some(step_index) = plan
        .steps
        .iter()
        .position(|step| step.id == request.step_id)
    else {
        return Ok(record_failure(
            "not_found",
            "plan step was not found",
            false,
        ));
    };
    let step_kind = plan.steps[step_index].kind;
    let requires_baseline = plan.steps[step_index].requires_baseline == Some(true);
    if request.status == "success" && requires_baseline {
        let artifacts = state
            .database
            .investigations()
            .list_artifacts(&task.id, 128)
            .map_err(|error| error.to_string())?;
        if !baseline_artifact_matches_plan(&artifacts, &plan) {
            return Ok(record_failure(
                "evidence_missing",
                "action step completed without a ready baseline bound to this plan revision",
                true,
            ));
        }
    }
    if request.status == "success" && step_kind == PlanStepKind::Verification {
        let has_verification = state
            .database
            .investigations()
            .list_artifacts(&task.id, 128)
            .map_err(|error| error.to_string())?
            .iter()
            .any(|artifact| {
                artifact.kind == TaskArtifactKind::Verification
                    && artifact.status == TaskArtifactStatus::Succeeded
                    && artifact.plan_id.as_deref() == Some(plan.id.as_str())
                    && artifact.plan_step_id.as_deref() == Some(request.step_id.as_str())
            });
        if !has_verification {
            return Ok(record_failure(
                "evidence_missing",
                "verification step must record a succeeded verification artifact before it can advance",
                true,
            ));
        }
    }
    if step_kind == PlanStepKind::Verification {
        if request.status != "success"
            && plan
                .observation_window
                .as_ref()
                .is_some_and(|window| window.status == ObservationWindowStatus::Running)
        {
            let failure_message = crate::commands::safe_audit_summary(
                request
                    .output_summary
                    .as_deref()
                    .unwrap_or("观察窗口采样异常"),
                8_192,
            );
            if let Some(window) = plan.observation_window.as_mut() {
                window.status = ObservationWindowStatus::Failed;
                window.last_failure = Some(failure_message.clone());
            }
            plan.updated_at = now.clone();
            state
                .database
                .investigations()
                .save_plan(&plan)
                .map_err(|error| error.to_string())?;
            let failure = yukinal_database::models::InvestigationFailure {
                code: TaskFailureCode::CommandFailed,
                message: failure_message.clone(),
                retryable: false,
                attempt: plan.steps[step_index].attempts.saturating_add(1),
                at: now.clone(),
                detail: Some(json!({
                    "planId": plan.id,
                    "stepId": request.step_id,
                    "observation": "failed",
                    "status": request.status,
                })),
                options: Some(crate::commands::failure_options(
                    TaskFailureCode::CommandFailed,
                )),
            };
            let failure_artifact = InvestigationArtifact {
                id: format!(
                    "artifact_observation_failure_{}_{}",
                    plan.id, request.step_id
                ),
                task_id: task.id.clone(),
                run_id: task.active_run_id.clone(),
                plan_id: Some(plan.id.clone()),
                plan_step_id: Some(request.step_id.clone()),
                phase: TaskPhase::Recovery,
                kind: TaskArtifactKind::Failure,
                status: TaskArtifactStatus::Failed,
                title: "观察窗口发现异常".into(),
                summary: failure_message,
                content: serde_json::to_value(&failure).map_err(|error| error.to_string())?,
                evidence_ids: plan.steps[step_index].evidence_ids.clone(),
                created_at: now.clone(),
                updated_at: now.clone(),
            };
            state
                .database
                .investigations()
                .upsert_artifact(&failure_artifact)
                .map_err(|error| error.to_string())?;
            if crate::commands::investigation::can_transition(task.status, TaskStatus::WaitingUser)
            {
                state
                    .database
                    .investigations()
                    .update_task_progress(&yukinal_database::repositories::TaskProgressUpdate {
                        id: &task.id,
                        status: TaskStatus::WaitingUser,
                        phase: TaskPhase::Recovery,
                        active_run_id: task.active_run_id.as_deref(),
                        last_failure: Some(&failure),
                        updated_at: &now,
                        completed_at: None,
                    })
                    .map_err(|error| error.to_string())?;
            }
            return Ok(json!({
                "recorded": true,
                "observation": "failed",
                "plan": plan,
            }));
        }
        if request.status == "success" {
            let epoch = yukinal_time::now_epoch_seconds();
            let mut observation_sampled = false;
            let mut next_sample_at = None;
            if let Some(window) = plan.observation_window.as_mut() {
                match window.status {
                    ObservationWindowStatus::Pending => {
                        let deadline = epoch.saturating_add(window.duration_seconds);
                        window.status = ObservationWindowStatus::Running;
                        window.sample_count = 1;
                        window.started_at = Some(now.clone());
                        window.deadline_epoch_seconds = Some(deadline);
                        window.deadline_at = Some(yukinal_time::iso8601_utc(deadline));
                        window.last_sample_epoch_seconds = Some(epoch);
                        window.last_sample_at = Some(now.clone());
                        observation_sampled = true;
                        next_sample_at = Some(yukinal_time::iso8601_utc(
                            epoch.saturating_add(window.interval_seconds),
                        ));
                    }
                    ObservationWindowStatus::Running => {
                        let deadline = window.deadline_epoch_seconds.unwrap_or(epoch);
                        if epoch >= deadline {
                            window.status = ObservationWindowStatus::Succeeded;
                            window.sample_count = window.sample_count.saturating_add(1);
                            window.last_sample_epoch_seconds = Some(epoch);
                            window.last_sample_at = Some(now.clone());
                        } else {
                            let last = window.last_sample_epoch_seconds.unwrap_or(0);
                            if epoch.saturating_sub(last) < window.interval_seconds {
                                next_sample_at = Some(yukinal_time::iso8601_utc(
                                    last.saturating_add(window.interval_seconds),
                                ));
                            } else {
                                window.sample_count = window.sample_count.saturating_add(1);
                                window.last_sample_epoch_seconds = Some(epoch);
                                window.last_sample_at = Some(now.clone());
                                observation_sampled = true;
                                next_sample_at = Some(yukinal_time::iso8601_utc(
                                    epoch.saturating_add(window.interval_seconds),
                                ));
                            }
                        }
                    }
                    ObservationWindowStatus::Succeeded => {}
                    ObservationWindowStatus::Failed | ObservationWindowStatus::Cancelled => {
                        return Ok(record_failure(
                            "invalid_input",
                            "observation window is no longer runnable; re-plan before sampling again",
                            false,
                        ));
                    }
                }
                if plan
                    .observation_window
                    .as_ref()
                    .is_some_and(|current| current.status == ObservationWindowStatus::Running)
                    && (observation_sampled || next_sample_at.is_some())
                {
                    plan.updated_at = now.clone();
                    state
                        .database
                        .investigations()
                        .save_plan(&plan)
                        .map_err(|error| error.to_string())?;
                    if let Some(next_sample_at) = next_sample_at {
                        return Ok(json!({
                            "recorded": true,
                            "observation": "running",
                            "sampleAccepted": observation_sampled,
                            "nextSampleAt": next_sample_at,
                            "plan": plan,
                        }));
                    }
                }
            }
        }
    }
    let step = &mut plan.steps[step_index];
    if !matches!(
        step.status,
        PlanStepStatus::Pending | PlanStepStatus::Running
    ) {
        return Ok(record_failure(
            "invalid_input",
            "plan step is no longer active",
            false,
        ));
    }
    step.attempts = step.attempts.saturating_add(1);
    step.started_at.get_or_insert_with(|| now.clone());
    step.ended_at = Some(now.clone());
    let output_summary = request.output_summary.clone();
    match request.status.as_str() {
        "success" => {
            step.status = PlanStepStatus::Succeeded;
            step.last_deviation = None;
            let next_index = step_index + 1;
            if let Some(next) = plan.steps.get_mut(next_index) {
                next.status = PlanStepStatus::Running;
                next.started_at.get_or_insert_with(|| now.clone());
                plan.current_step_id = Some(next.id.clone());
            } else {
                plan.current_step_id = None;
                plan.status = yukinal_database::models::PlanStatus::Completed;
            }
        }
        "failed" | "cancelled" => {
            if !request.retryable || step.attempts >= step.max_attempts {
                step.status = PlanStepStatus::Blocked;
            } else {
                step.status = PlanStepStatus::Pending;
            }
        }
        _ => unreachable!("status was validated above"),
    }
    let blocked = plan.steps[step_index].status == PlanStepStatus::Blocked;
    plan.updated_at = now.clone();
    match state.database.investigations().save_plan(&plan) {
        Ok(()) => {}
        Err(DatabaseError::Validation(message)) => {
            return Ok(record_failure("invalid_input", message, false))
        }
        Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
    }
    if blocked {
        let failure_code = if request.status == "cancelled" {
            TaskFailureCode::Cancelled
        } else {
            TaskFailureCode::CommandFailed
        };
        let failure_message = crate::commands::safe_audit_summary(
            output_summary
                .as_deref()
                .unwrap_or("计划步骤未完成，等待用户决定下一步"),
            8_192,
        );
        let mut failure_options = crate::commands::failure_options(failure_code);
        if step_kind == PlanStepKind::Action
            && plan.steps[step_index]
                .rollback
                .as_deref()
                .is_some_and(|rollback| !rollback.trim().is_empty())
        {
            failure_options.insert(
                0,
                InvestigationFailureOption {
                    id: "rollback".into(),
                    action: FailureOptionAction::Rollback,
                    title: "先规划回退".into(),
                    description:
                        "保留当前现场，生成一份单独的回退计划；回退动作仍需重新通过风险与用户审批。"
                            .into(),
                    requires_approval: true,
                },
            );
        }
        let failure = yukinal_database::models::InvestigationFailure {
            code: failure_code,
            message: failure_message.clone(),
            retryable: false,
            attempt: plan.steps[step_index].attempts,
            at: now.clone(),
            detail: Some(json!({
                "planId": plan.id,
                "stepId": request.step_id,
                "status": request.status,
                "retryable": request.retryable,
            })),
            options: Some(failure_options),
        };
        let failure_artifact = InvestigationArtifact {
            id: format!("artifact_failure_{}_{}", plan.id, request.step_id),
            task_id: task.id.clone(),
            run_id: task.active_run_id.clone(),
            plan_id: Some(plan.id.clone()),
            plan_step_id: Some(request.step_id.clone()),
            phase: TaskPhase::Recovery,
            kind: TaskArtifactKind::Failure,
            status: TaskArtifactStatus::Failed,
            title: "计划步骤失败".into(),
            summary: failure_message,
            content: serde_json::to_value(&failure).map_err(|error| error.to_string())?,
            evidence_ids: plan.steps[step_index].evidence_ids.clone(),
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        state
            .database
            .investigations()
            .upsert_artifact(&failure_artifact)
            .map_err(|error| error.to_string())?;
        if crate::commands::investigation::can_transition(task.status, TaskStatus::Failed) {
            state
                .database
                .investigations()
                .update_task_progress(&yukinal_database::repositories::TaskProgressUpdate {
                    id: &task.id,
                    status: TaskStatus::Failed,
                    phase: TaskPhase::Recovery,
                    active_run_id: task.active_run_id.as_deref(),
                    last_failure: Some(&failure),
                    updated_at: &now,
                    completed_at: None,
                })
                .map_err(|error| error.to_string())?;
        }
    }
    if blocked {
        return Ok(json!({ "recorded": true, "plan": plan }));
    }
    let desired_status = if plan.status == yukinal_database::models::PlanStatus::Completed
        && step_kind == PlanStepKind::Verification
    {
        TaskStatus::Completed
    } else {
        plan.current_step_id
            .as_deref()
            .and_then(|current_step_id| {
                plan.steps
                    .iter()
                    .find(|candidate| candidate.id == current_step_id)
                    .map(|candidate| match candidate.kind {
                        PlanStepKind::Action => TaskStatus::Executing,
                        PlanStepKind::Verification => TaskStatus::Verifying,
                        PlanStepKind::Decision => TaskStatus::WaitingUser,
                        PlanStepKind::Evidence => TaskStatus::Investigating,
                    })
            })
            .unwrap_or(TaskStatus::WaitingUser)
    };
    if task.status != desired_status
        && crate::commands::investigation::can_transition(task.status, desired_status)
    {
        let completed_at = (desired_status == TaskStatus::Completed).then_some(now.as_str());
        state
            .database
            .investigations()
            .update_task_status(&task.id, desired_status, &now, completed_at)
            .map_err(|error| error.to_string())?;
    }
    Ok(json!({ "recorded": true, "plan": plan }))
}

pub(super) fn handle_artifact_record(state: &AppState, params: Value) -> Result<Value, String> {
    let request = match serde_json::from_value::<HostArtifactRecordRequest>(params) {
        Ok(request) => request,
        Err(error) => {
            return Ok(record_failure(
                "invalid_input",
                format!("invalid investigation artifact: {error}"),
                false,
            ))
        }
    };
    let mut artifact = request.artifact;
    // The envelope's binding is copied from the separately validated request. The model is
    // allowed to propose content, but it cannot smuggle a different plan revision into the
    // row that the host persists.
    artifact.plan_id = request.plan_id.clone();
    artifact.plan_step_id = request.plan_step_id.clone();
    let task = match state.database.investigations().get_task(&artifact.task_id) {
        Ok(task) => task,
        Err(DatabaseError::NotFound) => {
            return Ok(record_failure(
                "not_found",
                "investigation task was not found",
                false,
            ))
        }
        Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
    };
    match check_plan_for_tool(
        state,
        &task.id,
        "investigation.artifact",
        &HostToolTarget {
            host: match task.scope.host {
                yukinal_database::models::InvestigationTargetHost::Local => "local".into(),
                yukinal_database::models::InvestigationTargetHost::Remote => "remote".into(),
            },
            server_id: task.scope.server_id.clone(),
            workspace_id: task.scope.workspace_id.clone(),
            environment: task.scope.environment,
        },
        PlanCheckBinding {
            input: &Value::Object(serde_json::Map::new()),
            plan_id: request.plan_id.as_deref(),
            step_id: request.plan_step_id.as_deref(),
            evidence_ids: request.evidence_ids.as_deref(),
        },
    )? {
        Ok(()) => {}
        Err(deviation) => return Ok(record_failure("plan_deviation", deviation.message, false)),
    }
    if artifact.title.trim().is_empty() || artifact.summary.trim().is_empty() {
        return Ok(record_failure(
            "invalid_input",
            "artifact title and summary are required",
            false,
        ));
    }
    if artifact.kind == TaskArtifactKind::Baseline
        && !matches!(
            artifact.status,
            TaskArtifactStatus::Ready | TaskArtifactStatus::Succeeded
        )
    {
        return Ok(record_failure(
            "invalid_input",
            "baseline artifacts must be ready or succeeded before an action can consume them",
            false,
        ));
    }
    if artifact.kind == TaskArtifactKind::Baseline {
        let Some(plan_id) = artifact.plan_id.as_deref() else {
            return Ok(record_failure(
                "invalid_input",
                "baseline artifacts must be bound to the current plan",
                false,
            ));
        };
        let Some(plan_step_id) = artifact.plan_step_id.as_deref() else {
            return Ok(record_failure(
                "invalid_input",
                "baseline artifacts must identify their evidence step",
                false,
            ));
        };
        if artifact.evidence_ids.is_empty() {
            return Ok(record_failure(
                "evidence_missing",
                "baseline artifacts must cite at least one persisted evidence envelope",
                true,
            ));
        }
        let plan = match state.database.investigations().get_plan(plan_id) {
            Ok(plan) if plan.task_id == task.id => plan,
            Ok(_) => {
                return Ok(record_failure(
                    "denied_by_policy",
                    "baseline plan does not belong to the current investigation task",
                    false,
                ))
            }
            Err(DatabaseError::NotFound) => {
                return Ok(record_failure(
                    "not_found",
                    "baseline plan was not found",
                    false,
                ))
            }
            Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
        };
        if !plan
            .steps
            .iter()
            .any(|step| step.id == plan_step_id && step.kind == PlanStepKind::Evidence)
        {
            return Ok(record_failure(
                "invalid_input",
                "baseline artifacts must be produced by an evidence step",
                false,
            ));
        }
    }
    if artifact.title.chars().count() > 512 || artifact.summary.chars().count() > 8_192 {
        return Ok(record_failure(
            "invalid_input",
            "artifact title or summary exceeds the configured limit",
            false,
        ));
    }
    let content_bytes = serde_json::to_vec(&artifact.content).map_err(|error| error.to_string())?;
    if content_bytes.len() > yukinal_database::models::MAX_ARTIFACT_SERIALIZED_BYTES {
        return Ok(record_failure(
            "invalid_input",
            "artifact content exceeds the 1 MiB limit",
            false,
        ));
    }
    if let Some(run_id) = artifact.run_id.as_deref() {
        match state.database.investigations().get_run(run_id) {
            Ok(run) if run.task_id == task.id => {}
            Ok(_) => {
                return Ok(record_failure(
                    "denied_by_policy",
                    "artifact run does not belong to the current investigation task",
                    false,
                ))
            }
            Err(DatabaseError::NotFound) => {
                return Ok(record_failure(
                    "not_found",
                    "artifact run was not found",
                    false,
                ))
            }
            Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
        }
    }
    for evidence_id in &artifact.evidence_ids {
        match state.database.investigations().get_evidence(evidence_id) {
            Ok(evidence) if evidence.task_id == task.id => {}
            Ok(_) => {
                return Ok(record_failure(
                    "denied_by_policy",
                    "artifact references evidence from another task",
                    false,
                ))
            }
            Err(DatabaseError::NotFound) => {
                return Ok(record_failure(
                    "evidence_missing",
                    format!("evidence `{evidence_id}` was not found"),
                    true,
                ))
            }
            Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
        }
    }
    match state.database.investigations().upsert_artifact(&artifact) {
        Ok(()) => Ok(json!({ "recorded": true, "artifact": artifact })),
        Err(DatabaseError::Validation(message)) => {
            Ok(record_failure("invalid_input", message, false))
        }
        Err(error) => Ok(record_failure("internal", error.to_string(), false)),
    }
}

pub(super) fn validate_playbook_step(
    step: &yukinal_database::models::InvestigationPlanStep,
) -> Option<String> {
    if step.requires_baseline == Some(true) && step.kind != PlanStepKind::Action {
        return Some(format!(
            "only action step `{}` may require a baseline",
            step.id
        ));
    }
    if step.kind != PlanStepKind::Action {
        return None;
    }
    if step
        .preview
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
    {
        return Some(format!(
            "action step `{}` must include a bounded preview",
            step.id
        ));
    }
    let has_verification = step
        .verification_criteria
        .as_ref()
        .is_some_and(|criteria| !criteria.is_empty())
        || !step.success_criteria.is_empty();
    if !has_verification {
        return Some(format!(
            "action step `{}` must declare verification criteria",
            step.id
        ));
    }
    if matches!(step.risk_level, Some(RiskLevel::High | RiskLevel::Critical))
        && !step.requires_approval
    {
        return Some(format!(
            "high-risk action step `{}` must require approval",
            step.id
        ));
    }
    if step.idempotency == Some(PlanIdempotency::Unsafe)
        && step
            .rollback
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
    {
        return Some(format!(
            "unsafe action step `{}` must declare rollback details",
            step.id
        ));
    }
    if let Some(error) = validate_backup_rotation_step(step) {
        return Some(error);
    }
    None
}

/// Validate the structural binding produced by the `backup_rotation` playbook.  This check
/// belongs at plan-record time so an invalid rotation cannot sit in an approved-looking plan
/// and fail only after the user has approved it.
fn validate_backup_rotation_step(
    step: &yukinal_database::models::InvestigationPlanStep,
) -> Option<String> {
    let bindings = step.input_bindings.as_ref()?;
    let serialized_items = bindings.get("items")?;
    if step.allowed_tools != [FILESYSTEM_BACKUP_CLEANUP.to_string()] {
        return Some(format!(
            "backup rotation step `{}` must allow only filesystem.backup.cleanup",
            step.id
        ));
    }
    if step.risk_level != Some(RiskLevel::Medium) || !step.requires_approval {
        return Some(format!(
            "backup rotation step `{}` must be medium risk and require approval",
            step.id
        ));
    }
    let items =
        match serde_json::from_str::<Vec<FilesystemBackupCleanupItemInput>>(serialized_items) {
            Ok(items) => items,
            Err(error) => {
                return Some(format!(
                    "backup rotation step `{}` has invalid items binding: {error}",
                    step.id
                ))
            }
        };
    if items.is_empty() || items.len() > 32 {
        return Some(format!(
            "backup rotation step `{}` must bind between 1 and 32 items",
            step.id
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for item in &items {
        if let Err(error) = validate_remote_path(&item.path) {
            return Some(format!(
                "backup rotation step `{}` has an invalid path: {error}",
                step.id
            ));
        }
        if let Err(error) = validate_remote_path(&item.backup_path) {
            return Some(format!(
                "backup rotation step `{}` has an invalid backupPath: {error}",
                step.id
            ));
        }
        if let Err(error) =
            AgentCleanupBackupRequest::check(&item.path, &item.backup_path, &item.expected_revision)
        {
            return Some(format!(
                "backup rotation step `{}` has an invalid cleanup item: {error}",
                step.id
            ));
        }
        if !seen.insert((item.path.as_str(), item.backup_path.as_str())) {
            return Some(format!(
                "backup rotation step `{}` contains duplicate path and backupPath",
                step.id
            ));
        }
    }
    None
}

/// Tools a `medium`-risk action step may use with `requires_approval: false` on an
/// auto/goal/execute task targeting a remote development or staging server (ADR 0073).
/// Deliberately excludes deletions (`filesystem.backup.cleanup`), restore, restarts
/// and package installs: those stay high-risk and are approved per call.
pub(super) const AUTO_MEDIUM_TOOLS: &[&str] =
    &[FILESYSTEM_BACKUP, FILESYSTEM_EDIT, FILESYSTEM_WRITE];

/// Exactly one allowed medium tool, or the common "back up before writing" pair.
/// Any wider or different set stays behind a per-step approval.
fn auto_medium_tools_allowed(tools: &[String]) -> bool {
    let allowed = |name: &str| AUTO_MEDIUM_TOOLS.contains(&name);
    if tools.len() == 1 {
        return allowed(tools[0].as_str());
    }
    if tools.len() == 2 {
        let has_backup = tools.iter().any(|tool| tool == FILESYSTEM_BACKUP);
        let has_write = tools
            .iter()
            .any(|tool| tool == FILESYSTEM_WRITE || tool == FILESYSTEM_EDIT);
        return has_backup && has_write && tools.iter().all(|tool| allowed(tool.as_str()));
    }
    false
}

pub(super) fn task_allows_auto_medium_action(
    task: &InvestigationTask,
    step: &yukinal_database::models::InvestigationPlanStep,
) -> bool {
    let target = step.target.as_ref().unwrap_or(&task.scope);
    task.permission_mode == InvestigationPermissionMode::Auto
        && task.mode == InvestigationRunMode::Goal
        && task.automation_level == TaskAutomationLevel::Execute
        && target.host == InvestigationTargetHost::Remote
        && matches!(
            target.environment,
            Environment::Development | Environment::Staging
        )
        && step.risk_level == Some(RiskLevel::Medium)
        && auto_medium_tools_allowed(&step.allowed_tools)
}

pub(super) fn validate_input_bindings(
    step: &yukinal_database::models::InvestigationPlanStep,
) -> Option<String> {
    let bindings = step.input_bindings.as_ref()?;
    if bindings.len() > 8 {
        return Some(format!(
            "plan step `{}` has too many input bindings",
            step.id
        ));
    }
    if bindings.iter().any(|(key, value)| {
        key.trim().is_empty()
            || key.chars().count() > 64
            || value.is_empty()
            || value.chars().count() > 16_384
    }) {
        return Some(format!(
            "plan step `{}` has an invalid input binding",
            step.id
        ));
    }
    None
}

pub(super) fn validate_observation_window(
    window: &InvestigationObservationWindow,
    steps: &[yukinal_database::models::InvestigationPlanStep],
) -> Option<String> {
    if window.duration_seconds == 0 || window.duration_seconds > 86_400 {
        return Some("observation duration must be between 1 and 86400 seconds".into());
    }
    if window.interval_seconds == 0 || window.interval_seconds > window.duration_seconds {
        return Some("observation interval must be between 1 and the duration".into());
    }
    if window.allowed_tools.is_empty() || window.allowed_tools.len() > 32 {
        return Some("observation window needs 1 to 32 allowed read tools".into());
    }
    if window.success_criteria.is_empty() || window.success_criteria.len() > 16 {
        return Some("observation window needs 1 to 16 success criteria".into());
    }
    if window
        .allowed_tools
        .iter()
        .any(|tool| tool.trim().is_empty() || tool.len() > 256)
    {
        return Some("observation tools must be bounded names".into());
    }
    if window
        .success_criteria
        .iter()
        .any(|criterion| criterion.trim().is_empty() || criterion.chars().count() > 1_024)
    {
        return Some("observation success criteria must be bounded text".into());
    }
    let Some(verification) = steps
        .last()
        .filter(|step| step.kind == PlanStepKind::Verification)
    else {
        return Some("observation window requires the final plan step to be verification".into());
    };
    if window.allowed_tools.iter().any(|tool| {
        !verification
            .allowed_tools
            .iter()
            .any(|allowed| allowed == tool)
    }) {
        return Some("observation tools must be allowed by the final verification step".into());
    }
    None
}

pub(super) fn observation_configuration_matches(
    left: &InvestigationObservationWindow,
    right: &InvestigationObservationWindow,
) -> bool {
    left.duration_seconds == right.duration_seconds
        && left.interval_seconds == right.interval_seconds
        && left.allowed_tools == right.allowed_tools
        && left.success_criteria == right.success_criteria
}

pub(super) fn reset_observation_window(window: &mut InvestigationObservationWindow) {
    window.status = ObservationWindowStatus::Pending;
    window.sample_count = 0;
    window.started_at = None;
    window.deadline_at = None;
    window.deadline_epoch_seconds = None;
    window.last_sample_at = None;
    window.last_sample_epoch_seconds = None;
    window.last_failure = None;
}

pub(super) struct PlanCheckBinding<'a> {
    pub(super) input: &'a Value,
    pub(super) plan_id: Option<&'a str>,
    pub(super) step_id: Option<&'a str>,
    pub(super) evidence_ids: Option<&'a [String]>,
}

pub(super) struct TaskGuardrailViolation {
    pub(super) code: PlanDeviationCode,
    pub(super) message: String,
}

pub(super) fn task_guardrail_violation(
    task: &InvestigationTask,
    tool_name: &str,
    input: &Value,
) -> Option<TaskGuardrailViolation> {
    let now = yukinal_time::now_epoch_seconds();
    if let Some(not_before) = task.guardrails.not_before_at.as_deref() {
        let Some(not_before) = yukinal_time::parse_iso8601_utc(not_before) else {
            return Some(TaskGuardrailViolation {
                code: PlanDeviationCode::OutsideTimeWindow,
                message: "task guardrail notBeforeAt is invalid; execution is blocked".into(),
            });
        };
        if now < not_before {
            return Some(TaskGuardrailViolation {
                code: PlanDeviationCode::OutsideTimeWindow,
                message: format!(
                    "task execution is not allowed before {}",
                    task.guardrails.not_before_at.as_deref().unwrap_or_default()
                ),
            });
        }
    }
    if let Some(expires) = task.guardrails.expires_at.as_deref() {
        let Some(expires) = yukinal_time::parse_iso8601_utc(expires) else {
            return Some(TaskGuardrailViolation {
                code: PlanDeviationCode::OutsideTimeWindow,
                message: "task guardrail expiresAt is invalid; execution is blocked".into(),
            });
        };
        if now >= expires {
            return Some(TaskGuardrailViolation {
                code: PlanDeviationCode::OutsideTimeWindow,
                message: format!(
                    "task execution time window expired at {}",
                    task.guardrails.expires_at.as_deref().unwrap_or_default()
                ),
            });
        }
    }
    if task
        .guardrails
        .forbidden_tools
        .iter()
        .any(|forbidden| forbidden == tool_name)
    {
        return Some(TaskGuardrailViolation {
            code: PlanDeviationCode::ScopeForbidden,
            message: format!("tool `{tool_name}` is forbidden by the task guardrails"),
        });
    }
    for path in input_path_values(input) {
        if let Some(prefix) = task
            .guardrails
            .forbidden_path_prefixes
            .iter()
            .find(|prefix| path_is_under_prefix(path, prefix))
        {
            return Some(TaskGuardrailViolation {
                code: PlanDeviationCode::ScopeForbidden,
                message: format!(
                    "path `{path}` is forbidden by the task guardrail prefix `{prefix}`"
                ),
            });
        }
    }
    None
}

pub(super) fn validate_task_guardrails_for_plan_step(
    task: &InvestigationTask,
    step: &yukinal_database::models::InvestigationPlanStep,
) -> Option<String> {
    if !task.guardrails.forbidden_tools.is_empty()
        && step.allowed_tools.iter().any(|tool| tool == "*")
    {
        return Some(format!(
            "plan step `{}` uses wildcard tools while task guardrails forbid specific tools; enumerate only allowed tools",
            step.id
        ));
    }
    if let Some(tool) = step.allowed_tools.iter().find(|tool| {
        task.guardrails
            .forbidden_tools
            .iter()
            .any(|forbidden| forbidden == *tool)
    }) {
        return Some(format!(
            "plan step `{}` allows tool `{tool}`, which is forbidden by the task guardrails",
            step.id
        ));
    }
    if let Some(bindings) = step.input_bindings.as_ref() {
        let input = Value::Object(
            bindings
                .iter()
                .map(|(key, value)| {
                    let parsed = serde_json::from_str::<Value>(value)
                        .ok()
                        .filter(|candidate| candidate.is_array() || candidate.is_object())
                        .unwrap_or_else(|| Value::String(value.clone()));
                    (key.clone(), parsed)
                })
                .collect(),
        );
        if let Some(violation) = task_guardrail_violation(task, "plan.binding", &input) {
            if violation.code == PlanDeviationCode::ScopeForbidden {
                return Some(format!(
                    "plan step `{}` violates task guardrails: {}",
                    step.id, violation.message
                ));
            }
        }
    }
    None
}

pub(super) fn input_path_values(input: &Value) -> Vec<&str> {
    let Some(object) = input.as_object() else {
        return Vec::new();
    };
    let mut paths = ["path", "backupPath"]
        .into_iter()
        .filter_map(|key| object.get(key).and_then(Value::as_str))
        .collect::<Vec<_>>();
    if let Some(items) = object.get("items").and_then(Value::as_array) {
        for item in items {
            paths.extend(input_path_values(item));
        }
    }
    paths
}

pub(super) fn path_is_under_prefix(path: &str, prefix: &str) -> bool {
    let path = normalize_guardrail_path(path);
    let prefix = normalize_guardrail_path(prefix);
    prefix == "/" || path == prefix || path.starts_with(&format!("{prefix}/"))
}

pub(super) fn normalize_guardrail_path(value: &str) -> String {
    let mut parts = Vec::new();
    for part in value.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            value => parts.push(value),
        }
    }
    if parts.is_empty() {
        "/".into()
    } else {
        format!("/{}", parts.join("/"))
    }
}

pub(super) fn check_plan_for_tool(
    state: &AppState,
    task_id: &str,
    tool_name: &str,
    target: &HostToolTarget,
    binding: PlanCheckBinding<'_>,
) -> Result<std::result::Result<(), InvestigationPlanDeviation>, String> {
    let task = state
        .database
        .investigations()
        .get_task(task_id)
        .map_err(|error| error.to_string())?;
    if let Some(violation) = task_guardrail_violation(&task, tool_name, binding.input) {
        return Ok(Err(plan_deviation(
            violation.code,
            PlanDeviationAction::WaitUser,
            violation.message,
            tool_name,
            yukinal_core::sidecar::iso8601_now(),
            None,
            None,
        )));
    }
    let at = yukinal_core::sidecar::iso8601_now();
    let plan = match state
        .database
        .investigations()
        .latest_plan(task_id)
        .map_err(|error| error.to_string())?
    {
        Some(plan) => plan,
        None => return Ok(Err(plan_deviation(
            PlanDeviationCode::MissingPlan,
            PlanDeviationAction::Replan,
            "durable task has no active plan; create investigation.plan before using a task tool",
            tool_name,
            at,
            None,
            None,
        ))),
    };
    let Some(current_step_id) = plan.current_step_id.as_deref() else {
        return Ok(Err(plan_deviation(
            PlanDeviationCode::NoActiveStep,
            PlanDeviationAction::Replan,
            "active plan has no current step",
            tool_name,
            at,
            Some(plan.id),
            None,
        )));
    };
    let Some(step) = plan.steps.iter().find(|step| step.id == current_step_id) else {
        return Ok(Err(plan_deviation(
            PlanDeviationCode::NoActiveStep,
            PlanDeviationAction::Replan,
            "active plan points to a missing current step",
            tool_name,
            at,
            Some(plan.id),
            Some(current_step_id.to_string()),
        )));
    };
    if !plan_binding_matches(
        &plan,
        step,
        binding.plan_id,
        binding.step_id,
        binding.evidence_ids,
    ) {
        return Ok(Err(plan_deviation(
            PlanDeviationCode::BindingMismatch,
            PlanDeviationAction::Deny,
            "tool request is not bound to the plan revision, current step and evidence set returned by the host",
            tool_name,
            at,
            Some(plan.id.clone()),
            Some(step.id.clone()),
        )));
    }
    if !matches!(
        step.status,
        PlanStepStatus::Pending | PlanStepStatus::Running
    ) {
        return Ok(Err(plan_deviation(
            PlanDeviationCode::NoActiveStep,
            PlanDeviationAction::Replan,
            "current plan step is no longer runnable",
            tool_name,
            at,
            Some(plan.id),
            Some(step.id.clone()),
        )));
    }
    if step.attempts >= step.max_attempts {
        return Ok(Err(plan_deviation(
            PlanDeviationCode::StepBudgetExhausted,
            PlanDeviationAction::WaitUser,
            "current plan step has exhausted its retry budget",
            tool_name,
            at,
            Some(plan.id),
            Some(step.id.clone()),
        )));
    }
    if let Some(window) = plan
        .observation_window
        .as_ref()
        .filter(|window| window.status == ObservationWindowStatus::Running)
    {
        if step.kind != PlanStepKind::Verification
            || !window
                .allowed_tools
                .iter()
                .any(|allowed| allowed == tool_name)
        {
            return Ok(Err(plan_deviation(
                PlanDeviationCode::ToolNotAllowed,
                PlanDeviationAction::WaitUser,
                "observation window only permits its declared verification read tools",
                tool_name,
                at,
                Some(plan.id.clone()),
                Some(step.id.clone()),
            )));
        }
    }
    if step.kind == PlanStepKind::Action {
        match task.automation_level {
            TaskAutomationLevel::Readonly => {
                return Ok(Err(plan_deviation(
                    PlanDeviationCode::ToolNotAllowed,
                    PlanDeviationAction::Deny,
                    "this task is read-only; action steps are available for planning but cannot execute",
                    tool_name,
                    at,
                    Some(plan.id),
                    Some(step.id.clone()),
                )))
            }
            TaskAutomationLevel::Propose
                if plan.approval.as_ref().map(|approval| approval.status)
                    != Some(yukinal_database::models::PlanApprovalStatus::Approved) =>
            {
                return Ok(Err(plan_deviation(
                    PlanDeviationCode::NoActiveStep,
                    PlanDeviationAction::WaitUser,
                    "proposal-mode action is waiting for the user to select and approve a decision option",
                    tool_name,
                    at,
                    Some(plan.id),
                    Some(step.id.clone()),
                )))
            }
            TaskAutomationLevel::Propose | TaskAutomationLevel::Execute => {}
        }
    }
    if step.kind == PlanStepKind::Action && step.requires_baseline == Some(true) {
        let artifacts = state
            .database
            .investigations()
            .list_artifacts(&task.id, 128)
            .map_err(|error| error.to_string())?;
        let has_baseline = baseline_artifact_matches_plan(&artifacts, &plan);
        if !has_baseline {
            return Ok(Err(plan_deviation(
                PlanDeviationCode::EvidenceMissing,
                PlanDeviationAction::Replan,
                "current action step requires a ready baseline artifact bound to this plan revision",
                tool_name,
                at,
                Some(plan.id),
                Some(step.id.clone()),
            )));
        }
    }
    if tool_name != "investigation.artifact"
        && !step
            .allowed_tools
            .iter()
            .any(|allowed| allowed == "*" || allowed == tool_name)
    {
        return Ok(Err(plan_deviation(
            PlanDeviationCode::ToolNotAllowed,
            PlanDeviationAction::Replan,
            format!("tool `{tool_name}` is not allowed by the current plan step"),
            tool_name,
            at,
            Some(plan.id),
            Some(step.id.clone()),
        )));
    }
    if !input_bindings_match(step, binding.input) {
        return Ok(Err(plan_deviation(
            PlanDeviationCode::BindingMismatch,
            PlanDeviationAction::Deny,
            "tool input does not match the exact arguments bound by the active plan step",
            tool_name,
            at,
            Some(plan.id),
            Some(step.id.clone()),
        )));
    }
    let target = host_target_to_investigation(target)?;
    let expected = step.target.as_ref().unwrap_or(&task.scope);
    if !same_investigation_target(expected, &target) {
        return Ok(Err(plan_deviation(
            PlanDeviationCode::TargetMismatch,
            PlanDeviationAction::WaitUser,
            "tool target does not match the active plan step scope",
            tool_name,
            at,
            Some(plan.id),
            Some(step.id.clone()),
        )));
    }
    for evidence_id in &step.evidence_ids {
        match state.database.investigations().get_evidence(evidence_id) {
            Ok(evidence) if evidence.task_id == task.id => {}
            Ok(_) | Err(DatabaseError::NotFound) => {
                return Ok(Err(plan_deviation(
                    PlanDeviationCode::EvidenceMissing,
                    PlanDeviationAction::Replan,
                    format!("plan step requires missing evidence `{evidence_id}`"),
                    tool_name,
                    at,
                    Some(plan.id),
                    Some(step.id.clone()),
                )))
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(Ok(()))
}

pub(super) fn baseline_artifact_matches_plan(
    artifacts: &[InvestigationArtifact],
    plan: &InvestigationPlan,
) -> bool {
    artifacts.iter().any(|artifact| {
        artifact.kind == TaskArtifactKind::Baseline
            && matches!(
                artifact.status,
                TaskArtifactStatus::Ready | TaskArtifactStatus::Succeeded
            )
            && artifact.plan_id.as_deref() == Some(plan.id.as_str())
            && !artifact.evidence_ids.is_empty()
            && artifact.plan_step_id.as_deref().is_some_and(|step_id| {
                plan.steps.iter().any(|step| {
                    step.id == step_id
                        && step.kind == PlanStepKind::Evidence
                        && step.status == PlanStepStatus::Succeeded
                })
            })
    })
}

pub(super) fn plan_binding_matches(
    plan: &InvestigationPlan,
    step: &yukinal_database::models::InvestigationPlanStep,
    expected_plan_id: Option<&str>,
    expected_step_id: Option<&str>,
    expected_evidence_ids: Option<&[String]>,
) -> bool {
    // The first `host.investigation.plan.check` call has no binding yet: it asks
    // the host to issue the current binding.  The actual `host.tool.execute`
    // request must carry all three values and is checked fail-closed.  Treating
    // the unbound probe as a mismatch would make every durable task unusable.
    if expected_plan_id.is_none() && expected_step_id.is_none() && expected_evidence_ids.is_none() {
        return true;
    }
    expected_plan_id == Some(plan.id.as_str())
        && expected_step_id == Some(step.id.as_str())
        && expected_evidence_ids == Some(step.evidence_ids.as_slice())
}

pub(super) fn input_bindings_match(
    step: &yukinal_database::models::InvestigationPlanStep,
    input: &Value,
) -> bool {
    let Some(bindings) = step.input_bindings.as_ref() else {
        return true;
    };
    let Some(object) = input.as_object() else {
        return false;
    };
    bindings.iter().all(|(key, expected)| {
        let Some(actual) = object.get(key) else {
            return false;
        };
        // A string binding is an exact string comparison, as it always was. A binding whose
        // input is an array or object is the canonical JSON of that value: `backup_rotation`
        // binds its whole item list this way (ADR 0076). Comparing `serde_json::Value`
        // equality keeps object key order out of the comparison while still pinning every
        // path, backup path and revision.
        if let Some(actual_str) = actual.as_str() {
            return actual_str == expected;
        }
        if actual.is_array() || actual.is_object() {
            if let Ok(expected_value) = serde_json::from_str::<Value>(expected) {
                return &expected_value == actual;
            }
        }
        false
    })
}

pub(super) fn plan_deviation(
    code: PlanDeviationCode,
    action: PlanDeviationAction,
    message: impl Into<String>,
    tool_name: &str,
    at: String,
    plan_id: Option<String>,
    step_id: Option<String>,
) -> InvestigationPlanDeviation {
    InvestigationPlanDeviation {
        code,
        action,
        message: message.into(),
        tool_name: tool_name.to_string(),
        at,
        plan_id,
        step_id,
    }
}
