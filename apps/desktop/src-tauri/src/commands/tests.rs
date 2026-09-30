//! `commands/mod.rs` 的单元测试。
//!
//! 从 `commands/mod.rs` 拆出来只为可读性：这里覆盖 IPC 事件名映射与命令层的纯规则。

use super::event_projection::failure_from_event;
use super::{
    failure_options, is_current_investigation_run, is_stable_server_id,
    is_terminal_investigation_run, is_terminal_result_status, next_investigation_status,
    persist_investigation_event_state, safe_audit_summary, sanitize_audit_input,
    sync_investigation_task_status_state, task_failure_code_from_tool_error, tauri_event_name,
};
use crate::state::AppState;
use serde_json::json;
use yukinal_database::models::{
    Environment, ErrorCategory, FailureOptionAction, InvestigationPermissionMode, InvestigationRun,
    InvestigationRunMode, InvestigationRunStatus, InvestigationStep, InvestigationStepKind,
    InvestigationStepStatus, InvestigationTarget, InvestigationTargetHost, InvestigationTask,
    TaskAutomationLevel, TaskBudget, TaskFailureCode, TaskPhase, TaskStatus, ToolExecutionStatus,
};

/// 三个终态放行，三个在途状态一律挡住 —— 挡住的那三个正是「崩溃中断」会留下的形状，
/// 而审计里它们必须表现为「没有这一行」。
#[test]
fn only_terminal_statuses_may_be_persisted_as_a_result() {
    for status in [
        ToolExecutionStatus::Success,
        ToolExecutionStatus::Failed,
        ToolExecutionStatus::Cancelled,
    ] {
        assert!(is_terminal_result_status(&status), "{status:?} is a result");
    }
    for status in [
        ToolExecutionStatus::Pending,
        ToolExecutionStatus::Running,
        ToolExecutionStatus::WaitingApproval,
    ] {
        assert!(
            !is_terminal_result_status(&status),
            "{status:?} cannot describe a finished call",
        );
    }
}

#[test]
fn investigation_event_statuses_follow_the_host_transition_table() {
    assert_eq!(
        next_investigation_status(
            TaskStatus::Investigating,
            "agent.completed",
            Some("completed")
        ),
        Some(TaskStatus::WaitingUser)
    );
    assert_eq!(
        next_investigation_status(TaskStatus::Executing, "agent.completed", Some("completed")),
        Some(TaskStatus::Verifying)
    );
    assert_eq!(
        next_investigation_status(
            TaskStatus::Investigating,
            "agent.completed",
            Some("cancelled")
        ),
        Some(TaskStatus::Stopped)
    );
    assert_eq!(
        next_investigation_status(TaskStatus::Completed, "agent.failed", None),
        None
    );
    assert_eq!(
        next_investigation_status(TaskStatus::Investigating, "agent.tool_result", None),
        None
    );
}

#[test]
fn late_run_frames_cannot_reopen_a_recovered_run() {
    for status in [
        InvestigationRunStatus::Completed,
        InvestigationRunStatus::Failed,
        InvestigationRunStatus::Cancelled,
        InvestigationRunStatus::Interrupted,
    ] {
        assert!(is_terminal_investigation_run(status));
    }
    for status in [
        InvestigationRunStatus::Admitted,
        InvestigationRunStatus::Running,
        InvestigationRunStatus::WaitingUser,
    ] {
        assert!(!is_terminal_investigation_run(status));
    }
    assert!(is_current_investigation_run(Some("run_new"), "run_new"));
    assert!(!is_current_investigation_run(Some("run_new"), "run_old"));
    assert!(!is_current_investigation_run(None, "run_old"));
}

#[test]
fn cross_layer_event_helpers_close_stop_and_reject_late_frames() {
    let directory = std::env::temp_dir().join(format!(
        "yukinal-command-event-fence-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    let state = AppState::bootstrap(&directory).expect("bootstrap command fixture");
    let scope = InvestigationTarget {
        host: InvestigationTargetHost::Local,
        server_id: None,
        workspace_id: None,
        environment: Environment::Local,
    };
    let task = |id: &str, run_id: &str| InvestigationTask {
        id: id.into(),
        workspace_id: None,
        server_id: None,
        objective: "exercise event fence".into(),
        success_criteria: vec!["persist a terminal run".into()],
        scope: scope.clone(),
        guardrails: Default::default(),
        mode: InvestigationRunMode::Readonly,
        permission_mode: InvestigationPermissionMode::Ask,
        automation_level: TaskAutomationLevel::Readonly,
        created_by: "fixture".into(),
        phase: TaskPhase::Investigating,
        status: TaskStatus::Investigating,
        budget: TaskBudget {
            max_steps: 8,
            max_run_ms: 60_000,
            max_attempts: 2,
        },
        created_at: "2026-09-20T00:00:00Z".into(),
        updated_at: "2026-09-20T00:00:00Z".into(),
        completed_at: None,
        active_run_id: Some(run_id.into()),
        last_failure: None,
    };
    let run = |id: &str, task_id: &str, status: InvestigationRunStatus| InvestigationRun {
        id: id.into(),
        task_id: task_id.into(),
        session_id: None,
        message_id: None,
        trace_id: None,
        attempt: 1,
        phase: TaskPhase::Investigating,
        status,
        started_at: "2026-09-20T00:00:00Z".into(),
        updated_at: "2026-09-20T00:00:00Z".into(),
        ended_at: None,
        checkpoint: None,
        failure: None,
    };

    let stopped_task = task("task_stop_fence", "run_stop_fence");
    state
        .database
        .investigations()
        .create_task(&stopped_task)
        .expect("create stopped task");
    state
        .database
        .investigations()
        .create_run(&run(
            "run_stop_fence",
            &stopped_task.id,
            InvestigationRunStatus::Running,
        ))
        .expect("create stopped run");
    let stopped_event = json!({
        "taskId": stopped_task.id,
        "runId": "run_stop_fence",
        "type": "agent.completed",
        "at": "2026-09-20T00:00:01Z",
        "result": { "state": "cancelled" }
    });
    assert!(sync_investigation_task_status_state(
        &state,
        "agent.completed",
        &stopped_event
    ));
    let stopped_run = persist_investigation_event_state(&state, "agent.completed", &stopped_event)
        .expect("persist cancelled run");
    assert_eq!(stopped_run.status, InvestigationRunStatus::Cancelled);
    let stopped = state
        .database
        .investigations()
        .get_task(&stopped_task.id)
        .expect("read stopped task");
    assert_eq!(stopped.status, TaskStatus::Stopped);
    assert!(stopped.active_run_id.is_none());

    let recovered_task = task("task_late_fence", "run_new_fence");
    state
        .database
        .investigations()
        .create_task(&recovered_task)
        .expect("create recovered task");
    state
        .database
        .investigations()
        .create_run(&run(
            "run_old_fence",
            &recovered_task.id,
            InvestigationRunStatus::Interrupted,
        ))
        .expect("create old run");
    state
        .database
        .investigations()
        .create_run(&run(
            "run_new_fence",
            &recovered_task.id,
            InvestigationRunStatus::Running,
        ))
        .expect("create current run");
    let late_event = json!({
        "taskId": recovered_task.id,
        "runId": "run_old_fence",
        "type": "agent.completed",
        "result": { "state": "completed" }
    });
    assert!(!sync_investigation_task_status_state(
        &state,
        "agent.completed",
        &late_event
    ));
    assert!(persist_investigation_event_state(&state, "agent.completed", &late_event).is_none());
    let still_current = state
        .database
        .investigations()
        .get_task(&recovered_task.id)
        .expect("read current task");
    assert_eq!(still_current.status, TaskStatus::Investigating);
    assert_eq!(
        still_current.active_run_id.as_deref(),
        Some("run_new_fence")
    );

    let current_event = json!({
        "taskId": recovered_task.id,
        "runId": "run_new_fence",
        "type": "agent.completed",
        "result": { "state": "completed" }
    });
    assert!(sync_investigation_task_status_state(
        &state,
        "agent.completed",
        &current_event
    ));
    let current_run = persist_investigation_event_state(&state, "agent.completed", &current_event)
        .expect("persist current run");
    assert_eq!(current_run.status, InvestigationRunStatus::Completed);
    let waiting = state
        .database
        .investigations()
        .get_task(&recovered_task.id)
        .expect("read completed task");
    assert_eq!(waiting.status, TaskStatus::WaitingUser);
    assert!(waiting.active_run_id.is_none());

    // A second terminal frame for the same run is ignored by both layers.
    assert!(!sync_investigation_task_status_state(
        &state,
        "agent.failed",
        &current_event
    ));
    assert!(persist_investigation_event_state(&state, "agent.failed", &current_event).is_none());

    drop(state);
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn event_projection_keeps_steps_after_the_first_page_and_preserves_unknown_action_outcomes() {
    let directory = std::env::temp_dir().join(format!(
        "yukinal-command-step-page-boundary-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    let state = AppState::bootstrap(&directory).expect("bootstrap command fixture");
    let task = InvestigationTask {
        id: "task_step_page_boundary".into(),
        workspace_id: None,
        server_id: Some("srv_step_page_boundary".into()),
        objective: "retain the full execution timeline".into(),
        success_criteria: vec!["the newest step remains auditable".into()],
        scope: InvestigationTarget {
            host: InvestigationTargetHost::Remote,
            server_id: Some("srv_step_page_boundary".into()),
            workspace_id: None,
            environment: Environment::Staging,
        },
        guardrails: Default::default(),
        mode: InvestigationRunMode::Goal,
        permission_mode: InvestigationPermissionMode::Ask,
        automation_level: TaskAutomationLevel::Execute,
        created_by: "fixture".into(),
        phase: TaskPhase::Execution,
        status: TaskStatus::Executing,
        budget: TaskBudget {
            max_steps: 10_000,
            max_run_ms: 60_000,
            max_attempts: 1,
        },
        created_at: "2026-09-30T00:00:00Z".into(),
        updated_at: "2026-09-30T00:00:00Z".into(),
        completed_at: None,
        active_run_id: Some("run_step_page_boundary".into()),
        last_failure: None,
    };
    state
        .database
        .investigations()
        .create_task(&task)
        .expect("create long-running task");
    state
        .database
        .investigations()
        .create_run(&InvestigationRun {
            id: "run_step_page_boundary".into(),
            task_id: task.id.clone(),
            session_id: None,
            message_id: None,
            trace_id: Some("trace_step_page_boundary".into()),
            attempt: 1,
            phase: TaskPhase::Execution,
            status: InvestigationRunStatus::Running,
            started_at: "2026-09-30T00:00:00Z".into(),
            updated_at: "2026-09-30T00:00:00Z".into(),
            ended_at: None,
            checkpoint: None,
            failure: None,
        })
        .expect("create active run");

    for ordinal in 0..513_u32 {
        state
            .database
            .investigations()
            .upsert_step(&InvestigationStep {
                id: format!("step_history_{ordinal:03}"),
                task_id: task.id.clone(),
                run_id: "run_step_page_boundary".into(),
                ordinal,
                kind: InvestigationStepKind::Evidence,
                title: format!("historical step {ordinal}"),
                status: InvestigationStepStatus::Succeeded,
                attempt: 1,
                tool_name: Some("server.info".into()),
                plan_id: None,
                plan_step_id: None,
                target: None,
                input_summary: None,
                output_summary: Some("historical result".into()),
                evidence_ids: vec![],
                started_at: Some("2026-09-30T00:00:00Z".into()),
                ended_at: Some("2026-09-30T00:00:01Z".into()),
                failure: None,
            })
            .expect("seed timeline history");
    }

    let tool_call = json!({
        "taskId": task.id,
        "runId": "run_step_page_boundary",
        "stepId": "step_after_page_boundary",
        "toolName": "server.exec",
        "riskLevel": "high",
        "target": { "host": "remote", "serverId": "srv_step_page_boundary", "environment": "staging" },
        "input": { "command": "change a remote setting", "password": "fixture-secret" },
        "at": "2026-09-30T00:00:02Z"
    });
    persist_investigation_event_state(&state, "agent.tool_call", &tool_call)
        .expect("persist tool call beyond the first page");

    let tool_result = json!({
        "taskId": task.id,
        "runId": "run_step_page_boundary",
        "stepId": "step_after_page_boundary",
        "toolName": "server.exec",
        "status": "failed",
        "errorCode": "transport",
        "executionState": "result_unknown",
        "error": "SSH command result is unavailable; its effect may have occurred",
        "outputSummary": "result_unknown; exit status unavailable",
        "target": { "host": "remote", "serverId": "srv_step_page_boundary", "environment": "staging" },
        "endedAt": "2026-09-30T00:00:03Z"
    });
    persist_investigation_event_state(&state, "agent.tool_result", &tool_result)
        .expect("persist tool result beyond the first page");

    let step = state
        .database
        .investigations()
        .get_step_for_run(
            &task.id,
            "run_step_page_boundary",
            "step_after_page_boundary",
        )
        .expect("read the projected step by full identity");
    assert_eq!(step.ordinal, 513, "the new step must follow all history");
    assert_eq!(step.status, InvestigationStepStatus::Failed);
    assert_eq!(
        step.output_summary.as_deref(),
        Some("result_unknown; exit status unavailable")
    );
    let failure = step.failure.as_ref().expect("persisted step failure");
    assert_eq!(failure.code, TaskFailureCode::OutcomeUnknown);
    assert!(!failure.retryable);
    assert_eq!(
        failure.detail.as_ref().unwrap()["executionState"],
        "result_unknown"
    );

    let failed_run = json!({
        "taskId": task.id,
        "runId": "run_step_page_boundary",
        "type": "agent.failed",
        "error": "Agent run stopped after the remote result was lost",
        "at": "2026-09-30T00:00:04Z"
    });
    let run = persist_investigation_event_state(&state, "agent.failed", &failed_run)
        .expect("persist terminal run");
    assert_eq!(
        run.failure.as_ref().map(|failure| failure.code),
        Some(TaskFailureCode::OutcomeUnknown),
        "the run must retain the exact action failure instead of a generic terminal error"
    );
    let failed_task = state
        .database
        .investigations()
        .get_task(&task.id)
        .expect("read finalized task");
    assert_eq!(
        failed_task
            .last_failure
            .as_ref()
            .map(|failure| failure.code),
        Some(TaskFailureCode::OutcomeUnknown)
    );

    drop(state);
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn tool_failure_codes_keep_recovery_categories_structured() {
    assert_eq!(
        task_failure_code_from_tool_error("transport"),
        Some(TaskFailureCode::Transport)
    );
    assert_eq!(
        task_failure_code_from_tool_error("approval_timeout"),
        Some(TaskFailureCode::ApprovalRequired)
    );
    assert_eq!(
        task_failure_code_from_tool_error("execution_failed"),
        Some(TaskFailureCode::CommandFailed)
    );
    assert_eq!(task_failure_code_from_tool_error("unknown_code"), None);
}

#[test]
fn server_exec_interruption_states_persist_as_non_retryable_unknown_outcomes() {
    for state in ["result_unknown", "timed_out", "cancelled"] {
        let failure = failure_from_event(
            &json!({
                "toolName": "server.exec",
                "errorCode": "transport",
                "executionState": state,
                "error": "SSH command result is unavailable",
                "at": "2026-09-30T00:00:00Z"
            }),
            2,
        )
        .expect("tool failure event has a summary");
        assert_eq!(failure.code, TaskFailureCode::OutcomeUnknown);
        assert!(!failure.retryable);
        assert!(failure.message.contains("不要直接重试"));
        assert_eq!(failure.detail.as_ref().unwrap()["executionState"], state);
        assert!(failure
            .options
            .as_ref()
            .unwrap()
            .iter()
            .any(|option| { option.action == FailureOptionAction::WaitUser }));
        assert!(!failure
            .options
            .as_ref()
            .unwrap()
            .iter()
            .any(|option| { option.action == FailureOptionAction::Retry }));
    }

    // The state discriminator only has special meaning on the host-owned command tool.
    let unrelated = failure_from_event(
        &json!({
            "toolName": "mcp.inspect",
            "errorCode": "transport",
            "executionState": "result_unknown",
            "error": "MCP unavailable"
        }),
        1,
    )
    .expect("MCP failure event has a summary");
    assert_eq!(unrelated.code, TaskFailureCode::Transport);
    assert!(unrelated.retryable);
}

#[test]
fn recovery_options_derive_retryability_from_the_failure_code() {
    let transport = failure_options(TaskFailureCode::Transport);
    assert!(transport
        .iter()
        .any(|option| option.action == FailureOptionAction::Retry));

    // Cancellation and unknown failures remain visible/recoverable through
    // explicit user choices, but must not offer a generic automatic retry.
    for code in [
        TaskFailureCode::Cancelled,
        TaskFailureCode::OutcomeUnknown,
        TaskFailureCode::Unknown,
    ] {
        assert!(
            !failure_options(code)
                .iter()
                .any(|option| option.action == FailureOptionAction::Retry),
            "{code:?} must not have a retry affordance"
        );
    }
}

/// The Rust tool-error mapping is part of the shared taxonomy: every wire code in
/// the fixture must map to a task failure whose category matches, so the UI never
/// has to guess a category for a tool error the host already understood.
#[test]
fn the_shipped_tool_error_mapping_matches_the_shared_taxonomy() {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Taxonomy {
        tool_errors: Vec<Entry>,
    }
    #[derive(serde::Deserialize)]
    struct Entry {
        code: String,
        category: ErrorCategory,
    }
    let taxonomy: Taxonomy = serde_json::from_str(include_str!(
        "../../../../../packages/shared/fixtures/error-taxonomy.json"
    ))
    .expect("the shared taxonomy fixture must parse");
    assert!(!taxonomy.tool_errors.is_empty());
    for entry in taxonomy.tool_errors {
        let code = task_failure_code_from_tool_error(&entry.code)
            .unwrap_or_else(|| panic!("tool error {} has no Rust mapping", entry.code));
        assert_eq!(
            code.category(),
            entry.category,
            "tool error {} maps to the wrong category",
            entry.code
        );
    }
}

#[test]
fn stable_server_ids_are_lowercase_and_scoped() {
    assert!(is_stable_server_id("srv_01abc"));
    assert!(!is_stable_server_id("server_01abc"));
    assert!(!is_stable_server_id("srv_ABC"));
    // 这条断言是这次修复补上的：审计管道曾经允许下划线，于是它接受的服务器目标
    // 比契约和 `tool_execution_list` 都宽。规则本身在 `yukinal_core::ids`，
    // 这里保留一行是为了让「审计接受的目标必须与契约一致」继续被钉住。
    assert!(!is_stable_server_id("srv_01_abc"));
}

#[test]
fn audit_input_redacts_secret_keys_and_bounds_strings() {
    let input = sanitize_audit_input(json!({
        "command": "echo hello",
        "apiKey": "do-not-persist",
        "content": "file body must not be persisted",
        "nested": { "password": "also-do-not-persist" },
    }));
    assert_eq!(input["apiKey"], "[redacted]");
    assert_eq!(input["content"], "[redacted]");
    assert_eq!(input["nested"]["password"], "[redacted]");
    assert_eq!(input["command"], "echo hello");
}

/// `filesystem.edit` 的两个字符串参数同样是任意文件内容，所以和 `content` 一样处理。
///
/// 这条测试存在的理由很具体：这三个键分属三个工具，而漏掉其中一个不会有任何报错 ——
/// 审计里只是安静地多出一份 `.env` 的片段。名字也按 `sanitize_audit_input` 的归一化
/// 规则写（`old_string` / `newString` 都算命中）。
#[test]
fn audit_input_treats_edit_content_like_write_content() {
    let input = sanitize_audit_input(json!({
        "path": "/srv/app/.env",
        "expectedRevision": "0000000000000000000000000000000000000000000000000000000000000000",
        "oldString": "API_KEY=old-do-not-persist",
        "newString": "API_KEY=new-do-not-persist",
    }));
    assert_eq!(input["oldString"], "[redacted]");
    assert_eq!(input["newString"], "[redacted]");
    // 路径与摘要不是内容，留着才有排障价值。
    assert_eq!(input["path"], "/srv/app/.env");
}

#[test]
fn audit_summary_omits_sensitive_output_and_truncates_other_output() {
    assert_eq!(
        safe_audit_summary("token=do-not-persist", 4000),
        "[sensitive output omitted]"
    );
    assert_eq!(safe_audit_summary("abcdef", 3), "abc\n…[truncated]");
}

#[test]
fn tauri_event_channels_use_only_supported_characters() {
    let channel = tauri_event_name("agent.started");
    assert_eq!(channel, "agent:started");
    assert!(channel
        .chars()
        .all(|character| character.is_ascii_alphanumeric()
            || matches!(character, '-' | '/' | ':' | '_')));
}
