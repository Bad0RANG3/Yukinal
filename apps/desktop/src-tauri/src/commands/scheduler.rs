//! Background scheduler for durable, read-only investigation triggers.
//!
//! The scheduler only claims rows that the database has already validated as
//! read-only. It never receives a provider secret from a schedule row: the key is
//! resolved from the OS credential store at the moment a claimed run is launched,
//! through the same path as a user-started Agent run.

use std::time::Duration;

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};
use tokio_util::sync::CancellationToken;

use crate::commands::{agent_run, investigation, provider, tauri_event_name};
use crate::state::AppState;
use yukinal_core::provider::runtime_provider_config;
use yukinal_database::models::{
    InvestigationRun, InvestigationRunMode, InvestigationRunStatus, InvestigationSchedule,
    InvestigationScheduleRun, InvestigationScheduleRunStatus, InvestigationScheduleStatus,
    TaskBudget, TaskFailureCode, TaskPhase, TaskStatus,
};
use yukinal_database::repositories::TaskProgressUpdate;

const TICK_INTERVAL: Duration = Duration::from_secs(15);
const INITIAL_DELAY: Duration = Duration::from_secs(5);
const CLAIM_LIMIT: usize = 8;

/// Start one process-local scheduler loop. SQLite remains the durable source of
/// truth, so a second loop (or a restart) can only claim a unique due key once.
pub(crate) fn start_scheduler(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let shutdown = state.shutdown.clone();
        let now = yukinal_core::sidecar::iso8601_now();
        if let Err(error) = state
            .database
            .investigations()
            .interrupt_active_investigation_runs(&now, "application_restarted")
        {
            eprintln!("[yukinal] investigation recovery failed: {error}");
        }
        if let Err(error) = state
            .database
            .investigations()
            .interrupt_schedule_runs(&now)
        {
            eprintln!("[yukinal] scheduler recovery failed: {error}");
        }
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(INITIAL_DELAY) => {}
        }
        let mut ticker = tokio::time::interval(TICK_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = ticker.tick() => {}
            }
            let now = yukinal_core::sidecar::iso8601_now();
            let claimed = match state
                .database
                .investigations()
                .claim_due_schedule_runs(&now, CLAIM_LIMIT)
            {
                Ok((claimed, skipped)) => {
                    if !skipped.is_empty() {
                        let _ = app.emit(
                            &tauri_event_name("investigation.schedule_skipped"),
                            json!({ "runs": skipped }),
                        );
                    }
                    claimed
                }
                Err(error) => {
                    eprintln!("[yukinal] scheduler tick failed: {error}");
                    continue;
                }
            };
            for run in claimed {
                let app = app.clone();
                let shutdown = shutdown.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = launch_claimed_run(&app, &run, &shutdown).await {
                        eprintln!("[yukinal] scheduled run {} failed: {error}", run.id);
                    }
                });
            }
        }
    });
}

async fn launch_claimed_run(
    app: &AppHandle,
    schedule_run: &InvestigationScheduleRun,
    shutdown: &CancellationToken,
) -> Result<(), String> {
    match launch_claimed_run_inner(app, schedule_run, shutdown).await {
        Ok(()) => Ok(()),
        Err(error) => {
            let state = app.state::<AppState>();
            close_admitted_launch_failure(
                app,
                &state,
                &schedule_run.task_id,
                &schedule_run.id,
                &error,
                yukinal_database::models::TaskFailureCode::Internal,
                "scheduler_error",
            )?;
            Err(error)
        }
    }
}

async fn launch_claimed_run_inner(
    app: &AppHandle,
    schedule_run: &InvestigationScheduleRun,
    shutdown: &CancellationToken,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let now = yukinal_core::sidecar::iso8601_now();
    state
        .database
        .investigations()
        .start_schedule_run(&schedule_run.id, &now)
        .map_err(|error| error.to_string())?;
    let mut schedule = state
        .database
        .investigations()
        .get_schedule_for_run(&schedule_run.id)
        .map_err(|error| error.to_string())?;
    let task = state
        .database
        .investigations()
        .get_task(&schedule_run.task_id)
        .map_err(|error| error.to_string())?;
    if is_terminal_task_status(task.status) {
        schedule.status = InvestigationScheduleStatus::Revoked;
        schedule.last_outcome = Some("task_terminal".into());
        schedule.last_error = Some("scheduled task is terminal; trigger was revoked".into());
        schedule.updated_at = now.clone();
        state
            .database
            .investigations()
            .update_schedule(&schedule)
            .map_err(|error| error.to_string())?;
        finish_schedule_skipped(
            app,
            &state,
            &schedule_run.id,
            "task_terminal",
            "scheduled task is terminal; trigger was revoked",
        )?;
        return Ok(());
    }
    if task.mode != InvestigationRunMode::Readonly
        || task.automation_level != yukinal_database::models::TaskAutomationLevel::Readonly
    {
        finish_schedule_failure(
            app,
            &state,
            &schedule_run.id,
            "task_safety_envelope_changed",
            "scheduled tasks remain read-only",
        )?;
        return Ok(());
    }
    if let Some(error) = schedule_time_window_block(&task) {
        finish_schedule_skipped(app, &state, &schedule_run.id, "outside_time_window", &error)?;
        return Ok(());
    }
    if task.active_run_id.is_some() {
        finish_schedule_failure(
            app,
            &state,
            &schedule_run.id,
            "task_already_running",
            "task already has an active Agent run",
        )?;
        return Ok(());
    }

    // A schedule may narrow a task's budget for periodic work, but it must never
    // widen the durable task envelope the user created.  Keep this calculation
    // host-owned and deterministic; the sidecar receives only the effective cap.
    let budget = effective_schedule_budget(&task.budget, &schedule.budget);

    provider::normalize_active_provider(&state)?;
    let provider_config_row = agent_run::resolve_provider(&state, None)?;
    let api_key = provider::resolve_api_key(&state, &provider_config_row)?;
    let provider_config = runtime_provider_config(
        &provider_config_row,
        &provider_config_row.model,
        api_key,
        120_000,
    );
    let attempt = state
        .database
        .investigations()
        .list_runs(&task.id, 64)
        .map_err(|error| error.to_string())?
        .iter()
        .map(|run| run.attempt)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    if attempt > budget.max_attempts {
        finish_schedule_failure(
            app,
            &state,
            &schedule_run.id,
            "budget_exhausted",
            "investigation attempt budget exhausted",
        )?;
        return Ok(());
    }
    state
        .database
        .investigations()
        .create_run(&InvestigationRun {
            id: schedule_run.id.clone(),
            task_id: task.id.clone(),
            session_id: Some(format!("session_{}", schedule_run.id)),
            message_id: Some(format!("message_{}", schedule_run.id)),
            trace_id: None,
            attempt,
            phase: TaskPhase::Investigating,
            status: InvestigationRunStatus::Admitted,
            started_at: now.clone(),
            updated_at: now.clone(),
            ended_at: None,
            checkpoint: Some(json!({ "source": "scheduler", "scheduleRunId": schedule_run.id })),
            failure: None,
        })
        .map_err(|error| error.to_string())?;
    state
        .database
        .investigations()
        .update_task_progress(&TaskProgressUpdate {
            id: &task.id,
            status: TaskStatus::Investigating,
            phase: TaskPhase::Investigating,
            active_run_id: Some(&schedule_run.id),
            last_failure: None,
            updated_at: &now,
            completed_at: None,
        })
        .map_err(|error| error.to_string())?;

    let prompt = scheduled_investigation_prompt(&task.objective, schedule.last_outcome.as_deref());
    let mut params = json!({
        "runId": schedule_run.id,
        "sessionId": format!("session_{}", schedule_run.id),
        "messageId": format!("message_{}", schedule_run.id),
        "prompt": prompt,
        "delivery": "async",
        "resume": true,
        "taskId": task.id,
        "taskBudget": {
            "maxSteps": budget.max_steps,
            "maxRunMs": budget.max_run_ms,
            "maxAttempts": budget.max_attempts,
        },
        "permissionMode": task.permission_mode.as_str(),
        "mode": "readonly",
        "providerConfig": provider_config,
        "target": serde_json::to_value(&task.scope).map_err(|error| error.to_string())?,
    });
    if let Some(server_id) = task.server_id.as_deref() {
        params["focusServerId"] = json!(server_id);
    }
    if let Some(workspace_id) = task.workspace_id.as_deref() {
        params["workspaceId"] = json!(workspace_id);
    }
    let response = tokio::select! {
        _ = shutdown.cancelled() => {
            let error = "application shutdown cancelled the scheduled run";
            close_admitted_launch_failure(
                app,
                &state,
                &task.id,
                &schedule_run.id,
                error,
                yukinal_database::models::TaskFailureCode::Internal,
                "shutdown",
            )?;
            return Ok(());
        }
        response = state
            .supervisor
            .request("agent.run.start", params, Duration::from_secs(10)) => response,
    };
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            close_admitted_launch_failure(
                app,
                &state,
                &task.id,
                &schedule_run.id,
                &error.to_string(),
                yukinal_database::models::TaskFailureCode::Transport,
                "transport",
            )?;
            return Ok(());
        }
    };
    let started = response
        .get("started")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| "agent sidecar returned an invalid scheduled run response".to_string())?;
    if !started {
        close_admitted_launch_failure(
            app,
            &state,
            &task.id,
            &schedule_run.id,
            "sidecar admitted but did not start the scheduled run",
            yukinal_database::models::TaskFailureCode::Internal,
            "not_started",
        )?;
        return Ok(());
    }
    let _ = app.emit(
        &tauri_event_name("investigation.schedule_started"),
        json!({ "scheduleRunId": schedule_run.id, "taskId": task.id }),
    );
    Ok(())
}

fn close_admitted_launch_failure(
    app: &AppHandle,
    state: &AppState,
    task_id: &str,
    run_id: &str,
    error: &str,
    failure_code: yukinal_database::models::TaskFailureCode,
    outcome: &str,
) -> Result<(), String> {
    let retryable = matches!(
        failure_code,
        TaskFailureCode::Transport | TaskFailureCode::Timeout
    );
    let schedule_run = state
        .database
        .investigations()
        .fail_schedule_run_launch(
            task_id,
            run_id,
            error,
            failure_code,
            retryable,
            super::failure_options(failure_code, retryable),
            outcome,
            &yukinal_core::sidecar::iso8601_now(),
        )
        .map_err(|failure| failure.to_string())?;
    if let Ok(schedule) = state
        .database
        .investigations()
        .get_schedule(&schedule_run.schedule_id)
    {
        notify_schedule_failure(app, &schedule, run_id);
    }
    Ok(())
}

/// Turn the durable result of the previous scheduled sample into a bounded,
/// host-owned investigation instruction.  The sidecar still decides which
/// read-only tools are useful, but it must first compare the persisted samples
/// and collect a fresh observation before presenting a change as a current
/// fact.  No schedule outcome can grant a write, restart, or delete capability.
fn scheduled_investigation_prompt(objective: &str, last_outcome: Option<&str>) -> String {
    let focus = match last_outcome {
        Some("changed") => {
            "上一轮宿主比较结果为 changed。先用 investigation.evidence.search 找到最近两轮证据摘要，再用 investigation.evidence.compare 比较两条真实证据的有界差异；如果需要对齐同一轮的健康、服务、容器和日志，使用 investigation.evidence.correlate，再用 investigation.evidence.triage 整理有限候选信号。随后在同一目标范围内调用允许的只读工具收集新样本；只在新样本支持时记录 Finding 和决策摘要，并说明仍未证实的原因。"
        }
        Some("baseline") => {
            "上一轮只是建立基线。继续收集同一范围内的只读样本，不要把基线本身当成异常或变更授权。"
        }
        Some("no_change") => {
            "上一轮与前一成功样本没有可观测变化。仍需按相同范围采集一轮新样本；没有新差异时安静结束，不要重复制造 Finding。"
        }
        Some("insufficient_evidence") => {
            "上一轮证据不足。优先补齐同一范围内缺失的只读观测，并明确说明仍无法证明的部分。"
        }
        _ => "这是第一次或上一轮没有可用比较结果。先建立有来源的只读基线，再判断是否存在异常。",
    };
    format!(
        "这是一次已保存的只读巡检。目标：{}。{} 只允许收集与目标范围一致的证据；禁止写入、重启、删除或改变目标状态。旧证据只能作为历史上下文，不能替代当前采样。若发现需要变更的可能，只输出事实、证据、风险和需要用户决定的下一步，不执行任何变更。",
        objective, focus
    )
}

fn effective_schedule_budget(task: &TaskBudget, schedule: &TaskBudget) -> TaskBudget {
    TaskBudget {
        max_steps: task.max_steps.min(schedule.max_steps),
        max_run_ms: task.max_run_ms.min(schedule.max_run_ms),
        max_attempts: task.max_attempts.min(schedule.max_attempts),
    }
}

fn schedule_time_window_block(
    task: &yukinal_database::models::InvestigationTask,
) -> Option<String> {
    investigation::task_time_window_error(task).err()
}

fn is_terminal_task_status(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Stopped | TaskStatus::Expired
    )
}

fn finish_schedule_failure(
    app: &AppHandle,
    state: &AppState,
    run_id: &str,
    outcome: &str,
    error: &str,
) -> Result<(), String> {
    let schedule = state
        .database
        .investigations()
        .get_schedule_for_run(run_id)
        .ok();
    state
        .database
        .investigations()
        .finish_schedule_run(
            run_id,
            InvestigationScheduleRunStatus::Failed,
            Some(outcome),
            Some(error),
            &yukinal_core::sidecar::iso8601_now(),
        )
        .map_err(|failure| failure.to_string())?;

    if let Some(schedule) = schedule {
        let should_notify = matches!(
            schedule.notification_policy,
            yukinal_database::models::InvestigationNotificationPolicy::Always
                | yukinal_database::models::InvestigationNotificationPolicy::OnChange
                | yukinal_database::models::InvestigationNotificationPolicy::FailedRunsOnly
        );
        if should_notify {
            let _ = app.emit(
                &tauri_event_name("investigation.schedule_notification"),
                json!({
                    "scheduleId": schedule.id,
                    "scheduleRunId": run_id,
                    "taskId": schedule.task_id,
                    "outcome": "failed",
                    "notificationPolicy": schedule.notification_policy.as_str(),
                    "title": "持续巡检运行失败",
                    "evidenceIds": [],
                    "at": yukinal_core::sidecar::iso8601_now(),
                }),
            );
        }
    }
    Ok(())
}

fn finish_schedule_skipped(
    app: &AppHandle,
    state: &AppState,
    run_id: &str,
    outcome: &str,
    error: &str,
) -> Result<(), String> {
    let run = state
        .database
        .investigations()
        .finish_schedule_run(
            run_id,
            InvestigationScheduleRunStatus::Skipped,
            Some(outcome),
            Some(error),
            &yukinal_core::sidecar::iso8601_now(),
        )
        .map_err(|failure| failure.to_string())?;
    let _ = app.emit(
        &tauri_event_name("investigation.schedule_skipped"),
        json!({ "runs": [run] }),
    );
    Ok(())
}

fn notify_schedule_failure(app: &AppHandle, schedule: &InvestigationSchedule, run_id: &str) {
    let should_notify = matches!(
        schedule.notification_policy,
        yukinal_database::models::InvestigationNotificationPolicy::Always
            | yukinal_database::models::InvestigationNotificationPolicy::OnChange
            | yukinal_database::models::InvestigationNotificationPolicy::FailedRunsOnly
    );
    if should_notify {
        let _ = app.emit(
            &tauri_event_name("investigation.schedule_notification"),
            json!({
                "scheduleId": schedule.id,
                "scheduleRunId": run_id,
                "taskId": schedule.task_id,
                "outcome": "failed",
                "notificationPolicy": schedule.notification_policy.as_str(),
                "title": "鎸佺画宸℃杩愯澶辫触",
                "evidenceIds": [],
                "at": yukinal_core::sidecar::iso8601_now(),
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{
        effective_schedule_budget, is_terminal_task_status, schedule_time_window_block,
        scheduled_investigation_prompt,
    };
    use yukinal_database::models::{
        Environment, InvestigationPermissionMode, InvestigationRunMode, InvestigationTarget,
        InvestigationTargetHost, InvestigationTask, InvestigationTaskGuardrails,
        TaskAutomationLevel, TaskBudget, TaskPhase, TaskStatus,
    };

    #[test]
    fn schedule_budget_can_only_narrow_the_task_envelope() {
        let task = TaskBudget {
            max_steps: 100,
            max_run_ms: 60_000,
            max_attempts: 5,
        };
        let schedule = TaskBudget {
            max_steps: 12,
            max_run_ms: 10_000,
            max_attempts: 2,
        };
        assert_eq!(effective_schedule_budget(&task, &schedule).max_steps, 12);
        assert_eq!(
            effective_schedule_budget(&task, &schedule).max_run_ms,
            10_000
        );
        assert_eq!(effective_schedule_budget(&task, &schedule).max_attempts, 2);
    }

    #[test]
    fn a_wider_schedule_budget_does_not_expand_the_task_envelope() {
        let task = TaskBudget {
            max_steps: 10,
            max_run_ms: 20_000,
            max_attempts: 2,
        };
        let schedule = TaskBudget {
            max_steps: 100,
            max_run_ms: 90_000,
            max_attempts: 9,
        };
        assert_eq!(
            effective_schedule_budget(&task, &schedule).max_steps,
            task.max_steps
        );
        assert_eq!(
            effective_schedule_budget(&task, &schedule).max_run_ms,
            task.max_run_ms
        );
        assert_eq!(
            effective_schedule_budget(&task, &schedule).max_attempts,
            task.max_attempts
        );
    }

    #[test]
    fn changed_schedule_samples_start_a_bounded_readonly_follow_up() {
        let prompt = scheduled_investigation_prompt("确认 API 延迟", Some("changed"));
        assert!(prompt.contains("上一轮宿主比较结果为 changed"));
        assert!(prompt.contains("investigation.evidence.search"));
        assert!(prompt.contains("investigation.evidence.compare"));
        assert!(prompt.contains("investigation.evidence.correlate"));
        assert!(prompt.contains("investigation.evidence.triage"));
        assert!(prompt.contains("收集新样本"));
        assert!(prompt.contains("禁止写入、重启、删除"));
    }

    #[test]
    fn first_schedule_sample_establishes_a_baseline_without_claiming_anomaly() {
        let prompt = scheduled_investigation_prompt("检查服务", None);
        assert!(prompt.contains("建立有来源的只读基线"));
        assert!(prompt.contains("不能替代当前采样"));
        assert!(!prompt.contains("上一轮宿主比较结果为 changed"));
    }

    #[test]
    fn scheduled_runs_are_blocked_before_provider_admission_when_the_window_is_expired() {
        let task = InvestigationTask {
            id: "task_expired_schedule".into(),
            workspace_id: None,
            server_id: Some("srv_1".into()),
            objective: "检查服务".into(),
            success_criteria: vec!["读取健康状态".into()],
            scope: InvestigationTarget {
                host: InvestigationTargetHost::Remote,
                server_id: Some("srv_1".into()),
                workspace_id: None,
                environment: Environment::Staging,
            },
            guardrails: InvestigationTaskGuardrails {
                not_before_at: None,
                expires_at: Some("1970-01-01T00:00:01Z".into()),
                forbidden_tools: Vec::new(),
                forbidden_path_prefixes: Vec::new(),
            },
            mode: InvestigationRunMode::Readonly,
            permission_mode: InvestigationPermissionMode::Ask,
            automation_level: TaskAutomationLevel::Readonly,
            created_by: "fixture".into(),
            phase: TaskPhase::Investigating,
            status: TaskStatus::Investigating,
            budget: TaskBudget {
                max_steps: 4,
                max_run_ms: 10_000,
                max_attempts: 1,
            },
            created_at: "2026-09-20T00:00:00Z".into(),
            updated_at: "2026-09-20T00:00:00Z".into(),
            completed_at: None,
            active_run_id: None,
            last_failure: None,
        };
        let reason = schedule_time_window_block(&task).expect("expired schedule window");
        assert!(reason.contains("expired"));
    }

    #[test]
    fn terminal_tasks_cannot_keep_a_schedule_alive() {
        assert!(is_terminal_task_status(TaskStatus::Completed));
        assert!(is_terminal_task_status(TaskStatus::Failed));
        assert!(is_terminal_task_status(TaskStatus::Stopped));
        assert!(is_terminal_task_status(TaskStatus::Expired));
        assert!(!is_terminal_task_status(TaskStatus::Pending));
        assert!(!is_terminal_task_status(TaskStatus::Investigating));
        assert!(!is_terminal_task_status(TaskStatus::WaitingUser));
    }
}
