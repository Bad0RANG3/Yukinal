#![allow(dead_code, unused_imports)]
//! `persistence.rs` 各测试文件共享的 fixture。
//!
//! 集成测试每个文件是一个独立 crate，所以共享 fixture 只能放在 `common/` 子目录里由各
//! 文件 `mod common;` 引入；`#![allow(dead_code, unused_imports)]` 是必要的 —— 不是每个
//! 测试 crate 都会用到这里全部的类型与 fixture。

//! Acceptance: restart-persistence for servers/workspaces/provider_configs, write+read
//! for tool_executions, and the camelCase wire shape of the row structs.

pub use std::path::PathBuf;

pub use rusqlite::Connection;
pub use serde_json::json;
pub use sha2::{Digest, Sha256};
pub use yukinal_database::models::{
    Activity, ActivityOutcome, ActivitySource, ActivityType, AiProviderConfig, AiProviderKind,
    ChatMessage, ChatMessageRole, ChatSession, ChatSessionCounts, DecisionBrief,
    DecisionBriefStatus, DecisionOption, DecisionOptionStatus, Environment, Evidence,
    EvidenceContentType, EvidenceKind, EvidenceRedactionStatus, Finding, FindingConfidence,
    FindingKind, HostCertificateAuthority, Identity, InfrastructureProviderConfig,
    InvestigationArtifact, InvestigationFailure, InvestigationNotificationPolicy,
    InvestigationPermissionMode, InvestigationPlan, InvestigationPlanApproval,
    InvestigationPlanStep, InvestigationRun, InvestigationRunMode, InvestigationRunStatus,
    InvestigationSchedule, InvestigationScheduleComparisonStatus, InvestigationScheduleRunStatus,
    InvestigationScheduleStatus, InvestigationStep, InvestigationStepKind, InvestigationStepStatus,
    InvestigationTarget, InvestigationTargetHost, InvestigationTask, McpHttpAuthHeaderConfig,
    McpServerConfig, PermissionMode, PlanApprovalSource, PlanApprovalStatus, PlanStepKind,
    PlanStepStatus, RiskLevel, Server, ServerCapabilities, ServerConnection, ServerMetadata,
    ServerSnapshot, ServerStatus, TaskArtifactKind, TaskArtifactStatus, TaskAutomationLevel,
    TaskBudget, TaskFailureCode, TaskPhase, TaskStatus, ToolExecutionRecord, ToolExecutionStatus,
    Workspace, WorkspaceRepository,
};
pub use yukinal_database::{
    repositories::{
        EvidenceSearchQuery, HostToolCallClaim, HostToolCallInput, HostToolCallStatus,
        InvestigationRetentionKind, InvestigationRetentionRequestItem, TaskProgressUpdate,
    },
    Database, DatabaseError,
};

pub fn sample_server(id: &str) -> Server {
    Server {
        id: id.to_string(),
        name: "Production API".into(),
        connection: ServerConnection {
            host: "api.example.com".into(),
            port: 22,
            username: "deploy".into(),
            identity_id: None,
            host_certificate_authority: None,
        },
        group_id: None,
        capabilities: ServerCapabilities {
            linux: Some(true),
            docker: Some(true),
            ..Default::default()
        },
        status: ServerStatus::Connected,
        metadata: ServerMetadata {
            environment: Environment::Production,
            region: Some("Singapore".into()),
            hostname: None,
            os: None,
            tags: Some(vec!["api".into()]),
            workspace_ids: None,
        },
        created_at: "2026-01-01T00:00:00.000Z".into(),
        updated_at: "2026-01-01T00:00:00.000Z".into(),
    }
}

pub fn sample_snapshot(id: &str, server_id: &str, collected_at: &str) -> ServerSnapshot {
    ServerSnapshot {
        id: id.into(),
        server_id: server_id.into(),
        collected_at: collected_at.into(),
        health: yukinal_database::models::HealthState::Healthy,
        os: Some(
            json!({ "distribution": "Ubuntu", "version": "24.04", "hostname": "api-01", "kernel": "6.8.0", "arch": "x86_64" }),
        ),
        cpu: Some(
            json!({ "model": "Xeon", "cores": 8, "usagePercent": 23.5, "loadAverage": [1.2, 0.9, 0.7] }),
        ),
        memory: None,
        disks: None,
        uptime_seconds: Some(172_800),
        network: None,
        docker: None,
        capabilities: ServerCapabilities {
            linux: Some(true),
            docker: Some(true),
            ..Default::default()
        },
        collectors: None,
    }
}

pub fn sample_task(id: &str) -> InvestigationTask {
    InvestigationTask {
        id: id.into(),
        workspace_id: None,
        server_id: Some("srv_demo".into()),
        objective: "Find the cause of elevated API latency".into(),
        success_criteria: vec!["Produce one evidence-backed cause".into()],
        scope: InvestigationTarget {
            host: InvestigationTargetHost::Remote,
            server_id: Some("srv_demo".into()),
            workspace_id: None,
            environment: Environment::Staging,
        },
        guardrails: Default::default(),
        mode: InvestigationRunMode::Readonly,
        permission_mode: InvestigationPermissionMode::Ask,
        automation_level: TaskAutomationLevel::Readonly,
        created_by: "user".into(),
        phase: TaskPhase::Investigating,
        status: TaskStatus::Pending,
        budget: TaskBudget {
            max_steps: 25,
            max_run_ms: 600_000,
            max_attempts: 3,
        },
        created_at: "2026-01-01T00:00:00.000Z".into(),
        updated_at: "2026-01-01T00:00:00.000Z".into(),
        completed_at: None,
        active_run_id: None,
        last_failure: None,
    }
}

pub fn content_hash(content: &serde_json::Value) -> String {
    format!("{:x}", Sha256::digest(serde_json::to_vec(content).unwrap()))
}

pub fn temp_db(tag: &str) -> (PathBuf, Database) {
    let path = std::env::temp_dir().join(format!("yukinal-db-{tag}-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    let db = Database::open(&path).expect("open temp database");
    (path, db)
}

pub fn cleanup(path: &PathBuf) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}
