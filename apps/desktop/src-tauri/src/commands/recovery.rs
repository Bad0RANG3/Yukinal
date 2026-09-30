//! Host-owned startup recovery for delegated, retryable task failures.
//!
//! A retryable transport/timeout failure on an explicitly delegated executable
//! task must be recoverable without the user keeping the investigation pane
//! open. The decision and the transaction therefore live here, in the host
//! lifecycle — not in React. The pane only displays the failure and offers the
//! user actions; it never decides on its own that a task may be retried.
//!
//! Scope and limits:
//!
//! This runs **once at application start**. Nothing survives the window and
//! nothing runs while the app is closed; that would be a separate background
//! service with its own authorization design.
//!
//! It only touches a task that the user already delegated (`auto` + `execute` +
//! `goal`) on a remote development/staging target, with a retryable failure and
//! attempt budget left. Everything else stays a visible, user-driven decision.
//!
//! The durable recovery transaction seals the old run and invalidates old
//! approvals/baselines before the new run starts, and the existing run-start
//! path re-checks the task's budget, time window and active-run fence.

use std::time::Duration;

use tauri::{AppHandle, Manager};
use yukinal_core::sidecar::iso8601_now;
use yukinal_database::models::{
    Environment, InvestigationFailure, InvestigationPermissionMode, InvestigationRunMode,
    InvestigationTargetHost, InvestigationTask, TaskAutomationLevel, TaskFailureCode, TaskPhase,
    TaskStatus,
};
use yukinal_database::repositories::TaskProgressUpdate;

use crate::state::AppState;

/// How long to wait for the sidecar autostart to finish before giving up. The
/// recovery needs a running sidecar to start a run; if it never comes up, the
/// failed task is left untouched for the user to recover from the pane.
const SIDECAR_WAIT_ATTEMPTS: u32 = 60;
const SIDECAR_WAIT_INTERVAL: Duration = Duration::from_millis(500);
const RECOVERY_SCAN_LIMIT: usize = 64;

/// Start the one-shot startup recovery. Safe to call once from `setup`.
pub(crate) fn start_auto_recovery(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let shutdown = state.shutdown.clone();
        if !wait_for_sidecar(&state, &shutdown).await {
            return;
        }
        if shutdown.is_cancelled() {
            return;
        }
        if let Err(error) = recover_eligible_tasks(&state, &shutdown).await {
            eprintln!("[yukinal] startup task recovery failed: {error}");
        }
    });
}

async fn wait_for_sidecar(
    state: &AppState,
    shutdown: &tokio_util::sync::CancellationToken,
) -> bool {
    for _ in 0..SIDECAR_WAIT_ATTEMPTS {
        if shutdown.is_cancelled() {
            return false;
        }
        if state.supervisor.status().await.running {
            return true;
        }
        tokio::select! {
            _ = shutdown.cancelled() => return false,
            _ = tokio::time::sleep(SIDECAR_WAIT_INTERVAL) => {}
        }
    }
    eprintln!("[yukinal] startup task recovery skipped: agent sidecar did not come up");
    false
}

async fn recover_eligible_tasks(
    state: &AppState,
    shutdown: &tokio_util::sync::CancellationToken,
) -> Result<(), String> {
    let tasks = state
        .database
        .investigations()
        .list_tasks(Some(TaskStatus::Failed), RECOVERY_SCAN_LIMIT)
        .map_err(|error| error.to_string())?;
    for task in tasks {
        if shutdown.is_cancelled() {
            return Ok(());
        }
        if !should_auto_recover(&task) {
            continue;
        }
        // Re-read immediately before acting: the row could have changed between
        // the list and here (a user action, a schedule, another window).
        let current = match state.database.investigations().get_task(&task.id) {
            Ok(current) => current,
            Err(yukinal_database::DatabaseError::NotFound) => continue,
            Err(error) => {
                eprintln!(
                    "[yukinal] startup task recovery could not read task {}: {error}",
                    task.id
                );
                continue;
            }
        };
        if !should_auto_recover(&current) {
            continue;
        }
        let recovered =
            match crate::commands::investigation::recover_investigation_task(state, &task.id, None)
            {
                Ok(recovered) => recovered,
                Err(error) => {
                    eprintln!(
                        "[yukinal] startup task recovery could not recover task {}: {error}",
                        task.id
                    );
                    continue;
                }
            };
        if recovered.status != TaskStatus::Investigating || recovered.active_run_id.is_some() {
            // A rollback/wait/stop choice (not produced here) or a race left the
            // task somewhere the host must not silently start. Leave it visible.
            continue;
        }
        if let Err(error) =
            crate::commands::investigation::start_investigation_task(state, &recovered.id).await
        {
            // The recovery already sealed the old run and cleared `activeRunId`;
            // a start failure must not leave the task looking like it is running.
            fail_recovered_start(state, &recovered, &error);
        }
    }
    Ok(())
}

/// Whether a failed task may be recovered automatically at startup.
///
/// This is the host-owned twin of the former UI predicate. It is deliberately
/// strict: anything not explicitly delegated to a bounded executable run on a
/// non-production remote target stays a user decision.
pub(crate) fn should_auto_recover(task: &InvestigationTask) -> bool {
    let Some(failure) = task.last_failure.as_ref() else {
        return false;
    };
    task.status == TaskStatus::Failed
        && task.active_run_id.is_none()
        && task.permission_mode == InvestigationPermissionMode::Auto
        && task.mode == InvestigationRunMode::Goal
        && task.automation_level == TaskAutomationLevel::Execute
        && task.scope.host == InvestigationTargetHost::Remote
        && matches!(
            task.scope.environment,
            Environment::Development | Environment::Staging
        )
        // `retryable` is a persisted projection for display/audit. The code is
        // the authoritative policy input: a legacy or damaged row must never
        // turn a non-retryable failure into an autonomous retry.
        && failure.code.retryable()
        && failure.attempt < task.budget.max_attempts
}

/// Put a task back into a visible failed state when the automatic start itself
/// fails, so a startup error is never presented as an in-progress run.
fn fail_recovered_start(state: &AppState, task: &InvestigationTask, error: &str) {
    let now = iso8601_now();
    let attempt = task
        .last_failure
        .as_ref()
        .map(|failure| failure.attempt)
        .unwrap_or(1);
    let message: String = error.chars().take(4_096).collect();
    let failure = InvestigationFailure {
        code: TaskFailureCode::Internal,
        message,
        retryable: false,
        attempt,
        at: now.clone(),
        detail: None,
        options: Some(crate::commands::failure_options(TaskFailureCode::Internal)),
    };
    if let Err(update_error) =
        state
            .database
            .investigations()
            .update_task_progress(&TaskProgressUpdate {
                id: &task.id,
                status: TaskStatus::Failed,
                phase: TaskPhase::Recovery,
                active_run_id: None,
                last_failure: Some(&failure),
                updated_at: &now,
                completed_at: Some(&now),
            })
    {
        eprintln!(
            "[yukinal] startup task recovery failed to persist the failure for {}: {update_error}",
            task.id
        );
    }
}

#[cfg(test)]
mod tests {
    use super::should_auto_recover;
    use crate::state::AppState;
    use yukinal_database::models::{
        Environment, InvestigationFailure, InvestigationPermissionMode, InvestigationRun,
        InvestigationRunMode, InvestigationRunStatus, InvestigationTarget, InvestigationTargetHost,
        InvestigationTask, InvestigationTaskGuardrails, TaskAutomationLevel, TaskBudget,
        TaskFailureCode, TaskPhase, TaskStatus,
    };

    fn delegated_task() -> InvestigationTask {
        InvestigationTask {
            id: "task_auto".into(),
            workspace_id: None,
            server_id: Some("srv_1".into()),
            objective: "检查 staging 服务".into(),
            success_criteria: vec!["收集健康状态".into()],
            scope: InvestigationTarget {
                host: InvestigationTargetHost::Remote,
                server_id: Some("srv_1".into()),
                workspace_id: None,
                environment: Environment::Staging,
            },
            guardrails: InvestigationTaskGuardrails::default(),
            mode: InvestigationRunMode::Goal,
            permission_mode: InvestigationPermissionMode::Auto,
            automation_level: TaskAutomationLevel::Execute,
            created_by: "user".into(),
            phase: TaskPhase::Recovery,
            status: TaskStatus::Failed,
            budget: TaskBudget {
                max_steps: 25,
                max_run_ms: 600_000,
                max_attempts: 3,
            },
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            completed_at: Some("2026-01-01T00:00:00Z".into()),
            active_run_id: None,
            last_failure: Some(InvestigationFailure {
                code: TaskFailureCode::Transport,
                message: "sidecar 传输失败".into(),
                retryable: true,
                attempt: 1,
                at: "2026-01-01T00:00:00Z".into(),
                detail: None,
                options: None,
            }),
        }
    }

    #[test]
    fn only_a_delegated_retryable_failure_is_recovered_at_startup() {
        let task = delegated_task();
        assert!(should_auto_recover(&task));

        let mut wrong_mode = task.clone();
        wrong_mode.mode = InvestigationRunMode::Readonly;
        assert!(!should_auto_recover(&wrong_mode));

        let mut asking = task.clone();
        asking.permission_mode = InvestigationPermissionMode::Ask;
        assert!(!should_auto_recover(&asking));

        let mut proposing = task.clone();
        proposing.automation_level = TaskAutomationLevel::Propose;
        assert!(!should_auto_recover(&proposing));

        let mut local = task.clone();
        local.scope.host = InvestigationTargetHost::Local;
        local.scope.server_id = None;
        local.scope.environment = Environment::Local;
        assert!(!should_auto_recover(&local));
    }

    #[test]
    fn production_targets_and_exhausted_attempts_stay_user_controlled() {
        let mut production = delegated_task();
        production.scope.environment = Environment::Production;
        assert!(!should_auto_recover(&production));

        // `retryable` is derived data. A stale false marker must not make a
        // transport failure ineligible, and a stale true marker must not
        // elevate an internal failure into an autonomous retry.
        let mut stale_display_flag = delegated_task();
        stale_display_flag.last_failure.as_mut().unwrap().retryable = false;
        assert!(should_auto_recover(&stale_display_flag));

        let mut non_retryable = delegated_task();
        let failure = non_retryable.last_failure.as_mut().unwrap();
        failure.code = TaskFailureCode::Internal;
        failure.retryable = true;
        assert!(!should_auto_recover(&non_retryable));

        let mut exhausted = delegated_task();
        exhausted.last_failure.as_mut().unwrap().attempt = 3;
        assert!(!should_auto_recover(&exhausted));

        let mut live = delegated_task();
        live.active_run_id = Some("run_live".into());
        assert!(!should_auto_recover(&live));
    }

    /// The host-owned recovery transaction seals a still-open run and clears the
    /// active-run fence before it returns the task to `investigating`. If it did
    /// not, the next start would be refused by the active-run check and the task
    /// would be stuck.
    #[test]
    fn recovery_seals_the_old_run_and_clears_the_active_fence() {
        let directory = std::env::temp_dir().join(format!(
            "yukinal-recovery-transaction-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        let state = AppState::bootstrap(&directory).expect("bootstrap recovery fixture");

        let mut task = delegated_task();
        task.active_run_id = Some("run_old".into());
        state
            .database
            .investigations()
            .create_task(&task)
            .expect("create task");
        state
            .database
            .investigations()
            .create_run(&InvestigationRun {
                id: "run_old".into(),
                task_id: task.id.clone(),
                session_id: None,
                message_id: None,
                trace_id: None,
                attempt: 1,
                phase: TaskPhase::Investigating,
                status: InvestigationRunStatus::Running,
                started_at: "2026-01-01T00:00:00Z".into(),
                updated_at: "2026-01-01T00:00:00Z".into(),
                ended_at: None,
                checkpoint: None,
                failure: None,
            })
            .expect("create run");

        let recovered =
            crate::commands::investigation::recover_investigation_task(&state, &task.id, None)
                .expect("recover the task");
        assert_eq!(recovered.status, TaskStatus::Investigating);
        assert_eq!(recovered.active_run_id, None);
        let sealed = state
            .database
            .investigations()
            .get_run("run_old")
            .expect("read the sealed run");
        assert_eq!(sealed.status, InvestigationRunStatus::Interrupted);

        let _ = std::fs::remove_dir_all(&directory);
    }
}
