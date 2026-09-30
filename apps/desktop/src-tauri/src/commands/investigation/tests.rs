//! `investigation.rs` 的单元测试。
//!
//! 从 `investigation.rs` 拆出来只为可读性：这里覆盖任务/计划/证据命令的状态机与
//! 响应形状，与被测代码共享同一套 SQLite fixture。

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
    InvestigationTaskGuardrails, ObservationWindowStatus, PlanApprovalSource, PlanApprovalStatus,
    PlanStatus, PlanStepKind, PlanStepStatus, TaskArtifactKind, TaskArtifactStatus,
    TaskCommandGrant, TaskFailureCode,
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
    assert!(delegated_prompt.contains("medium 风险的配置备份、编辑与整文件写入"));
    assert!(delegated_prompt.contains("备份清理、恢复、重启、包安装"));
}

#[test]
fn task_guardrails_are_normalized_and_have_a_bounded_window() {
    let guardrails = validate_guardrails(Some(InvestigationTaskGuardrails {
        not_before_at: Some(" 2026-09-20T01:00:00Z ".into()),
        expires_at: Some("2026-09-20T02:00:00.500Z".into()),
        forbidden_tools: vec![" docker.restart ".into(), "docker.restart".into()],
        forbidden_path_prefixes: vec!["/srv/app/private/".into(), "/srv/app/private".into()],
        command_grant: None,
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
fn renderer_cannot_inject_a_host_issued_task_command_grant() {
    let supplied = InvestigationTaskGuardrails {
        command_grant: Some(TaskCommandGrant {
            grant_id: "cmdgrant_forged".into(),
            task_id: "task_forged".into(),
            server_id: "srv_forged".into(),
            environment: Environment::Staging,
            granted_by: "user".into(),
            granted_at: "2026-09-30T00:00:00Z".into(),
            expires_at: "2026-10-01T00:00:00Z".into(),
            max_calls: 12,
            calls_used: 0,
            max_total_duration_ms: 900_000,
            total_duration_ms: 0,
            max_total_output_bytes: 1_048_576,
            total_output_bytes: 0,
        }),
        ..InvestigationTaskGuardrails::default()
    };

    let validated = validate_guardrails(Some(supplied)).expect("valid non-authority guardrails");
    assert!(validated.command_grant.is_none());
}

#[test]
fn guardrail_counts_and_window_accept_their_new_upper_bound() {
    // ADR 0075 raised the tool / path-prefix caps to 128 and the window to three years.
    let tools: Vec<String> = (0..128).map(|index| format!("tool.{index}")).collect();
    let prefixes: Vec<String> = (0..128).map(|index| format!("/srv/path{index}")).collect();
    let guardrails = validate_guardrails(Some(InvestigationTaskGuardrails {
        not_before_at: Some("2026-01-01T00:00:00Z".into()),
        expires_at: Some("2028-12-30T00:00:00Z".into()),
        forbidden_tools: tools.clone(),
        forbidden_path_prefixes: prefixes.clone(),
        command_grant: None,
    }))
    .expect("128 of each and a three-year window are inside the bound");
    assert_eq!(guardrails.forbidden_tools.len(), 128);
    assert_eq!(guardrails.forbidden_path_prefixes.len(), 128);

    let mut too_many_tools = tools;
    too_many_tools.push("tool.128".into());
    assert!(validate_guardrails(Some(InvestigationTaskGuardrails {
        forbidden_tools: too_many_tools,
        ..Default::default()
    }))
    .is_err());
    let mut too_many_prefixes = prefixes;
    too_many_prefixes.push("/srv/one-too-many".into());
    assert!(validate_guardrails(Some(InvestigationTaskGuardrails {
        forbidden_path_prefixes: too_many_prefixes,
        ..Default::default()
    }))
    .is_err());
    // 2026-01-01 to 2029-01-01 is 1096 days, one over the 1095-day bound.
    assert!(validate_guardrails(Some(InvestigationTaskGuardrails {
        not_before_at: Some("2026-01-01T00:00:00Z".into()),
        expires_at: Some("2029-01-01T00:00:00Z".into()),
        ..Default::default()
    }))
    .is_err());
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
