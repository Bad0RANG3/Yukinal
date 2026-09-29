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

mod policy;
use policy::*;
pub(crate) use policy::{can_transition, task_time_window_error};

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
const MAX_GUARDRAIL_TOOLS: usize = 128;
const MAX_GUARDRAIL_PATH_PREFIXES: usize = 128;
const MAX_GUARDRAIL_NAME_CHARS: usize = 256;
const MAX_GUARDRAIL_PATH_CHARS: usize = 4_096;
/// Three years (ADR 0075; was one year). A long-running task may legitimately need a
/// window that spans a slow migration, while still remaining a finite bound.
const MAX_GUARDRAIL_WINDOW_SECONDS: u64 = 1095 * 86_400;

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
    start_investigation_task(&state, &task_id).await
}

/// Body of [`investigation_task_start`], callable from the host lifecycle
/// (startup auto-recovery) without a Tauri `State`.
pub(crate) async fn start_investigation_task(
    state: &AppState,
    task_id: &str,
) -> Result<InvestigationTaskStartResponse, String> {
    let task_id = validate_id(task_id, "task id")?;
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
    let response = crate::commands::agent_run::start_agent_run(
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
    let task = recover_investigation_task(&state, &input.task_id, input.option_id.as_deref())?;
    Ok(InvestigationTaskResponse { task })
}

/// Body of [`investigation_task_recover`], callable from the host lifecycle. The
/// selected option is optional: `None` is the retry/resume path the startup
/// recovery uses after it has already validated the task's envelope.
pub(crate) fn recover_investigation_task(
    state: &AppState,
    task_id: &str,
    option_id: Option<&str>,
) -> Result<InvestigationTask, String> {
    let task_id = validate_id(task_id, "task id")?;
    let task = state
        .database
        .investigations()
        .get_task(&task_id)
        .map_err(|error| error.to_string())?;
    if matches!(task.status, TaskStatus::Completed | TaskStatus::Expired) {
        return Err("completed or expired investigation tasks cannot be recovered".into());
    }
    let selected_action = match option_id {
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
        return Ok(task);
    }
    let now = yukinal_core::sidecar::iso8601_now();
    let mut failure = task.last_failure.clone().unwrap_or(InvestigationFailure {
        code: TaskFailureCode::Unknown,
        message: "任务由用户请求恢复，上一轮运行被标记为中断".into(),
        retryable: TaskFailureCode::Unknown.retryable(),
        attempt: 1,
        at: now.clone(),
        detail: None,
        options: Some(super::failure_options(TaskFailureCode::Unknown)),
    });
    failure.attempt = failure.attempt.saturating_add(1);
    failure.at = now.clone();
    let mut recovery_detail = json!({
        "rollbackRequested": selected_action
            == Some(yukinal_database::models::FailureOptionAction::Rollback),
        "requiresFreshBaseline": true,
    });
    if let Some(option_id) = option_id {
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
    Ok(recovered)
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

#[cfg(test)]
mod tests;
