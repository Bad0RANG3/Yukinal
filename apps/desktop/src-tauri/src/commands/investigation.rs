//! Durable investigation task commands.
//!
//! The task row is the desktop-side anchor for a run. The sidecar may be
//! restarted, but a user can still reopen the objective and inspect the bounded
//! evidence, findings and decision brief that belong to it.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, State};

use crate::state::AppState;
use yukinal_database::models::{
    DecisionBrief, DecisionOptionContinuation, InvestigationFailure,
    InvestigationNotificationPolicy, InvestigationPermissionMode, InvestigationPlanApproval,
    InvestigationRunMode, InvestigationSchedule, InvestigationScheduleRun,
    InvestigationScheduleStatus, InvestigationTarget, InvestigationTargetHost, InvestigationTask,
    InvestigationTaskGuardrails, PlanApprovalSource, PlanApprovalStatus, TaskArtifactKind,
    TaskArtifactStatus, TaskAutomationLevel, TaskBudget, TaskFailureCode, TaskPhase, TaskStatus,
};
use yukinal_database::repositories::{
    InvestigationRetentionKind, InvestigationRetentionRequestItem, TaskProgressUpdate,
};

const DEFAULT_MAX_STEPS: u32 = 25;
const DEFAULT_MAX_RUN_MS: u64 = 900_000;
const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 100;
const MAX_OBJECTIVE_CHARS: usize = 8_192;
const MAX_SUCCESS_CRITERIA: usize = 16;
const MAX_SUCCESS_CRITERION_CHARS: usize = 1_024;
const DEFAULT_MAX_ATTEMPTS: u32 = 3;
const MAX_ATTEMPTS: u32 = 32;
const DEFAULT_SCHEDULE_COOLDOWN_SECONDS: u64 = 300;
const DEFAULT_SCHEDULE_DEDUPE_SECONDS: u64 = 300;
const DEFAULT_SCHEDULE_MAX_CONCURRENT_RUNS: u32 = 1;
const DEFAULT_SCHEDULE_LIMIT: usize = 100;
const DEFAULT_RETENTION_DAYS: u64 = 30;
const DEFAULT_RETENTION_LIMIT: usize = 64;
const MAX_RETENTION_LIMIT: usize = 128;
const MAX_GUARDRAIL_TOOLS: usize = 64;
const MAX_GUARDRAIL_PATH_PREFIXES: usize = 64;
const MAX_GUARDRAIL_NAME_CHARS: usize = 256;
const MAX_GUARDRAIL_PATH_CHARS: usize = 4_096;
const MAX_GUARDRAIL_WINDOW_SECONDS: u64 = 365 * 86_400;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationTaskCreateInput {
    pub id: Option<String>,
    pub workspace_id: Option<String>,
    pub server_id: Option<String>,
    pub objective: String,
    pub success_criteria: Vec<String>,
    pub scope: InvestigationTarget,
    pub guardrails: Option<InvestigationTaskGuardrails>,
    pub mode: InvestigationRunMode,
    pub permission_mode: InvestigationPermissionMode,
    pub automation_level: TaskAutomationLevel,
    pub budget: Option<TaskBudget>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationTaskListResponse {
    pub tasks: Vec<InvestigationTask>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationTaskResponse {
    pub task: InvestigationTask,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationTaskStartResponse {
    pub run_id: String,
    pub started: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationTaskDetailResponse {
    pub task: InvestigationTask,
    /// Evidence bodies remain bounded/redacted, while this response adds a
    /// host-derived freshness object for the UI. The persisted row is unchanged.
    pub evidence: Vec<Value>,
    pub findings: Vec<yukinal_database::models::Finding>,
    pub runs: Vec<yukinal_database::models::InvestigationRun>,
    pub steps: Vec<yukinal_database::models::InvestigationStep>,
    pub artifacts: Vec<yukinal_database::models::InvestigationArtifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<yukinal_database::models::InvestigationPlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_brief: Option<DecisionBrief>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationBriefSelectInput {
    pub task_id: String,
    pub brief_id: String,
    pub option_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationBriefResponse {
    pub brief: DecisionBrief,
    pub continuation: DecisionOptionContinuation,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationTaskRecoverInput {
    pub task_id: String,
    pub option_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationScheduleCreateInput {
    pub id: Option<String>,
    pub task_id: String,
    pub interval_seconds: u64,
    pub cooldown_seconds: Option<u64>,
    pub dedupe_window_seconds: Option<u64>,
    pub max_concurrent_runs: Option<u32>,
    pub budget: Option<TaskBudget>,
    pub notification_policy: Option<InvestigationNotificationPolicy>,
    pub next_run_at: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationScheduleUpdateInput {
    pub schedule_id: String,
    pub status: Option<InvestigationScheduleStatus>,
    pub interval_seconds: Option<u64>,
    pub cooldown_seconds: Option<u64>,
    pub dedupe_window_seconds: Option<u64>,
    pub max_concurrent_runs: Option<u32>,
    pub budget: Option<TaskBudget>,
    pub notification_policy: Option<InvestigationNotificationPolicy>,
    pub next_run_at: Option<String>,
    /// `None` means the field was omitted; `Some(None)` explicitly clears a
    /// fixed baseline and returns the schedule to its latest-sample policy.
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub baseline_run_id: Option<Option<String>>,
}

fn deserialize_nullable_string<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::<String>::deserialize(deserializer)?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationScheduleTickInput {
    pub now: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationScheduleListResponse {
    pub schedules: Vec<InvestigationSchedule>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationScheduleRunListResponse {
    pub runs: Vec<InvestigationScheduleRun>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationScheduleResponse {
    pub schedule: InvestigationSchedule,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationScheduleTickResponse {
    pub claimed: Vec<InvestigationScheduleRun>,
    pub skipped: Vec<InvestigationScheduleRun>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationRetentionPruneInput {
    pub task_id: String,
    pub cutoff_at: String,
    pub items: Vec<InvestigationRetentionPruneItem>,
    pub confirmation: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationRetentionPruneItem {
    pub id: String,
    pub kind: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationRetentionItemResponse {
    pub id: String,
    pub task_id: String,
    pub kind: String,
    pub created_at: String,
    pub bytes: u64,
    pub reason: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationRetentionSkipResponse {
    pub id: String,
    pub kind: String,
    pub reason: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationRetentionPreviewPayload {
    pub task_id: String,
    pub cutoff_at: String,
    pub candidates: Vec<InvestigationRetentionItemResponse>,
    pub protected_count: u32,
    pub candidate_bytes: u64,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationRetentionPreviewResponse {
    pub preview: InvestigationRetentionPreviewPayload,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationRetentionPruneResultResponse {
    pub task_id: String,
    pub cutoff_at: String,
    pub deleted: Vec<InvestigationRetentionItemResponse>,
    pub skipped: Vec<InvestigationRetentionSkipResponse>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationRetentionPruneResponse {
    pub result: InvestigationRetentionPruneResultResponse,
}

#[tauri::command]
pub fn investigation_task_list(
    state: State<'_, AppState>,
    status: Option<TaskStatus>,
    limit: Option<usize>,
) -> Result<InvestigationTaskListResponse, String> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(format!(
            "investigation task limit must be between 1 and {MAX_LIMIT}"
        ));
    }
    let tasks = state
        .database
        .investigations()
        .list_tasks(status, limit)
        .map_err(|error| error.to_string())?;
    Ok(InvestigationTaskListResponse { tasks })
}

#[tauri::command]
pub fn investigation_task_get(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<InvestigationTaskDetailResponse, String> {
    let task_id = validate_id(&task_id, "task id")?;
    let task = state
        .database
        .investigations()
        .get_task(&task_id)
        .map_err(|error| error.to_string())?;
    let evidence = state
        .database
        .investigations()
        .list_evidence(&task_id, MAX_LIMIT)
        .map_err(|error| error.to_string())?;
    let evaluated_at_epoch = yukinal_time::now_epoch_seconds();
    let evidence = evidence
        .iter()
        .map(|item| {
            crate::commands::host::evidence_json_with_freshness_at(item, evaluated_at_epoch)
        })
        .collect();
    let findings = state
        .database
        .investigations()
        .list_findings(&task_id, MAX_LIMIT)
        .map_err(|error| error.to_string())?;
    let decision_brief = state
        .database
        .investigations()
        .latest_decision_brief(&task_id)
        .map_err(|error| error.to_string())?;
    let runs = state
        .database
        .investigations()
        .list_runs(&task_id, 64)
        .map_err(|error| error.to_string())?;
    let steps = state
        .database
        .investigations()
        .list_steps(&task_id, 512)
        .map_err(|error| error.to_string())?;
    let artifacts = state
        .database
        .investigations()
        .list_artifacts(&task_id, 128)
        .map_err(|error| error.to_string())?;
    let plan = state
        .database
        .investigations()
        .latest_plan(&task_id)
        .map_err(|error| error.to_string())?;
    Ok(InvestigationTaskDetailResponse {
        task,
        evidence,
        findings,
        runs,
        steps,
        artifacts,
        plan,
        decision_brief,
    })
}

#[tauri::command]
pub fn investigation_task_create(
    state: State<'_, AppState>,
    input: InvestigationTaskCreateInput,
) -> Result<InvestigationTaskResponse, String> {
    let task = validate_create_input(&state, input)?;
    state
        .database
        .investigations()
        .create_task(&task)
        .map_err(|error| error.to_string())?;
    Ok(InvestigationTaskResponse { task })
}

/// Start a durable task directly from its objective. This is the task-level entry point for
/// autonomous work: the caller does not need to open the chat panel or invent a second prompt.
/// The existing `agent_run_start` command still owns provider resolution, run admission,
/// permission mode and sidecar supervision; this command only supplies a bounded objective
/// prompt and the task's host-owned safety envelope.
#[tauri::command]
pub async fn investigation_task_start(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<InvestigationTaskStartResponse, String> {
    let task_id = validate_id(&task_id, "task id")?;
    let task = state
        .database
        .investigations()
        .get_task(&task_id)
        .map_err(|error| error.to_string())?;
    if is_terminal(task.status) {
        return Err("terminal investigation tasks cannot start a new Agent run".into());
    }
    task_time_window_error(&task)?;
    if let Some(active_run_id) = task.active_run_id.as_deref() {
        match state.database.investigations().get_run(active_run_id) {
            Ok(run)
                if matches!(
                    run.status,
                    yukinal_database::models::InvestigationRunStatus::Admitted
                        | yukinal_database::models::InvestigationRunStatus::Running
                        | yukinal_database::models::InvestigationRunStatus::WaitingUser
                ) =>
            {
                return Err("investigation task already has an active Agent run".into());
            }
            Ok(_) | Err(yukinal_database::DatabaseError::NotFound) => {}
            Err(error) => return Err(format!("failed to inspect the active Agent run: {error}")),
        }
    }

    let run_id = crate::commands::server::next_id("run");
    let session_id = format!("session_task_{}", task.id);
    let message_id = format!("message_{run_id}");
    let prompt = autonomous_task_prompt(&task);
    let response = crate::commands::agent_run::agent_run_start(
        state,
        Some(run_id),
        session_id,
        Some(task.id.clone()),
        prompt,
        Some(message_id),
        None,
        Some("async".into()),
        Some(true),
        None,
        None,
        task.workspace_id.clone(),
        task.server_id.clone(),
        Some(task.permission_mode.as_str().into()),
        Some(task.mode.as_str().into()),
        None,
    )
    .await?;
    Ok(InvestigationTaskStartResponse {
        run_id: response.run_id,
        started: response.started,
    })
}

/// Stop a durable task through the host-owned cancellation fence. This is deliberately
/// separate from the generic status update command: an active task must first ask the
/// sidecar to cancel its run, then persist the terminal task state while late events are
/// fenced. A repeated stop of an already-stopped task is idempotent.
#[tauri::command]
pub async fn investigation_task_stop(
    app: AppHandle,
    state: State<'_, AppState>,
    task_id: String,
) -> Result<InvestigationTaskResponse, String> {
    let task_id = validate_id(&task_id, "task id")?;
    let task = state
        .database
        .investigations()
        .get_task(&task_id)
        .map_err(|error| error.to_string())?;
    if task.status == TaskStatus::Stopped {
        return Ok(InvestigationTaskResponse { task });
    }
    if is_terminal(task.status) {
        return Err("completed, failed, or expired investigation tasks cannot be stopped".into());
    }

    stop_task_for_decision(&app, &state, &task_id).await?;
    let updated = state
        .database
        .investigations()
        .get_task(&task_id)
        .map_err(|error| error.to_string())?;
    Ok(InvestigationTaskResponse { task: updated })
}

/// Preview terminal-task history that is safe for an explicit, user-approved prune.
/// The host owns the cutoff default so the Agent/UI cannot silently choose an
/// aggressive retention window.
#[tauri::command]
pub fn investigation_retention_preview(
    state: State<'_, AppState>,
    task_id: String,
    cutoff_at: Option<String>,
    limit: Option<usize>,
) -> Result<InvestigationRetentionPreviewResponse, String> {
    let task_id = validate_id(&task_id, "task id")?;
    let cutoff_at = retention_cutoff(cutoff_at)?;
    let limit = limit.unwrap_or(DEFAULT_RETENTION_LIMIT);
    if !(1..=MAX_RETENTION_LIMIT).contains(&limit) {
        return Err(format!(
            "retention preview limit must be between 1 and {MAX_RETENTION_LIMIT}"
        ));
    }
    let preview = state
        .database
        .investigation_retention()
        .preview(&task_id, &cutoff_at, limit)
        .map_err(|error| error.to_string())?;
    Ok(InvestigationRetentionPreviewResponse {
        preview: InvestigationRetentionPreviewPayload {
            task_id: preview.task_id,
            cutoff_at: preview.cutoff_at,
            candidates: preview
                .candidates
                .into_iter()
                .map(retention_item_response)
                .collect(),
            protected_count: preview.protected_count,
            candidate_bytes: preview.candidate_bytes,
            truncated: preview.truncated,
        },
    })
}

/// Prune exactly the metadata items named by the UI's prior preview. The
/// confirmation literal is intentionally checked at the host boundary and the
/// repository re-checks every item inside one transaction before deleting it.
#[tauri::command]
pub fn investigation_retention_prune(
    state: State<'_, AppState>,
    input: InvestigationRetentionPruneInput,
) -> Result<InvestigationRetentionPruneResponse, String> {
    let task_id = validate_id(&input.task_id, "task id")?;
    let cutoff_at = retention_cutoff(Some(input.cutoff_at))?;
    if input.confirmation != "delete_unreferenced" {
        return Err("retention prune requires confirmation=delete_unreferenced".into());
    }
    if input.items.is_empty() || input.items.len() > MAX_RETENTION_LIMIT {
        return Err(format!(
            "retention prune requires between 1 and {MAX_RETENTION_LIMIT} items"
        ));
    }
    let mut request_items = Vec::with_capacity(input.items.len());
    for item in &input.items {
        let id = validate_id(&item.id, "retention item id")?;
        let kind = parse_retention_kind(&item.kind)?;
        request_items.push((id, kind));
    }
    let requests = request_items
        .iter()
        .map(|(id, kind)| InvestigationRetentionRequestItem { id, kind: *kind })
        .collect::<Vec<_>>();
    let result = state
        .database
        .investigation_retention()
        .prune(
            &task_id,
            &cutoff_at,
            &requests,
            &crate::commands::server::next_id("retention"),
            &yukinal_core::sidecar::iso8601_now(),
        )
        .map_err(|error| error.to_string())?;
    Ok(InvestigationRetentionPruneResponse {
        result: InvestigationRetentionPruneResultResponse {
            task_id: result.task_id,
            cutoff_at: result.cutoff_at,
            deleted: result
                .deleted
                .into_iter()
                .map(retention_item_response)
                .collect(),
            skipped: result
                .skipped
                .into_iter()
                .map(retention_skip_response)
                .collect(),
        },
    })
}

#[tauri::command]
pub fn investigation_schedule_list(
    state: State<'_, AppState>,
) -> Result<InvestigationScheduleListResponse, String> {
    let schedules = state
        .database
        .investigations()
        .list_schedules(DEFAULT_SCHEDULE_LIMIT)
        .map_err(|error| error.to_string())?;
    Ok(InvestigationScheduleListResponse { schedules })
}

#[tauri::command]
pub fn investigation_schedule_runs(
    state: State<'_, AppState>,
    schedule_id: String,
) -> Result<InvestigationScheduleRunListResponse, String> {
    let schedule_id = validate_id(&schedule_id, "schedule id")?;
    let runs = state
        .database
        .investigations()
        .list_schedule_runs(&schedule_id, DEFAULT_SCHEDULE_LIMIT)
        .map_err(|error| error.to_string())?;
    Ok(InvestigationScheduleRunListResponse { runs })
}

#[tauri::command]
pub fn investigation_schedule_create(
    state: State<'_, AppState>,
    input: InvestigationScheduleCreateInput,
) -> Result<InvestigationScheduleResponse, String> {
    let task_id = validate_id(&input.task_id, "task id")?;
    let task = state
        .database
        .investigations()
        .get_task(&task_id)
        .map_err(|error| error.to_string())?;
    validate_scheduler_task(&task)?;
    let now = yukinal_core::sidecar::iso8601_now();
    let next_run_at = input.next_run_at.unwrap_or_else(|| now.clone());
    let budget = input.budget.unwrap_or_else(|| task.budget.clone());
    let schedule = InvestigationSchedule {
        id: input
            .id
            .map(|id| validate_id(&id, "schedule id"))
            .transpose()?
            .unwrap_or_else(|| crate::commands::server::next_id("schedule")),
        task_id,
        status: InvestigationScheduleStatus::Active,
        interval_seconds: input.interval_seconds,
        cooldown_seconds: input
            .cooldown_seconds
            .unwrap_or(DEFAULT_SCHEDULE_COOLDOWN_SECONDS),
        dedupe_window_seconds: input
            .dedupe_window_seconds
            .unwrap_or(DEFAULT_SCHEDULE_DEDUPE_SECONDS),
        max_concurrent_runs: input
            .max_concurrent_runs
            .unwrap_or(DEFAULT_SCHEDULE_MAX_CONCURRENT_RUNS),
        budget,
        notification_policy: input
            .notification_policy
            .unwrap_or(InvestigationNotificationPolicy::OnChange),
        next_run_at,
        baseline_run_id: None,
        last_run_at: None,
        last_outcome: None,
        last_error: None,
        created_at: now.clone(),
        updated_at: now,
    };
    validate_schedule_input(&schedule)?;
    state
        .database
        .investigations()
        .create_schedule(&schedule)
        .map_err(|error| error.to_string())?;
    Ok(InvestigationScheduleResponse { schedule })
}

#[tauri::command]
pub fn investigation_schedule_update(
    state: State<'_, AppState>,
    input: InvestigationScheduleUpdateInput,
) -> Result<InvestigationScheduleResponse, String> {
    let schedule_id = validate_id(&input.schedule_id, "schedule id")?;
    let mut schedule = state
        .database
        .investigations()
        .get_schedule(&schedule_id)
        .map_err(|error| error.to_string())?;
    let task = state
        .database
        .investigations()
        .get_task(&schedule.task_id)
        .map_err(|error| error.to_string())?;
    let baseline_only_update = input.baseline_run_id.is_some()
        && input.status.is_none()
        && input.interval_seconds.is_none()
        && input.cooldown_seconds.is_none()
        && input.dedupe_window_seconds.is_none()
        && input.max_concurrent_runs.is_none()
        && input.budget.is_none()
        && input.notification_policy.is_none()
        && input.next_run_at.is_none();
    if !baseline_only_update && schedule_update_requires_live_task(input.status) {
        validate_scheduler_task(&task)?;
    }
    if schedule.status == InvestigationScheduleStatus::Revoked
        && input
            .status
            .is_some_and(|status| status != InvestigationScheduleStatus::Revoked)
    {
        return Err("revoked schedules cannot be reactivated".into());
    }
    if let Some(status) = input.status {
        schedule.status = status;
    }
    if let Some(value) = input.interval_seconds {
        schedule.interval_seconds = value;
    }
    if let Some(value) = input.cooldown_seconds {
        schedule.cooldown_seconds = value;
    }
    if let Some(value) = input.dedupe_window_seconds {
        schedule.dedupe_window_seconds = value;
    }
    if let Some(value) = input.max_concurrent_runs {
        schedule.max_concurrent_runs = value;
    }
    if let Some(value) = input.budget {
        schedule.budget = value;
    }
    if let Some(value) = input.notification_policy {
        schedule.notification_policy = value;
    }
    if let Some(value) = input.next_run_at {
        schedule.next_run_at = value;
    }
    if let Some(value) = input.baseline_run_id {
        if let Some(value) = value {
            let baseline_run_id = validate_id(&value, "baseline run id")?;
            let baseline_run = state
                .database
                .investigations()
                .get_schedule_run(&baseline_run_id)
                .map_err(|error| error.to_string())?;
            if baseline_run.schedule_id != schedule.id {
                return Err("baseline run must belong to this schedule".into());
            }
            if baseline_run.status
                != yukinal_database::models::InvestigationScheduleRunStatus::Succeeded
            {
                return Err("baseline run must be a succeeded schedule run".into());
            }
            if state
                .database
                .investigations()
                .list_evidence_for_run(&baseline_run.id, 1)
                .map_err(|error| error.to_string())?
                .is_empty()
            {
                return Err("baseline run must contain at least one evidence item".into());
            }
            schedule.baseline_run_id = Some(baseline_run_id);
        } else {
            schedule.baseline_run_id = None;
        }
    }
    schedule.updated_at = yukinal_core::sidecar::iso8601_now();
    validate_schedule_input(&schedule)?;
    state
        .database
        .investigations()
        .update_schedule(&schedule)
        .map_err(|error| error.to_string())?;
    Ok(InvestigationScheduleResponse { schedule })
}

#[tauri::command]
pub fn investigation_schedule_tick(
    state: State<'_, AppState>,
    input: InvestigationScheduleTickInput,
) -> Result<InvestigationScheduleTickResponse, String> {
    let now = input.now.unwrap_or_else(yukinal_core::sidecar::iso8601_now);
    let limit = input.limit.unwrap_or(16);
    if !(1..=100).contains(&limit) {
        return Err("schedule tick limit must be between 1 and 100".into());
    }
    let (claimed, skipped) = state
        .database
        .investigations()
        .claim_due_schedule_runs(&now, limit)
        .map_err(|error| error.to_string())?;
    Ok(InvestigationScheduleTickResponse { claimed, skipped })
}

#[tauri::command]
pub fn investigation_task_status_update(
    state: State<'_, AppState>,
    task_id: String,
    status: TaskStatus,
    completed_at: Option<String>,
) -> Result<InvestigationTaskResponse, String> {
    let task_id = validate_id(&task_id, "task id")?;
    let current = state
        .database
        .investigations()
        .get_task(&task_id)
        .map_err(|error| error.to_string())?;
    if !can_transition(current.status, status) {
        return Err(format!(
            "investigation task cannot transition from {} to {}",
            current.status.as_str(),
            status.as_str()
        ));
    }
    let terminal = is_terminal(status);
    let completed_at = match (terminal, completed_at) {
        (true, Some(value)) => Some(validate_timestamp(&value)?),
        (true, None) => Some(yukinal_core::sidecar::iso8601_now()),
        (false, Some(_)) => return Err("completedAt is only valid for a terminal task".into()),
        (false, None) => None,
    };
    let updated = state
        .database
        .investigations()
        .update_task_status(
            &task_id,
            status,
            &yukinal_core::sidecar::iso8601_now(),
            completed_at.as_deref(),
        )
        .map_err(|error| error.to_string())?;
    Ok(InvestigationTaskResponse { task: updated })
}

#[tauri::command]
pub fn investigation_task_recover(
    state: State<'_, AppState>,
    input: InvestigationTaskRecoverInput,
) -> Result<InvestigationTaskResponse, String> {
    let task_id = validate_id(&input.task_id, "task id")?;
    let task = state
        .database
        .investigations()
        .get_task(&task_id)
        .map_err(|error| error.to_string())?;
    if matches!(task.status, TaskStatus::Completed | TaskStatus::Expired) {
        return Err("completed or expired investigation tasks cannot be recovered".into());
    }
    let selected_action = match input.option_id.as_deref() {
        Some(option_id) => Some(
            task.last_failure
                .as_ref()
                .and_then(|failure| failure.options.as_ref())
                .and_then(|options| options.iter().find(|option| option.id == option_id))
                .ok_or_else(|| "the selected failure option is no longer available".to_string())?
                .action,
        ),
        None => None,
    };
    if matches!(
        selected_action,
        Some(yukinal_database::models::FailureOptionAction::Inspect)
    ) {
        return Ok(InvestigationTaskResponse { task });
    }
    let now = yukinal_core::sidecar::iso8601_now();
    let mut failure = task.last_failure.clone().unwrap_or(InvestigationFailure {
        code: TaskFailureCode::Unknown,
        message: "任务由用户请求恢复，上一轮运行被标记为中断".into(),
        retryable: true,
        attempt: 1,
        at: now.clone(),
        detail: None,
        options: Some(super::failure_options(TaskFailureCode::Unknown, true)),
    });
    failure.attempt = failure.attempt.saturating_add(1);
    failure.at = now.clone();
    let mut recovery_detail = json!({
        "rollbackRequested": selected_action
            == Some(yukinal_database::models::FailureOptionAction::Rollback),
        "requiresFreshBaseline": true,
    });
    if let Some(option_id) = input.option_id.as_deref() {
        recovery_detail["selectedOption"] = json!(option_id);
    }
    failure.detail = Some(recovery_detail);
    if let Some(mut plan) = state
        .database
        .investigations()
        .latest_plan(&task.id)
        .map_err(|error| error.to_string())?
    {
        let mut artifacts = state
            .database
            .investigations()
            .list_artifacts(&task.id, 128)
            .map_err(|error| error.to_string())?;
        invalidate_plan_after_recovery(&mut plan, &mut artifacts, &now);
        for artifact in artifacts.into_iter().filter(|artifact| {
            artifact.kind == TaskArtifactKind::Baseline
                && artifact.status == TaskArtifactStatus::Superseded
                && artifact.plan_id.as_deref() == Some(plan.id.as_str())
                && artifact.updated_at == now
        }) {
            state
                .database
                .investigations()
                .upsert_artifact(&artifact)
                .map_err(|error| error.to_string())?;
        }
        state
            .database
            .investigations()
            .save_plan(&plan)
            .map_err(|error| error.to_string())?;
    }
    let mut recovered = state
        .database
        .investigations()
        .recover_task(&task_id, &now, &failure)
        .map_err(|error| error.to_string())?;
    match selected_action {
        Some(yukinal_database::models::FailureOptionAction::Stop) => {
            recovered = state
                .database
                .investigations()
                .update_task_status(&task_id, TaskStatus::Stopped, &now, Some(&now))
                .map_err(|error| error.to_string())?;
        }
        Some(yukinal_database::models::FailureOptionAction::WaitUser) => {
            recovered = state
                .database
                .investigations()
                .update_task_status(&task_id, TaskStatus::WaitingUser, &now, None)
                .map_err(|error| error.to_string())?;
        }
        Some(
            yukinal_database::models::FailureOptionAction::Retry
            | yukinal_database::models::FailureOptionAction::Replan
            | yukinal_database::models::FailureOptionAction::Resume,
        )
        | None => {}
        Some(yukinal_database::models::FailureOptionAction::Rollback) => {
            // A rollback option is a user-approved request to plan the inverse change,
            // not permission to replay free-form text. Keep the task waiting until the
            // Agent presents a new, separately gated rollback plan.
            recovered = state
                .database
                .investigations()
                .update_task_status(&task_id, TaskStatus::WaitingUser, &now, None)
                .map_err(|error| error.to_string())?;
        }
        Some(yukinal_database::models::FailureOptionAction::Inspect) => unreachable!(),
    }
    Ok(InvestigationTaskResponse { task: recovered })
}

/// Recovery cannot trust an old approval or a baseline captured before a
/// disconnected run. Keep the declarative plan visible for the next Agent turn,
/// but force it to re-establish the target state and obtain a fresh decision.
fn invalidate_plan_after_recovery(
    plan: &mut yukinal_database::models::InvestigationPlan,
    artifacts: &mut [yukinal_database::models::InvestigationArtifact],
    now: &str,
) {
    plan.approval = Some(InvestigationPlanApproval {
        status: PlanApprovalStatus::Pending,
        source: None,
        option_id: None,
        approved_at: None,
        note: Some("恢复后需要重新核对目标、基线和用户选择".into()),
    });
    if let Some(window) = plan.observation_window.as_mut() {
        if window.status == yukinal_database::models::ObservationWindowStatus::Running {
            window.status = yukinal_database::models::ObservationWindowStatus::Cancelled;
            window.last_failure = Some("运行中断，观察窗口必须在重新规划后重启".into());
        }
    }
    plan.updated_at = now.to_string();
    for artifact in artifacts.iter_mut().filter(|artifact| {
        artifact.kind == TaskArtifactKind::Baseline
            && matches!(
                artifact.status,
                TaskArtifactStatus::Ready | TaskArtifactStatus::Succeeded
            )
            && artifact.plan_id.as_deref() == Some(plan.id.as_str())
    }) {
        artifact.status = TaskArtifactStatus::Superseded;
        artifact.summary = "恢复后旧基线失效，必须重新采集并核对目标状态".into();
        artifact.updated_at = now.to_string();
    }
}

#[tauri::command]
pub async fn investigation_brief_select(
    app: AppHandle,
    state: State<'_, AppState>,
    input: InvestigationBriefSelectInput,
) -> Result<InvestigationBriefResponse, String> {
    let task_id = validate_id(&input.task_id, "task id")?;
    let brief_id = validate_id(&input.brief_id, "brief id")?;
    let option_id = validate_id(&input.option_id, "option id")?;
    let task = state
        .database
        .investigations()
        .get_task(&task_id)
        .map_err(|error| error.to_string())?;
    if is_terminal(task.status) {
        return Err("terminal investigation tasks cannot select a decision option".into());
    }
    let brief = state
        .database
        .investigations()
        .select_decision_brief_option(&task_id, &brief_id, &option_id)
        .map_err(|error| error.to_string())?;
    let continuation = continuation_for_option(&brief, &option_id);
    // A read-only continuation is not an approval for the plan that happened to
    // produce the brief.  This matters for scheduler briefs, which may point at an
    // older plan only so the UI can link the evidence back to its run.  Only the
    // explicit start_plan continuation may turn the linked dry-run plan into a user
    // approval; wait_user and continue_readonly keep it pending.  Stop is handled by
    // the same terminal cancellation path as the task-stop command.
    if should_approve_selected_plan(continuation) {
        if let Some(plan_id) = brief.plan_id.as_deref() {
            if let Some(mut plan) = state
                .database
                .investigations()
                .latest_plan(&task_id)
                .map_err(|error| error.to_string())?
            {
                if plan.id == plan_id {
                    let now = yukinal_core::sidecar::iso8601_now();
                    plan.approval = Some(InvestigationPlanApproval {
                        status: PlanApprovalStatus::Approved,
                        source: Some(PlanApprovalSource::User),
                        option_id: Some(option_id.clone()),
                        approved_at: Some(now.clone()),
                        note: Some("用户选择了该决策摘要方案；行动步骤仍受逐项权限审批约束".into()),
                    });
                    plan.updated_at = now;
                    state
                        .database
                        .investigations()
                        .save_plan(&plan)
                        .map_err(|error| error.to_string())?;
                }
            }
        }
    } else if continuation == DecisionOptionContinuation::Stop {
        stop_task_for_decision(&app, &state, &task_id).await?;
    }
    Ok(InvestigationBriefResponse {
        brief,
        continuation,
    })
}

/// Apply the terminal meaning of a decision option without leaving an active
/// sidecar run behind.  A selected stop option can arrive after a normal run has
/// already been fenced (the usual waiting-user case), or while a durable run is
/// still active because the UI and event stream raced.  In the latter case reuse
/// the same cancellation/fence path as `agent_run_stop` and refuse to guess when
/// the sidecar did not acknowledge the stop.
async fn stop_task_for_decision(
    app: &AppHandle,
    state: &AppState,
    task_id: &str,
) -> Result<(), String> {
    let task = state
        .database
        .investigations()
        .get_task(task_id)
        .map_err(|error| error.to_string())?;
    if is_terminal(task.status) {
        return Ok(());
    }

    if let Some(active_run_id) = task.active_run_id.clone() {
        let stopped =
            crate::commands::agent_run::stop_investigation_run(app, state, &active_run_id).await?;
        let current = state
            .database
            .investigations()
            .get_task(task_id)
            .map_err(|error| error.to_string())?;
        if current.active_run_id.as_deref() == Some(active_run_id.as_str()) {
            if close_task_after_stop(state, task_id, &active_run_id, current.status, stopped)? {
                return Ok(());
            }
            return Err("the selected task still has an active Agent run".into());
        }
        if is_terminal(current.status) {
            return Ok(());
        }
        if current.active_run_id.is_some() {
            return Err("the selected task was taken over by another Agent run".into());
        }
        stop_task_status(state, current.status, task_id)?;
        return Ok(());
    }

    stop_task_status(state, task.status, task_id)
}

/// Normally `stop_investigation_run` has already closed this fence through the
/// synthetic terminal event.  This fallback only handles a durable pointer whose
/// event could not be replayed (for example an old database row with a missing or
/// already-terminal run); it still requires the pointer to be the same one read by
/// the caller, so a newer run can never be cleared accidentally.
fn close_task_after_stop(
    state: &AppState,
    task_id: &str,
    run_id: &str,
    current_status: TaskStatus,
    stopped: bool,
) -> Result<bool, String> {
    if is_terminal(current_status) {
        return Ok(true);
    }
    let run = match state.database.investigations().get_run(run_id) {
        Ok(run) if run.task_id == task_id => Some(run),
        Ok(_) => return Err("active investigation run belongs to another task".into()),
        Err(yukinal_database::DatabaseError::NotFound) => None,
        Err(error) => return Err(error.to_string()),
    };
    let run_is_terminal = run.as_ref().is_none_or(|run| {
        matches!(
            run.status,
            yukinal_database::models::InvestigationRunStatus::Completed
                | yukinal_database::models::InvestigationRunStatus::Failed
                | yukinal_database::models::InvestigationRunStatus::Cancelled
                | yukinal_database::models::InvestigationRunStatus::Interrupted
        )
    });
    if !stopped && !run_is_terminal {
        return Ok(false);
    }
    let now = yukinal_core::sidecar::iso8601_now();
    let failure = run.as_ref().and_then(|run| run.failure.as_ref());
    let updated = state
        .database
        .investigations()
        .update_task_progress_if_active(
            &TaskProgressUpdate {
                id: task_id,
                status: TaskStatus::Stopped,
                phase: TaskPhase::Recovery,
                active_run_id: None,
                last_failure: failure,
                updated_at: &now,
                completed_at: Some(&now),
            },
            run_id,
        )
        .map_err(|error| error.to_string())?;
    Ok(updated.is_some())
}

fn stop_task_status(state: &AppState, current: TaskStatus, task_id: &str) -> Result<(), String> {
    if !can_transition(current, TaskStatus::Stopped) {
        return Err(format!(
            "investigation task cannot stop from {}",
            current.as_str()
        ));
    }
    let now = yukinal_core::sidecar::iso8601_now();
    state
        .database
        .investigations()
        .update_task_status(task_id, TaskStatus::Stopped, &now, Some(&now))
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn continuation_for_option(brief: &DecisionBrief, option_id: &str) -> DecisionOptionContinuation {
    brief
        .options
        .iter()
        .find(|option| option.id == option_id)
        .and_then(|option| option.continuation)
        .unwrap_or(DecisionOptionContinuation::WaitUser)
}

fn should_approve_selected_plan(continuation: DecisionOptionContinuation) -> bool {
    continuation == DecisionOptionContinuation::StartPlan
}

fn validate_create_input(
    state: &AppState,
    input: InvestigationTaskCreateInput,
) -> Result<InvestigationTask, String> {
    let objective = input.objective.trim().to_string();
    if objective.is_empty() || objective.chars().count() > MAX_OBJECTIVE_CHARS {
        return Err(format!(
            "objective must be between 1 and {MAX_OBJECTIVE_CHARS} characters"
        ));
    }
    if input.success_criteria.is_empty() || input.success_criteria.len() > MAX_SUCCESS_CRITERIA {
        return Err(format!(
            "successCriteria must contain 1 to {MAX_SUCCESS_CRITERIA} items"
        ));
    }
    let success_criteria = input
        .success_criteria
        .into_iter()
        .map(|criterion| {
            let criterion = criterion.trim().to_string();
            if criterion.is_empty() || criterion.chars().count() > MAX_SUCCESS_CRITERION_CHARS {
                Err(format!(
                    "each success criterion must be between 1 and {MAX_SUCCESS_CRITERION_CHARS} characters"
                ))
            } else {
                Ok(criterion)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    let id = input
        .id
        .map(|value| validate_id(&value, "task id"))
        .transpose()?
        .unwrap_or_else(|| crate::commands::server::next_id("task"));
    let workspace_id = reconcile_reference(
        input.workspace_id,
        input.scope.workspace_id.clone(),
        "workspace",
    )?;
    let server_id = reconcile_reference(input.server_id, input.scope.server_id.clone(), "server")?;

    if input.scope.host == InvestigationTargetHost::Remote && server_id.is_none() {
        return Err("remote investigation tasks require a server scope".into());
    }
    if input.scope.host == InvestigationTargetHost::Local && server_id.is_some() {
        return Err("local investigation tasks cannot carry a server scope".into());
    }
    if let Some(server_id) = server_id.as_deref() {
        if !yukinal_core::ids::is_stable_server_id(server_id) {
            return Err("server scope must be an opaque srv_ id".into());
        }
        state
            .database
            .servers()
            .get(server_id)
            .map_err(|error| format!("server scope is not available: {error}"))?;
    }
    if let Some(workspace_id) = workspace_id.as_deref() {
        state
            .database
            .workspaces()
            .get(workspace_id)
            .map_err(|error| format!("workspace scope is not available: {error}"))?;
    }

    let scope = InvestigationTarget {
        host: input.scope.host,
        server_id,
        workspace_id,
        environment: input.scope.environment,
    };
    let guardrails = validate_guardrails(input.guardrails)?;
    if input.mode == InvestigationRunMode::Readonly
        && input.automation_level != TaskAutomationLevel::Readonly
    {
        return Err("readonly run mode cannot create an executable task".into());
    }
    if input.mode == InvestigationRunMode::Plan
        && input.automation_level == TaskAutomationLevel::Execute
    {
        return Err("plan mode can propose a change but cannot authorize execution".into());
    }
    let budget = input.budget.unwrap_or(TaskBudget {
        max_steps: DEFAULT_MAX_STEPS,
        max_run_ms: DEFAULT_MAX_RUN_MS,
        max_attempts: DEFAULT_MAX_ATTEMPTS,
    });
    if budget.max_steps == 0 || budget.max_steps > 10_000 {
        return Err("budget.maxSteps must be between 1 and 10000".into());
    }
    if budget.max_run_ms == 0 || budget.max_run_ms > 86_400_000 {
        return Err("budget.maxRunMs must be between 1 and 86400000".into());
    }
    if budget.max_attempts == 0 || budget.max_attempts > MAX_ATTEMPTS {
        return Err(format!(
            "budget.maxAttempts must be between 1 and {MAX_ATTEMPTS}"
        ));
    }
    let now = yukinal_core::sidecar::iso8601_now();
    Ok(InvestigationTask {
        id,
        workspace_id: scope.workspace_id.clone(),
        server_id: scope.server_id.clone(),
        objective,
        success_criteria,
        scope,
        guardrails,
        mode: input.mode,
        permission_mode: input.permission_mode,
        automation_level: input.automation_level,
        created_by: "user".into(),
        phase: TaskPhase::Investigating,
        status: TaskStatus::Pending,
        budget,
        created_at: now.clone(),
        updated_at: now,
        completed_at: None,
        active_run_id: None,
        last_failure: None,
    })
}

fn autonomous_task_prompt(task: &InvestigationTask) -> String {
    let criteria = task
        .success_criteria
        .iter()
        .enumerate()
        .map(|(index, criterion)| format!("{}. {}", index + 1, criterion))
        .collect::<Vec<_>>()
        .join("\n");
    let mode_instruction = match task.mode {
        InvestigationRunMode::Readonly => {
            "只调用任务范围内已允许的只读工具；不要写入、重启、删除或改变目标状态。"
        }
        InvestigationRunMode::Plan => {
            "先调查并生成宿主可验证的 dry-run 计划；没有用户选择和审批时不要执行行动步骤。"
        }
        InvestigationRunMode::Goal if task.permission_mode == InvestigationPermissionMode::Auto
            && task.automation_level == TaskAutomationLevel::Execute
            && task.scope.host == InvestigationTargetHost::Remote
            && matches!(
                task.scope.environment,
                yukinal_database::models::Environment::Development
                    | yukinal_database::models::Environment::Staging
            ) => {
            "按宿主保存的计划逐步推进；本任务已明确启用受限 auto 委托，medium 风险的配置备份/编辑可在宿主复核后自动推进；备份清理、恢复、重启、包安装及其他高风险或未明确授权的动作必须停下等待用户审批。"
        }
        InvestigationRunMode::Goal => {
            "按宿主保存的计划逐步推进；高风险或未明确授权的动作必须停下等待用户审批。"
        }
    };
    let recovery_instruction = if rollback_requested(task) {
        "\n\n用户选择了“先规划回退”。这不是对旧回退文本或旧工具调用的授权：请只生成一份新的、独立的回退 dry-run 计划，重新校验当前基线、目标范围和精确参数绑定；涉及写入时仍须取得本轮审批，禁止直接重放旧动作。"
    } else if fresh_baseline_required(task) {
        "\n\n这是一次中断后的恢复运行：旧计划审批、旧变更前基线和正在进行的观察窗口已经失效。请先重新校验目标身份、收集新的只读证据并生成/确认新的计划；在新基线和本轮审批完成前不要重放旧行动。"
    } else {
        ""
    };
    let guardrail_instruction = format_guardrails_for_prompt(&task.guardrails);
    format!(
        "你正在自主推进一项由用户创建的可恢复运维任务。\n\n目标：\n{}\n\n完成标准：\n{}\n\n边界：\n{}{}{}\n\n先调用 investigation.plan 声明有序步骤；如果目标属于只读健康检查、受保护配置编辑、容器重启、systemd 服务重启、明确的 apt/dnf 包安装或受限 deploy_sequence，可以调用 investigation.playbook 生成宿主拥有的 dry-run 计划。playbook 只记录计划，不直接执行写入。之后按计划收集证据、记录 Finding 和决策摘要。每个事实都要引用证据；证据不足时明确停在等待用户，而不是猜测已经完成。",
        task.objective, criteria, mode_instruction, recovery_instruction, guardrail_instruction
    )
}

fn validate_guardrails(
    raw: Option<InvestigationTaskGuardrails>,
) -> Result<InvestigationTaskGuardrails, String> {
    let mut guardrails = raw.unwrap_or_default();
    guardrails.not_before_at =
        normalize_guardrail_timestamp(guardrails.not_before_at, "notBeforeAt")?;
    guardrails.expires_at = normalize_guardrail_timestamp(guardrails.expires_at, "expiresAt")?;
    if guardrails.forbidden_tools.len() > MAX_GUARDRAIL_TOOLS {
        return Err(format!(
            "guardrails.forbiddenTools must contain at most {MAX_GUARDRAIL_TOOLS} items"
        ));
    }
    let mut tools = Vec::with_capacity(guardrails.forbidden_tools.len());
    for tool in guardrails.forbidden_tools {
        let tool = tool.trim().to_string();
        if tool.is_empty()
            || tool.chars().count() > MAX_GUARDRAIL_NAME_CHARS
            || tool.chars().any(char::is_whitespace)
        {
            return Err(format!(
                "each guardrails.forbiddenTools item must be a non-empty internal tool name of at most {MAX_GUARDRAIL_NAME_CHARS} characters"
            ));
        }
        if !tools.iter().any(|candidate: &String| candidate == &tool) {
            tools.push(tool);
        }
    }
    guardrails.forbidden_tools = tools;

    if guardrails.forbidden_path_prefixes.len() > MAX_GUARDRAIL_PATH_PREFIXES {
        return Err(format!(
            "guardrails.forbiddenPathPrefixes must contain at most {MAX_GUARDRAIL_PATH_PREFIXES} items"
        ));
    }
    let mut prefixes = Vec::with_capacity(guardrails.forbidden_path_prefixes.len());
    for prefix in guardrails.forbidden_path_prefixes {
        let prefix = normalize_guardrail_path_prefix(&prefix)?;
        if !prefixes
            .iter()
            .any(|candidate: &String| candidate == &prefix)
        {
            prefixes.push(prefix);
        }
    }
    guardrails.forbidden_path_prefixes = prefixes;

    if let (Some(not_before), Some(expires)) = (
        guardrails.not_before_at.as_deref(),
        guardrails.expires_at.as_deref(),
    ) {
        let start = yukinal_time::parse_iso8601_utc(not_before)
            .ok_or_else(|| "guardrails.notBeforeAt must be a UTC ISO-8601 timestamp".to_string())?;
        let end = yukinal_time::parse_iso8601_utc(expires)
            .ok_or_else(|| "guardrails.expiresAt must be a UTC ISO-8601 timestamp".to_string())?;
        if end <= start {
            return Err("guardrails.expiresAt must be later than guardrails.notBeforeAt".into());
        }
        if end.saturating_sub(start) > MAX_GUARDRAIL_WINDOW_SECONDS {
            return Err("guardrails time window cannot exceed 365 days".into());
        }
    }
    Ok(guardrails)
}

fn normalize_guardrail_timestamp(
    value: Option<String>,
    field: &str,
) -> Result<Option<String>, String> {
    let Some(value) = value else { return Ok(None) };
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(format!("guardrails.{field} cannot be empty"));
    }
    if yukinal_time::parse_iso8601_utc(&value).is_none() {
        return Err(format!(
            "guardrails.{field} must be a UTC ISO-8601 timestamp"
        ));
    }
    Ok(Some(value))
}

fn normalize_guardrail_path_prefix(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > MAX_GUARDRAIL_PATH_CHARS {
        return Err(format!(
            "each guardrails.forbiddenPathPrefixes item must be 1-{MAX_GUARDRAIL_PATH_CHARS} characters"
        ));
    }
    if !value.starts_with('/') {
        return Err("guardrails.forbiddenPathPrefixes items must be absolute paths".into());
    }
    if value
        .bytes()
        .any(|byte| byte == 0 || byte == b'\r' || byte == b'\n')
    {
        return Err("guardrails.forbiddenPathPrefixes cannot contain control characters".into());
    }
    let mut parts = Vec::new();
    for part in value.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err("guardrails.forbiddenPathPrefixes cannot contain `..`".into()),
            value => parts.push(value),
        }
    }
    if parts.is_empty() {
        Ok("/".into())
    } else {
        Ok(format!("/{}", parts.join("/")))
    }
}

pub(crate) fn task_time_window_error(task: &InvestigationTask) -> Result<(), String> {
    let now = yukinal_time::now_epoch_seconds();
    if let Some(not_before) = task.guardrails.not_before_at.as_deref() {
        let Some(not_before) = yukinal_time::parse_iso8601_utc(not_before) else {
            return Err(
                "task guardrail notBeforeAt is invalid; task is blocked until it is repaired"
                    .into(),
            );
        };
        if now < not_before {
            return Err(format!(
                "task execution is not allowed before {}",
                task.guardrails.not_before_at.as_deref().unwrap_or_default()
            ));
        }
    }
    if let Some(expires) = task.guardrails.expires_at.as_deref() {
        let Some(expires) = yukinal_time::parse_iso8601_utc(expires) else {
            return Err(
                "task guardrail expiresAt is invalid; task is blocked until it is repaired".into(),
            );
        };
        if now >= expires {
            return Err(format!(
                "task execution time window expired at {}",
                task.guardrails.expires_at.as_deref().unwrap_or_default()
            ));
        }
    }
    Ok(())
}

fn format_guardrails_for_prompt(guardrails: &InvestigationTaskGuardrails) -> String {
    let mut lines = Vec::new();
    if let Some(not_before) = guardrails.not_before_at.as_deref() {
        lines.push(format!("不得早于 {not_before}"));
    }
    if let Some(expires) = guardrails.expires_at.as_deref() {
        lines.push(format!("不得晚于 {expires}"));
    }
    if !guardrails.forbidden_tools.is_empty() {
        lines.push(format!(
            "禁止工具：{}",
            guardrails.forbidden_tools.join(", ")
        ));
    }
    if !guardrails.forbidden_path_prefixes.is_empty() {
        lines.push(format!(
            "禁止路径前缀：{}",
            guardrails.forbidden_path_prefixes.join(", ")
        ));
    }
    if lines.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n宿主硬边界（不能通过提示词或重试绕过）：{}。",
            lines.join("；")
        )
    }
}

fn rollback_requested(task: &InvestigationTask) -> bool {
    task.last_failure
        .as_ref()
        .and_then(|failure| failure.detail.as_ref())
        .and_then(Value::as_object)
        .and_then(|detail| detail.get("rollbackRequested"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn fresh_baseline_required(task: &InvestigationTask) -> bool {
    task.last_failure
        .as_ref()
        .and_then(|failure| failure.detail.as_ref())
        .and_then(Value::as_object)
        .and_then(|detail| detail.get("requiresFreshBaseline"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn validate_scheduler_task(task: &InvestigationTask) -> Result<(), String> {
    if task.mode != InvestigationRunMode::Readonly
        || task.automation_level != TaskAutomationLevel::Readonly
    {
        return Err("scheduler triggers can only target read-only tasks".into());
    }
    if matches!(
        task.status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Stopped | TaskStatus::Expired
    ) {
        return Err("terminal investigation tasks cannot be scheduled".into());
    }
    Ok(())
}

fn schedule_update_requires_live_task(status: Option<InvestigationScheduleStatus>) -> bool {
    !matches!(status, Some(InvestigationScheduleStatus::Revoked))
}

fn validate_schedule_input(schedule: &InvestigationSchedule) -> Result<(), String> {
    if schedule.interval_seconds == 0 || schedule.interval_seconds > 86_400 {
        return Err("intervalSeconds must be between 1 and 86400".into());
    }
    if schedule.cooldown_seconds > 86_400 {
        return Err("cooldownSeconds must be at most 86400".into());
    }
    if schedule.dedupe_window_seconds == 0 || schedule.dedupe_window_seconds > 86_400 {
        return Err("dedupeWindowSeconds must be between 1 and 86400".into());
    }
    if schedule.max_concurrent_runs == 0 || schedule.max_concurrent_runs > 16 {
        return Err("maxConcurrentRuns must be between 1 and 16".into());
    }
    if schedule.budget.max_steps == 0
        || schedule.budget.max_steps > 10_000
        || schedule.budget.max_run_ms == 0
        || schedule.budget.max_run_ms > 86_400_000
        || schedule.budget.max_attempts == 0
        || schedule.budget.max_attempts > MAX_ATTEMPTS
    {
        return Err("schedule budget is outside the supported bounds".into());
    }
    if yukinal_time::parse_iso8601_utc(&schedule.next_run_at).is_none() {
        return Err("nextRunAt must be a UTC timestamp".into());
    }
    Ok(())
}

fn reconcile_reference(
    explicit: Option<String>,
    scoped: Option<String>,
    label: &str,
) -> Result<Option<String>, String> {
    match (explicit, scoped) {
        (Some(explicit), Some(scoped)) if explicit != scoped => {
            Err(format!("{label} id disagrees with the task scope"))
        }
        (Some(value), _) | (_, Some(value)) => Ok(Some(validate_id(&value, label)?)),
        (None, None) => Ok(None),
    }
}

fn validate_id(value: &str, label: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 256 || value.chars().any(char::is_control) {
        return Err(format!(
            "{label} must be between 1 and 256 visible characters"
        ));
    }
    Ok(value.to_string())
}

fn validate_timestamp(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 80 || value.chars().any(char::is_control) {
        return Err("completedAt must be a bounded timestamp".into());
    }
    Ok(value.to_string())
}

fn retention_cutoff(value: Option<String>) -> Result<String, String> {
    let cutoff = value.unwrap_or_else(|| {
        let now = yukinal_time::now_epoch_seconds();
        yukinal_time::iso8601_utc(now.saturating_sub(DEFAULT_RETENTION_DAYS * 86_400))
    });
    let cutoff = validate_timestamp(&cutoff)?;
    if yukinal_time::parse_iso8601_utc(&cutoff).is_none() {
        return Err("retention cutoff must be a UTC ISO-8601 timestamp".into());
    }
    Ok(cutoff)
}

fn parse_retention_kind(value: &str) -> Result<InvestigationRetentionKind, String> {
    match value.trim() {
        "evidence" => Ok(InvestigationRetentionKind::Evidence),
        "artifact" => Ok(InvestigationRetentionKind::Artifact),
        _ => Err("retention item kind must be evidence or artifact".into()),
    }
}

fn retention_item_response(
    item: yukinal_database::repositories::InvestigationRetentionItem,
) -> InvestigationRetentionItemResponse {
    InvestigationRetentionItemResponse {
        id: item.id,
        task_id: item.task_id,
        kind: item.kind.as_str().into(),
        created_at: item.created_at,
        bytes: item.bytes,
        reason: item.reason,
    }
}

fn retention_skip_response(
    item: yukinal_database::repositories::InvestigationRetentionSkip,
) -> InvestigationRetentionSkipResponse {
    InvestigationRetentionSkipResponse {
        id: item.id,
        kind: item.kind.as_str().into(),
        reason: item.reason,
    }
}

fn is_terminal(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Stopped | TaskStatus::Expired
    )
}

pub(crate) fn can_transition(from: TaskStatus, to: TaskStatus) -> bool {
    if from == to {
        return true;
    }
    match from {
        TaskStatus::Pending => matches!(
            to,
            TaskStatus::Investigating | TaskStatus::Stopped | TaskStatus::Expired
        ),
        TaskStatus::Investigating => matches!(
            to,
            TaskStatus::WaitingUser
                | TaskStatus::Executing
                | TaskStatus::Verifying
                | TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Stopped
                | TaskStatus::Expired
        ),
        TaskStatus::WaitingUser => matches!(
            to,
            TaskStatus::Investigating
                | TaskStatus::Executing
                | TaskStatus::Stopped
                | TaskStatus::Expired
        ),
        TaskStatus::Executing => matches!(
            to,
            TaskStatus::Verifying | TaskStatus::Failed | TaskStatus::Stopped | TaskStatus::Expired
        ),
        TaskStatus::Verifying => matches!(
            to,
            TaskStatus::WaitingUser
                | TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Stopped
                | TaskStatus::Expired
        ),
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Stopped | TaskStatus::Expired => {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        autonomous_task_prompt, can_transition, continuation_for_option,
        invalidate_plan_after_recovery, is_terminal, schedule_update_requires_live_task,
        should_approve_selected_plan, task_time_window_error, validate_guardrails,
    };
    use super::{
        InvestigationRunMode, InvestigationScheduleUpdateInput, InvestigationTarget,
        InvestigationTargetHost,
    };
    use super::{InvestigationTask, TaskAutomationLevel, TaskBudget, TaskPhase, TaskStatus};
    use serde_json::json;
    use yukinal_database::models::{
        DecisionBrief, DecisionOptionContinuation, Environment, InvestigationArtifact,
        InvestigationFailure, InvestigationObservationWindow, InvestigationPlan,
        InvestigationPlanApproval, InvestigationPlanStep, InvestigationScheduleStatus,
        InvestigationTaskGuardrails, ObservationWindowStatus, PlanApprovalSource,
        PlanApprovalStatus, PlanStatus, PlanStepKind, PlanStepStatus, TaskArtifactKind,
        TaskArtifactStatus, TaskFailureCode,
    };

    #[test]
    fn a_terminal_task_can_still_revoke_its_schedule() {
        assert!(!schedule_update_requires_live_task(Some(
            InvestigationScheduleStatus::Revoked
        )));
        assert!(schedule_update_requires_live_task(Some(
            InvestigationScheduleStatus::Active
        )));
        assert!(schedule_update_requires_live_task(Some(
            InvestigationScheduleStatus::Paused
        )));
        assert!(schedule_update_requires_live_task(None));
    }

    #[test]
    fn schedule_baseline_update_distinguishes_omitted_from_explicit_clear() {
        let omitted: InvestigationScheduleUpdateInput =
            serde_json::from_value(json!({ "scheduleId": "schedule_01" }))
                .expect("omitted baseline should deserialize");
        assert!(omitted.baseline_run_id.is_none());

        let cleared: InvestigationScheduleUpdateInput = serde_json::from_value(json!({
            "scheduleId": "schedule_01",
            "baselineRunId": null
        }))
        .expect("null baseline should deserialize as an explicit clear");
        assert_eq!(cleared.baseline_run_id, Some(None));

        let selected: InvestigationScheduleUpdateInput = serde_json::from_value(json!({
            "scheduleId": "schedule_01",
            "baselineRunId": "run_01"
        }))
        .expect("selected baseline should deserialize");
        assert_eq!(selected.baseline_run_id, Some(Some("run_01".into())));
    }

    #[test]
    fn omitted_decision_continuation_defaults_to_waiting_for_user() {
        let brief = DecisionBrief {
            id: "brief_legacy".into(),
            task_id: "task_legacy".into(),
            plan_id: None,
            generated_at: "2026-09-20T00:00:00Z".into(),
            status: yukinal_database::models::DecisionBriefStatus::Selected,
            finding_ids: vec![],
            options: vec![],
            selected_option_id: Some("option_legacy".into()),
        };
        assert_eq!(
            continuation_for_option(&brief, "option_legacy"),
            DecisionOptionContinuation::WaitUser
        );
    }

    #[test]
    fn stop_decision_continuation_is_preserved_for_host_task_finalisation() {
        let brief = DecisionBrief {
            id: "brief_stop".into(),
            task_id: "task_stop".into(),
            plan_id: Some("plan_stop".into()),
            generated_at: "2026-09-20T00:00:00Z".into(),
            status: yukinal_database::models::DecisionBriefStatus::Presented,
            finding_ids: vec![],
            options: vec![yukinal_database::models::DecisionOption {
                id: "option_stop".into(),
                title: "停止任务".into(),
                summary: "不再启动下一轮运行".into(),
                impact: "任务进入停止终态".into(),
                risk_level: yukinal_database::models::RiskLevel::Low,
                evidence_ids: vec![],
                finding_ids: vec![],
                preview: None,
                verification: "确认任务状态为 stopped".into(),
                rollback: None,
                requires_approval: false,
                status: yukinal_database::models::DecisionOptionStatus::Available,
                continuation: Some(DecisionOptionContinuation::Stop),
            }],
            selected_option_id: None,
        };
        assert_eq!(
            continuation_for_option(&brief, "option_stop"),
            DecisionOptionContinuation::Stop
        );
        assert!(can_transition(TaskStatus::WaitingUser, TaskStatus::Stopped));
    }

    #[test]
    fn only_an_explicit_start_plan_continuation_approves_the_linked_plan() {
        assert!(should_approve_selected_plan(
            DecisionOptionContinuation::StartPlan
        ));
        assert!(!should_approve_selected_plan(
            DecisionOptionContinuation::ContinueReadonly
        ));
        assert!(!should_approve_selected_plan(
            DecisionOptionContinuation::WaitUser
        ));
        assert!(!should_approve_selected_plan(
            DecisionOptionContinuation::Stop
        ));
    }

    #[test]
    fn task_transitions_keep_terminal_rows_terminal() {
        assert!(can_transition(
            TaskStatus::Pending,
            TaskStatus::Investigating
        ));
        assert!(can_transition(TaskStatus::Verifying, TaskStatus::Completed));
        assert!(!can_transition(
            TaskStatus::Completed,
            TaskStatus::Investigating
        ));
        assert!(is_terminal(TaskStatus::Failed));
        assert!(!is_terminal(TaskStatus::WaitingUser));
    }

    #[test]
    fn autonomous_task_prompt_preserves_the_goal_and_completion_contract() {
        let task = InvestigationTask {
            id: "task_prompt".into(),
            workspace_id: None,
            server_id: None,
            objective: "检查本机服务是否仍在运行".into(),
            success_criteria: vec!["保存一条服务状态证据".into(), "说明未知项".into()],
            scope: InvestigationTarget {
                host: InvestigationTargetHost::Local,
                server_id: None,
                workspace_id: None,
                environment: Environment::Local,
            },
            guardrails: Default::default(),
            mode: InvestigationRunMode::Readonly,
            permission_mode: super::InvestigationPermissionMode::Ask,
            automation_level: TaskAutomationLevel::Readonly,
            created_by: "user".into(),
            phase: TaskPhase::Investigating,
            status: TaskStatus::Pending,
            budget: TaskBudget {
                max_steps: 5,
                max_run_ms: 60_000,
                max_attempts: 1,
            },
            created_at: "2026-09-20T00:00:00Z".into(),
            updated_at: "2026-09-20T00:00:00Z".into(),
            completed_at: None,
            active_run_id: None,
            last_failure: None,
        };
        let prompt = autonomous_task_prompt(&task);
        assert!(prompt.contains(&task.objective));
        assert!(prompt.contains("保存一条服务状态证据"));
        assert!(prompt.contains("不要写入、重启、删除"));
        assert!(prompt.contains("investigation.plan"));

        let mut delegated = task.clone();
        delegated.mode = InvestigationRunMode::Goal;
        delegated.permission_mode = super::InvestigationPermissionMode::Auto;
        delegated.automation_level = TaskAutomationLevel::Execute;
        delegated.server_id = Some("srv_prompt".into());
        delegated.scope.host = InvestigationTargetHost::Remote;
        delegated.scope.server_id = Some("srv_prompt".into());
        delegated.scope.environment = Environment::Staging;
        let delegated_prompt = autonomous_task_prompt(&delegated);
        assert!(delegated_prompt.contains("受限 auto 委托"));
        assert!(delegated_prompt.contains("medium 风险的配置备份/编辑"));
        assert!(delegated_prompt.contains("备份清理、恢复、重启、包安装"));
    }

    #[test]
    fn task_guardrails_are_normalized_and_have_a_bounded_window() {
        let guardrails = validate_guardrails(Some(InvestigationTaskGuardrails {
            not_before_at: Some(" 2026-09-20T01:00:00Z ".into()),
            expires_at: Some("2026-09-20T02:00:00.500Z".into()),
            forbidden_tools: vec![" docker.restart ".into(), "docker.restart".into()],
            forbidden_path_prefixes: vec!["/srv/app/private/".into(), "/srv/app/private".into()],
        }))
        .expect("valid task guardrails");
        assert_eq!(
            guardrails.not_before_at.as_deref(),
            Some("2026-09-20T01:00:00Z")
        );
        assert_eq!(
            guardrails.expires_at.as_deref(),
            Some("2026-09-20T02:00:00.500Z")
        );
        assert_eq!(guardrails.forbidden_tools, vec!["docker.restart"]);
        assert_eq!(guardrails.forbidden_path_prefixes, vec!["/srv/app/private"]);
        assert!(validate_guardrails(Some(InvestigationTaskGuardrails {
            not_before_at: Some("2026-09-20T02:00:00Z".into()),
            expires_at: Some("2026-09-20T01:00:00Z".into()),
            ..Default::default()
        }))
        .is_err());
        assert!(validate_guardrails(Some(InvestigationTaskGuardrails {
            forbidden_path_prefixes: vec!["relative/path".into()],
            ..Default::default()
        }))
        .is_err());
        let mut expired_task = InvestigationTask {
            id: "task_window".into(),
            workspace_id: None,
            server_id: None,
            objective: "window".into(),
            success_criteria: vec!["check".into()],
            scope: InvestigationTarget {
                host: InvestigationTargetHost::Local,
                server_id: None,
                workspace_id: None,
                environment: yukinal_database::models::Environment::Local,
            },
            guardrails: InvestigationTaskGuardrails {
                expires_at: Some("1970-01-01T00:00:01Z".into()),
                ..Default::default()
            },
            mode: InvestigationRunMode::Readonly,
            permission_mode: super::InvestigationPermissionMode::Ask,
            automation_level: TaskAutomationLevel::Readonly,
            created_by: "test".into(),
            phase: TaskPhase::Investigating,
            status: TaskStatus::Pending,
            budget: TaskBudget {
                max_steps: 1,
                max_run_ms: 1_000,
                max_attempts: 1,
            },
            created_at: "2026-09-20T00:00:00Z".into(),
            updated_at: "2026-09-20T00:00:00Z".into(),
            completed_at: None,
            active_run_id: None,
            last_failure: None,
        };
        assert!(task_time_window_error(&expired_task).is_err());
        expired_task.guardrails.expires_at = None;
        assert!(task_time_window_error(&expired_task).is_ok());
    }

    #[test]
    fn autonomous_task_prompt_turns_rollback_selection_into_a_new_gated_plan() {
        let mut task = InvestigationTask {
            id: "task_rollback_prompt".into(),
            workspace_id: None,
            server_id: None,
            objective: "恢复受保护配置".into(),
            success_criteria: vec!["验证配置恢复".into()],
            scope: InvestigationTarget {
                host: InvestigationTargetHost::Remote,
                server_id: Some("srv_rollback".into()),
                workspace_id: None,
                environment: Environment::Staging,
            },
            guardrails: Default::default(),
            mode: InvestigationRunMode::Goal,
            permission_mode: super::InvestigationPermissionMode::Ask,
            automation_level: TaskAutomationLevel::Execute,
            created_by: "user".into(),
            phase: TaskPhase::Recovery,
            status: TaskStatus::WaitingUser,
            budget: TaskBudget {
                max_steps: 5,
                max_run_ms: 60_000,
                max_attempts: 1,
            },
            created_at: "2026-09-20T00:00:00Z".into(),
            updated_at: "2026-09-20T00:00:00Z".into(),
            completed_at: None,
            active_run_id: None,
            last_failure: Some(InvestigationFailure {
                code: TaskFailureCode::CommandFailed,
                message: "验证失败".into(),
                retryable: false,
                attempt: 1,
                at: "2026-09-20T00:00:00Z".into(),
                detail: Some(json!({
                    "selectedOption": "rollback",
                    "rollbackRequested": true,
                })),
                options: None,
            }),
        };
        let prompt = autonomous_task_prompt(&task);
        assert!(prompt.contains("用户选择了“先规划回退”"));
        assert!(prompt.contains("重新校验当前基线"));
        assert!(prompt.contains("禁止直接重放旧动作"));

        task.last_failure.as_mut().unwrap().detail = Some(json!({
            "selectedOption": "retry",
            "rollbackRequested": false,
            "requiresFreshBaseline": true,
        }));
        let recovery_prompt = autonomous_task_prompt(&task);
        assert!(!recovery_prompt.contains("用户选择了“先规划回退”"));
        assert!(recovery_prompt.contains("旧计划审批、旧变更前基线和正在进行的观察窗口已经失效"));
    }

    #[test]
    fn recovery_invalidates_approval_baseline_and_running_observation() {
        let mut plan = InvestigationPlan {
            id: "plan_recovery".into(),
            task_id: "task_recovery".into(),
            revision: 1,
            status: PlanStatus::Active,
            created_at: "2026-09-20T00:00:00Z".into(),
            updated_at: "2026-09-20T00:00:01Z".into(),
            current_step_id: Some("verify".into()),
            approval: Some(InvestigationPlanApproval {
                status: PlanApprovalStatus::Approved,
                source: Some(PlanApprovalSource::User),
                option_id: Some("option_1".into()),
                approved_at: Some("2026-09-20T00:00:01Z".into()),
                note: None,
            }),
            observation_window: Some(InvestigationObservationWindow {
                duration_seconds: 60,
                interval_seconds: 10,
                allowed_tools: vec!["server.info".into()],
                success_criteria: vec!["healthy".into()],
                status: ObservationWindowStatus::Running,
                sample_count: 2,
                started_at: Some("2026-09-20T00:00:02Z".into()),
                deadline_at: Some("2026-09-20T00:01:02Z".into()),
                deadline_epoch_seconds: Some(1_000),
                last_sample_at: Some("2026-09-20T00:00:12Z".into()),
                last_sample_epoch_seconds: Some(950),
                last_failure: None,
            }),
            steps: vec![InvestigationPlanStep {
                id: "verify".into(),
                ordinal: 0,
                kind: PlanStepKind::Verification,
                title: "Verify".into(),
                purpose: "Check state".into(),
                allowed_tools: vec!["server.info".into()],
                input_bindings: None,
                idempotency: None,
                risk_level: None,
                requires_baseline: None,
                preconditions: None,
                verification_criteria: None,
                preview: None,
                rollback: None,
                target: None,
                evidence_ids: vec![],
                success_criteria: vec!["healthy".into()],
                requires_approval: false,
                max_attempts: 1,
                attempts: 1,
                status: PlanStepStatus::Running,
                started_at: Some("2026-09-20T00:00:02Z".into()),
                ended_at: None,
                last_deviation: None,
            }],
        };
        let mut artifacts = vec![InvestigationArtifact {
            id: "baseline_recovery".into(),
            task_id: "task_recovery".into(),
            run_id: None,
            plan_id: Some(plan.id.clone()),
            plan_step_id: Some("evidence".into()),
            phase: TaskPhase::Investigating,
            kind: TaskArtifactKind::Baseline,
            status: TaskArtifactStatus::Ready,
            title: "Baseline".into(),
            summary: "captured before interruption".into(),
            content: json!({ "revision": "a" }),
            evidence_ids: vec!["ev_1".into()],
            created_at: "2026-09-20T00:00:01Z".into(),
            updated_at: "2026-09-20T00:00:01Z".into(),
        }];

        invalidate_plan_after_recovery(&mut plan, &mut artifacts, "2026-09-20T00:02:00Z");

        assert_eq!(
            plan.approval.as_ref().map(|approval| approval.status),
            Some(PlanApprovalStatus::Pending)
        );
        assert_eq!(
            plan.observation_window.as_ref().map(|window| window.status),
            Some(ObservationWindowStatus::Cancelled)
        );
        assert_eq!(artifacts[0].status, TaskArtifactStatus::Superseded);
        assert_eq!(artifacts[0].updated_at, "2026-09-20T00:02:00Z");
    }
}
