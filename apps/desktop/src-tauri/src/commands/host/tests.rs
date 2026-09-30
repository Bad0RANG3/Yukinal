//! `host.rs` 的单元测试。
//!
//! 从 `host.rs` 拆出来只为可读性：测试覆盖的是纯决策（指纹、guardrail、plan binding、
//! 证据比较）与真实 SQLite fixture 上的证据/工具落库路径，与被测代码放在同一个模块树里。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::keys::{ssh_key, HashAlg};
use russh::server::{Auth, RunningServerHandle, Server as _};
use russh::{Channel, ChannelId};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use yukinal_filesystem::Error as FilesystemError;
use yukinal_ssh::{
    Authentication, ConnectionSecrets, KnownHostsPolicy, OutboundProxy, SshBackend, SshConfig,
};

use super::{
    backup_owner_matches_request, backup_record_id, baseline_artifact_matches_plan,
    cancel_sidecar_request, collect_json_diff, compare_text_content,
    effectful_tool_requires_durable_plan, evidence_freshness_at, filesystem_failure,
    finish_host_tool_call, handle_evidence_correlation, handle_sidecar_request_with_cancel,
    host_tool_call_status, input_bindings_match, is_effectful_host_tool, merge_plan_runtime,
    normalize_new_plan_runtime, observation_configuration_matches,
    plan_allows_repeated_observation, plan_binding_matches, plan_definition,
    prepare_host_tool_call, reset_observation_window, server_exec_input_fingerprint, success,
    task_allows_auto_medium_action, task_guardrail_violation, validate_observation_window,
    validate_playbook_step, BackupCleanupRequest, FilesystemBackupCleanupInput,
    FilesystemBackupCleanupItemInput, HostCancellationRegistry, HostToolCallDecision,
    HostToolExecuteRequest, HostToolTarget, DOCKER_LOGS, FILESYSTEM_BACKUP,
    FILESYSTEM_BACKUP_CLEANUP, FILESYSTEM_EDIT, FILESYSTEM_RESTORE, FILESYSTEM_WRITE,
    HOST_TOOL_EXECUTE, SERVER_INFO,
};
use super::{cross_task_cleanup_is_plan_bound, filesystem_backup_retention, retention_candidates};
use crate::state::AppState;
use yukinal_database::models::{
    Environment, Evidence, EvidenceContentType, EvidenceKind, EvidenceRedactionStatus,
    InvestigationArtifact, InvestigationObservationWindow, InvestigationPermissionMode,
    InvestigationPlan, InvestigationPlanApproval, InvestigationPlanStep, InvestigationRun,
    InvestigationRunMode, InvestigationRunStatus, InvestigationTarget, InvestigationTargetHost,
    InvestigationTask, InvestigationTaskGuardrails, ObservationWindowStatus, PlanApprovalSource,
    PlanApprovalStatus, PlanIdempotency, PlanStatus, PlanStepKind, PlanStepStatus, RiskLevel,
    Server, ServerCapabilities, ServerConnection, ServerMetadata, ServerStatus, TaskArtifactKind,
    TaskArtifactStatus, TaskAutomationLevel, TaskBudget, TaskCommandGrant, TaskPhase, TaskStatus,
};
use yukinal_database::repositories::{
    FilesystemBackupRecord, FilesystemBackupStatus, HostToolCallClaim, HostToolCallStatus,
};

const E2E_SSH_USER: &str = "yukinal-host-e2e";
const E2E_SSH_PASSWORD: &str = "loopback-only";

#[derive(Clone, Default)]
struct ExecServerObservations {
    commands: Arc<Mutex<Vec<String>>>,
}

struct LoopbackExecServer {
    address: SocketAddr,
    fingerprint: String,
    observed: ExecServerObservations,
    shutdown: RunningServerHandle,
}

impl Drop for LoopbackExecServer {
    fn drop(&mut self) {
        self.shutdown
            .shutdown("desktop host server.exec test complete".into());
    }
}

#[derive(Clone)]
struct LoopbackExecServerHandler {
    observed: ExecServerObservations,
}

struct LoopbackExecChannelHandler {
    observed: ExecServerObservations,
}

impl russh::server::Server for LoopbackExecServerHandler {
    type Handler = LoopbackExecChannelHandler;

    fn new_client(&mut self, _peer_addr: Option<SocketAddr>) -> Self::Handler {
        LoopbackExecChannelHandler {
            observed: self.observed.clone(),
        }
    }
}

impl russh::server::Handler for LoopbackExecChannelHandler {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if user == E2E_SSH_USER && password == E2E_SSH_PASSWORD {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<russh::server::Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        let command = String::from_utf8_lossy(data).into_owned();
        self.observed
            .commands
            .lock()
            .expect("loopback command observations")
            .push(command.clone());
        session.channel_success(channel)?;
        let handle = session.handle();
        tokio::spawn(async move {
            if command.contains("drop-after-request") {
                // The request reached the server, but there is no exit status to
                // tell the host whether its side effect completed.
                let _ = handle.close(channel).await;
                return;
            }
            let stdout = if command.contains("server-exec-e2e") {
                b"verified:server-exec-e2e\n".to_vec()
            } else {
                b"loopback-command-received\n".to_vec()
            };
            if handle.data(channel, stdout).await.is_err() {
                return;
            }
            let _ = handle.exit_status_request(channel, 0).await;
            let _ = handle.eof(channel).await;
            let _ = handle.close(channel).await;
        });
        Ok(())
    }
}

async fn start_loopback_exec_server() -> LoopbackExecServer {
    let host_key = ssh_key::PrivateKey::random(&mut rand::rng(), ssh_key::Algorithm::Ed25519)
        .expect("generate ephemeral loopback host key");
    let fingerprint = host_key
        .public_key()
        .fingerprint(HashAlg::Sha256)
        .to_string();
    let config = Arc::new(russh::server::Config {
        keys: vec![host_key],
        auth_rejection_time: Duration::from_millis(10),
        inactivity_timeout: None,
        ..Default::default()
    });
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback SSH server");
    let address = listener.local_addr().expect("loopback SSH address");
    let observed = ExecServerObservations::default();
    let server_observed = observed.clone();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let mut server = LoopbackExecServerHandler {
            observed: server_observed,
        };
        let running = server.run_on_socket(config, &listener);
        let _ = ready_tx.send(running.handle());
        let _ = running.await;
    });
    let shutdown = ready_rx.await.expect("loopback SSH server startup");
    LoopbackExecServer {
        address,
        fingerprint,
        observed,
        shutdown,
    }
}

fn task_for_server_exec(
    task_id: &str,
    server_id: &str,
    run_id: &str,
    grant: Option<TaskCommandGrant>,
) -> InvestigationTask {
    let mut task = lifecycle_task();
    task.id = task_id.into();
    task.server_id = Some(server_id.into());
    task.scope = InvestigationTarget {
        host: InvestigationTargetHost::Remote,
        server_id: Some(server_id.into()),
        workspace_id: None,
        environment: Environment::Staging,
    };
    task.created_by = "user".into();
    task.active_run_id = Some(run_id.into());
    task.guardrails = InvestigationTaskGuardrails {
        command_grant: grant,
        ..Default::default()
    };
    task
}

fn seed_server_exec_plan(state: &AppState, task: &InvestigationTask, plan_id: &str, step_id: &str) {
    state
        .database
        .investigations()
        .create_task(task)
        .expect("create durable server.exec task");
    let run_id = task.active_run_id.as_deref().expect("active run id");
    state
        .database
        .investigations()
        .create_run(&InvestigationRun {
            id: run_id.into(),
            task_id: task.id.clone(),
            session_id: None,
            message_id: Some(format!("message_{run_id}")),
            trace_id: Some(format!("trace_{run_id}")),
            attempt: 1,
            phase: TaskPhase::Execution,
            status: InvestigationRunStatus::Running,
            started_at: "2026-09-30T00:00:00Z".into(),
            updated_at: "2026-09-30T00:00:00Z".into(),
            ended_at: None,
            checkpoint: None,
            failure: None,
        })
        .expect("create durable server.exec run");
    state
        .database
        .investigations()
        .save_plan(&InvestigationPlan {
            id: plan_id.into(),
            task_id: task.id.clone(),
            revision: 1,
            status: PlanStatus::Active,
            created_at: "2026-09-30T00:00:00Z".into(),
            updated_at: "2026-09-30T00:00:00Z".into(),
            current_step_id: Some(step_id.into()),
            approval: Some(InvestigationPlanApproval {
                status: PlanApprovalStatus::Approved,
                source: Some(PlanApprovalSource::User),
                option_id: None,
                approved_at: Some("2026-09-30T00:00:00Z".into()),
                note: Some("approved for this local integration fixture".into()),
            }),
            observation_window: None,
            steps: vec![InvestigationPlanStep {
                id: step_id.into(),
                ordinal: 0,
                kind: PlanStepKind::Action,
                title: "Run a bounded server command".into(),
                purpose: "Prove the host SSH command path".into(),
                allowed_tools: vec!["server.exec".into()],
                input_bindings: None,
                idempotency: Some(PlanIdempotency::Unsafe),
                risk_level: Some(RiskLevel::High),
                requires_baseline: Some(false),
                preconditions: None,
                verification_criteria: Some(vec!["the fixture result is returned".into()]),
                preview: Some("run exactly one fixture command".into()),
                rollback: None,
                target: None,
                evidence_ids: vec![],
                success_criteria: vec!["the command result is bounded and recorded".into()],
                requires_approval: true,
                max_attempts: 1,
                attempts: 0,
                status: PlanStepStatus::Running,
                started_at: Some("2026-09-30T00:00:00Z".into()),
                ended_at: None,
                last_deviation: None,
            }],
        })
        .expect("save approved action plan");
}

fn server_exec_rpc_request(
    task_id: Option<&str>,
    run_id: &str,
    trace_id: &str,
    call_id: &str,
    plan_id: Option<&str>,
    plan_step_id: Option<&str>,
    command: &str,
) -> serde_json::Value {
    let mut request = json!({
        "callId": call_id,
        "runId": run_id,
        "traceId": trace_id,
        "toolName": "server.exec",
        "input": {
            "command": command,
            "purpose": "verify the desktop host executes a bounded command once",
            "timeoutMs": 5_000,
            "maxOutputBytes": 4_096
        },
        "target": {
            "host": "remote",
            "serverId": "srv_host_e2e",
            "environment": "staging"
        },
        "evidenceIds": []
    });
    if let Some(task_id) = task_id {
        request["taskId"] = json!(task_id);
    }
    if let Some(plan_id) = plan_id {
        request["planId"] = json!(plan_id);
    }
    if let Some(plan_step_id) = plan_step_id {
        request["planStepId"] = json!(plan_step_id);
    }
    request
}

#[tokio::test]
async fn desktop_host_rpc_executes_server_command_once_and_persists_unknown_outcome() {
    let directory = tempfile::tempdir().expect("isolated host data directory");
    let state = AppState::bootstrap(directory.path()).expect("bootstrap host state");
    let ssh_server = start_loopback_exec_server().await;
    let server_id = "srv_host_e2e";
    let now = yukinal_time::now_epoch_seconds();
    let expires_at = yukinal_time::iso8601_utc(now + 3_600);
    state
        .database
        .servers()
        .insert(&Server {
            id: server_id.into(),
            name: "loopback server.exec fixture".into(),
            connection: ServerConnection {
                host: "127.0.0.1".into(),
                port: ssh_server.address.port(),
                username: E2E_SSH_USER.into(),
                identity_id: None,
                host_certificate_authority: None,
            },
            group_id: None,
            capabilities: ServerCapabilities::default(),
            status: ServerStatus::Disconnected,
            metadata: ServerMetadata {
                environment: Environment::Staging,
                region: None,
                hostname: Some("localhost".into()),
                os: Some("test fixture".into()),
                tags: None,
                workspace_ids: None,
            },
            created_at: "2026-09-30T00:00:00Z".into(),
            updated_at: "2026-09-30T00:00:00Z".into(),
        })
        .expect("register loopback SSH server");
    state
        .ssh
        .trust_host(
            "127.0.0.1",
            ssh_server.address.port(),
            &ssh_server.fingerprint,
        )
        .expect("pin ephemeral SSH server key");
    let session = state
        .ssh
        .connect(
            SshConfig {
                server_id: server_id.into(),
                host: "127.0.0.1".into(),
                port: ssh_server.address.port(),
                username: E2E_SSH_USER.into(),
                authentication: Authentication::Password {
                    credential_ref: "keychain://test-only".into(),
                },
                host_certificate_authority: None,
                known_hosts_policy: KnownHostsPolicy::RequireMatch,
                outbound_proxy: OutboundProxy::default(),
                keepalive_interval_secs: 0,
            },
            ConnectionSecrets {
                password: Some(E2E_SSH_PASSWORD.into()),
                ..ConnectionSecrets::empty()
            },
        )
        .await
        .expect("authenticate over the real SSH protocol");
    state.terminals.cache_session(server_id, session);

    let denied_task = task_for_server_exec(
        "task_host_e2e_denied",
        server_id,
        "run_host_e2e_denied",
        None,
    );
    seed_server_exec_plan(
        &state,
        &denied_task,
        "plan_host_e2e_denied",
        "step_host_e2e_denied",
    );
    let denied = handle_sidecar_request_with_cancel(
        &state,
        HOST_TOOL_EXECUTE,
        server_exec_rpc_request(
            Some(&denied_task.id),
            "run_host_e2e_denied",
            "trace_host_e2e_denied",
            "call_host_e2e_denied",
            Some("plan_host_e2e_denied"),
            Some("step_host_e2e_denied"),
            "printf 'must-not-run\\n'",
        ),
        CancellationToken::new(),
    )
    .await
    .expect("denied command returns structured host response");
    assert_eq!(denied["status"], "failed");
    assert_eq!(denied["error"]["code"], "denied_by_policy");
    assert!(
        ssh_server
            .observed
            .commands
            .lock()
            .expect("command observations")
            .is_empty(),
        "missing user command delegation must stop before SSH"
    );

    let successful_grant = TaskCommandGrant {
        grant_id: "cmdgrant_host_e2e_success".into(),
        task_id: "task_host_e2e_success".into(),
        server_id: server_id.into(),
        environment: Environment::Staging,
        granted_by: "user".into(),
        granted_at: yukinal_time::iso8601_utc(now),
        expires_at: expires_at.clone(),
        max_calls: 2,
        calls_used: 0,
        max_total_duration_ms: 20_000,
        total_duration_ms: 0,
        max_total_output_bytes: 8_192,
        total_output_bytes: 0,
    };
    let successful_task = task_for_server_exec(
        "task_host_e2e_success",
        server_id,
        "run_host_e2e_success",
        Some(successful_grant),
    );
    seed_server_exec_plan(
        &state,
        &successful_task,
        "plan_host_e2e_success",
        "step_host_e2e_success",
    );
    let successful_request = server_exec_rpc_request(
        Some(&successful_task.id),
        "run_host_e2e_success",
        "trace_host_e2e_success",
        "call_host_e2e_success",
        Some("plan_host_e2e_success"),
        Some("step_host_e2e_success"),
        "printf 'server-exec-e2e\\n'",
    );
    let successful = handle_sidecar_request_with_cancel(
        &state,
        HOST_TOOL_EXECUTE,
        successful_request.clone(),
        CancellationToken::new(),
    )
    .await
    .expect("authorized command returns host response");
    assert_eq!(successful["status"], "success");
    assert_eq!(successful["output"]["state"], "completed");
    assert_eq!(successful["output"]["exitCode"], 0);
    assert_eq!(successful["output"]["stdout"], "verified:server-exec-e2e\n");
    let captured = ssh_server
        .observed
        .commands
        .lock()
        .expect("command observations")
        .clone();
    assert_eq!(captured.len(), 1);
    assert!(captured[0].starts_with("/bin/sh -c "));
    assert!(captured[0].contains("server-exec-e2e"));
    let grant_after_success = state
        .database
        .investigations()
        .get_task(&successful_task.id)
        .expect("read updated grant")
        .guardrails
        .command_grant
        .expect("persisted command grant");
    assert_eq!(grant_after_success.calls_used, 1);
    assert_eq!(grant_after_success.total_duration_ms, 5_000);
    assert_eq!(grant_after_success.total_output_bytes, 4_096);
    let replayed = handle_sidecar_request_with_cancel(
        &state,
        HOST_TOOL_EXECUTE,
        successful_request,
        CancellationToken::new(),
    )
    .await
    .expect("duplicate host RPC has a bounded process-local replay");
    assert_eq!(replayed, successful);
    assert_eq!(
        ssh_server
            .observed
            .commands
            .lock()
            .expect("command observations")
            .len(),
        1,
        "a duplicate host RPC must not execute the remote command twice"
    );

    let uncertain_grant = TaskCommandGrant {
        grant_id: "cmdgrant_host_e2e_unknown".into(),
        task_id: "task_host_e2e_unknown".into(),
        server_id: server_id.into(),
        environment: Environment::Staging,
        granted_by: "user".into(),
        granted_at: yukinal_time::iso8601_utc(now),
        expires_at,
        max_calls: 2,
        calls_used: 0,
        max_total_duration_ms: 20_000,
        total_duration_ms: 0,
        max_total_output_bytes: 8_192,
        total_output_bytes: 0,
    };
    let uncertain_task = task_for_server_exec(
        "task_host_e2e_unknown",
        server_id,
        "run_host_e2e_unknown",
        Some(uncertain_grant),
    );
    seed_server_exec_plan(
        &state,
        &uncertain_task,
        "plan_host_e2e_unknown",
        "step_host_e2e_unknown",
    );
    let uncertain_request = server_exec_rpc_request(
        Some(&uncertain_task.id),
        "run_host_e2e_unknown",
        "trace_host_e2e_unknown",
        "call_host_e2e_unknown",
        Some("plan_host_e2e_unknown"),
        Some("step_host_e2e_unknown"),
        "drop-after-request",
    );
    let uncertain = handle_sidecar_request_with_cancel(
        &state,
        HOST_TOOL_EXECUTE,
        uncertain_request.clone(),
        CancellationToken::new(),
    )
    .await
    .expect("lost remote exit status returns structured response");
    assert_eq!(uncertain["status"], "failed");
    assert_eq!(uncertain["error"]["code"], "execution_failed");
    assert_eq!(uncertain["error"]["detail"]["state"], "result_unknown");
    let call_claim = state
        .database
        .host_tool_calls()
        .claim(yukinal_database::repositories::HostToolCallInput {
            trace_id: "trace_host_e2e_unknown",
            call_id: "call_host_e2e_unknown",
            task_id: Some(&uncertain_task.id),
            plan_id: Some("plan_host_e2e_unknown"),
            plan_step_id: Some("step_host_e2e_unknown"),
            tool_name: "server.exec",
            request_fingerprint: "not-used-for-existing-action",
            action_fingerprint: None,
            started_at: "2026-09-30T00:00:00Z",
        })
        .expect("read durable command call status");
    assert!(matches!(
        call_claim,
        HostToolCallClaim::Existing {
            status: HostToolCallStatus::Uncertain,
            ..
        }
    ));
    let unknown_grant_after = state
        .database
        .investigations()
        .get_task(&uncertain_task.id)
        .expect("read consumed unknown grant")
        .guardrails
        .command_grant
        .expect("unknown command retains its retired but inspectable grant budget");
    assert_eq!(unknown_grant_after.calls_used, 1);
    let mut replay_with_new_call_id = uncertain_request;
    replay_with_new_call_id["callId"] = json!("call_host_e2e_unknown_retry");
    let replay_blocked = handle_sidecar_request_with_cancel(
        &state,
        HOST_TOOL_EXECUTE,
        replay_with_new_call_id,
        CancellationToken::new(),
    )
    .await
    .expect("uncertain logical action is blocked before a second SSH request");
    assert_eq!(replay_blocked["status"], "failed");
    assert_eq!(replay_blocked["error"]["code"], "plan_deviation");
    assert_eq!(replay_blocked["error"]["detail"]["code"], "duplicate_call");
    assert_eq!(
        ssh_server
            .observed
            .commands
            .lock()
            .expect("command observations")
            .len(),
        2,
        "a lost exit status must never cause an automatic logical replay"
    );

    state
        .ssh
        .close(
            &state
                .terminals
                .cached_session(server_id)
                .expect("cached session"),
        )
        .await
        .expect("close loopback SSH session");
    drop(state);
}

#[test]
fn server_exec_input_fingerprint_matches_agent_canonical_json() {
    let input = json!({
        "command": "printf '%s' hello",
        "env": { "LC_ALL": "C", "LANG": "C.UTF-8" },
        "maxOutputBytes": 4_096,
        "purpose": "verify fingerprint",
        "timeoutMs": 1_000,
    });
    assert_eq!(
        server_exec_input_fingerprint(&input).expect("canonical fingerprint"),
        "6a3e1291bf14b90df757000da0a3c14545d82fe1563bd795f40499c2d0e52fda"
    );
}

fn guarded_write_request(content: &str) -> HostToolExecuteRequest {
    HostToolExecuteRequest {
        call_id: "call_guarded_write".into(),
        run_id: Some("run_guarded_write".into()),
        trace_id: "trace_guarded_write".into(),
        tool_name: FILESYSTEM_WRITE.into(),
        input: json!({ "path": "/etc/app.env", "content": content }),
        target: HostToolTarget {
            host: "remote".into(),
            server_id: Some("srv_fixture".into()),
            workspace_id: None,
            environment: Environment::Staging,
        },
        task_id: Some("task_fixture".into()),
        plan_id: Some("plan_fixture".into()),
        plan_step_id: Some("step_fixture".into()),
        evidence_ids: Some(vec!["ev_fixture".into()]),
        approval_id: None,
    }
}

#[tokio::test]
async fn effectful_agent_calls_fail_closed_without_a_complete_durable_plan_binding() {
    let directory =
        std::env::temp_dir().join(format!("yukinal-host-effectful-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    let state = AppState::bootstrap(&directory).expect("bootstrap host state");

    let mut ordinary = guarded_write_request("one");
    ordinary.task_id = None;
    ordinary.plan_id = None;
    ordinary.plan_step_id = None;
    assert!(effectful_tool_requires_durable_plan(&state, &ordinary).await);

    let mut incomplete = guarded_write_request("one");
    incomplete.plan_step_id = None;
    assert!(effectful_tool_requires_durable_plan(&state, &incomplete).await);

    let planned = guarded_write_request("one");
    assert!(!effectful_tool_requires_durable_plan(&state, &planned).await);

    drop(state);
    let _ = std::fs::remove_dir_all(&directory);
}

#[tokio::test]
async fn mcp_effectful_classification_fails_closed_without_a_resolvable_catalog() {
    let directory =
        std::env::temp_dir().join(format!("yukinal-host-mcp-effectful-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    let state = AppState::bootstrap(&directory).expect("bootstrap host state");

    // A built-in name is classified by name, no catalog needed.
    assert!(is_effectful_host_tool(&state, FILESYSTEM_WRITE).await);
    assert!(!is_effectful_host_tool(&state, SERVER_INFO).await);
    // No MCP server is running and no catalog can resolve this name, so it is not `low`:
    // the host must treat it as effectful (fail closed, ADR 0074).
    assert!(is_effectful_host_tool(&state, "mcp.some-server.echo").await);

    drop(state);
    let _ = std::fs::remove_dir_all(&directory);
}

#[tokio::test]
async fn ordinary_effectful_host_request_is_rejected_before_target_resolution() {
    let directory =
        std::env::temp_dir().join(format!("yukinal-host-plan-gate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    let state = AppState::bootstrap(&directory).expect("bootstrap host state");
    let response = handle_sidecar_request_with_cancel(
        &state,
        HOST_TOOL_EXECUTE,
        json!({
            "callId": "call_chat_write",
            "traceId": "trace_chat_write",
            "toolName": FILESYSTEM_WRITE,
            "input": { "path": "/srv/app/config.yml", "content": "x" },
            "target": {
                "host": "remote",
                "serverId": "srv_not_registered",
                "environment": "staging"
            }
        }),
        CancellationToken::new(),
    )
    .await
    .expect("host request should return a structured refusal");

    assert_eq!(response["status"], json!("failed"));
    assert_eq!(response["error"]["code"], json!("denied_by_policy"));
    assert_eq!(
        response["error"]["detail"]["code"],
        json!("durable_plan_required")
    );
    drop(state);
    let _ = std::fs::remove_dir_all(&directory);
}

#[tokio::test]
async fn evidence_record_host_rpc_survives_host_restart_and_remains_fetchable() {
    let directory = std::env::temp_dir().join(format!(
        "yukinal-host-rpc-evidence-restart-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir(&directory).expect("create isolated SQLite directory");
    let state = AppState::bootstrap(&directory).expect("bootstrap host state");
    let task = lifecycle_task();
    state
        .database
        .investigations()
        .create_task(&task)
        .expect("create durable investigation task");
    state
        .database
        .investigations()
        .create_run(&InvestigationRun {
            id: "run_lifecycle".into(),
            task_id: task.id.clone(),
            session_id: None,
            message_id: None,
            trace_id: Some("trace_host_rpc_restart".into()),
            attempt: 1,
            phase: TaskPhase::Execution,
            status: InvestigationRunStatus::Running,
            started_at: yukinal_time::iso8601_utc(yukinal_time::now_epoch_seconds()),
            updated_at: yukinal_time::iso8601_utc(yukinal_time::now_epoch_seconds()),
            ended_at: None,
            checkpoint: None,
            failure: None,
        })
        .expect("create active investigation run");

    let content = json!({ "hostname": "fixture-host", "status": "reachable" });
    let evidence = Evidence {
        id: "ev_host_rpc_restart".into(),
        task_id: task.id.clone(),
        // The host must replace sidecar-supplied run attribution with its active run.
        run_id: Some("run_from_agent".into()),
        scope: task.scope.clone(),
        kind: EvidenceKind::Snapshot,
        source_tool: SERVER_INFO.into(),
        collected_at: yukinal_time::iso8601_utc(yukinal_time::now_epoch_seconds()),
        input_summary: "server.info".into(),
        content_type: EvidenceContentType::Json,
        content_hash: format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&content).unwrap())
        ),
        content,
        truncated: false,
        redaction_status: EvidenceRedactionStatus::Clean,
    };
    let recorded = handle_sidecar_request_with_cancel(
        &state,
        "host.investigation.evidence.record",
        json!({ "evidence": evidence }),
        CancellationToken::new(),
    )
    .await
    .expect("record evidence over the host RPC protocol");
    assert_eq!(
        recorded,
        json!({
            "recorded": true,
            "evidenceId": "ev_host_rpc_restart",
            "reused": false,
        })
    );

    // Reopening AppState closes the first SQLite connection and exercises the same host
    // request boundary used by the sidecar, rather than querying the database as a side channel.
    drop(state);
    let reopened = AppState::bootstrap(&directory).expect("reopen host state");
    let fetched = handle_sidecar_request_with_cancel(
        &reopened,
        "host.investigation.evidence.fetch",
        json!({
            "taskId": "task_lifecycle",
            "evidenceId": "ev_host_rpc_restart",
        }),
        CancellationToken::new(),
    )
    .await
    .expect("fetch persisted evidence over the host RPC protocol");
    assert_eq!(fetched["status"], json!("success"));
    assert_eq!(fetched["evidence"]["id"], json!("ev_host_rpc_restart"));
    assert_eq!(fetched["evidence"]["runId"], json!("run_lifecycle"));
    assert_eq!(
        fetched["evidence"]["content"],
        json!({ "hostname": "fixture-host", "status": "reachable" })
    );

    drop(reopened);
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn evidence_freshness_is_host_clocked_and_explicit_at_each_boundary() {
    let now = yukinal_time::parse_iso8601_utc("2026-09-20T00:00:00Z").unwrap();
    let fresh = evidence_freshness_at("2026-09-19T23:45:00Z", now);
    assert_eq!(fresh.status, "fresh");
    assert_eq!(fresh.age_seconds, Some(900));

    let stale = evidence_freshness_at("2026-09-19T23:44:59Z", now);
    assert_eq!(stale.status, "stale");
    assert_eq!(stale.age_seconds, Some(901));

    let expired = evidence_freshness_at("2026-09-18T23:59:59Z", now);
    assert_eq!(expired.status, "expired");
    assert_eq!(expired.age_seconds, Some(86_401));

    let invalid = evidence_freshness_at("not-a-timestamp", now);
    assert_eq!(invalid.status, "unknown");
    assert!(invalid.age_seconds.is_none());

    let future = evidence_freshness_at("2026-09-20T00:00:01Z", now);
    assert_eq!(future.status, "unknown");
    assert!(future.reason.unwrap().contains("future"));
}

#[test]
fn task_guardrails_block_forbidden_tools_paths_and_outside_windows() {
    let task = InvestigationTask {
        id: "task_guardrails".into(),
        workspace_id: None,
        server_id: Some("srv_fixture".into()),
        objective: "bounded task".into(),
        success_criteria: vec!["collect evidence".into()],
        scope: InvestigationTarget {
            host: InvestigationTargetHost::Remote,
            server_id: Some("srv_fixture".into()),
            workspace_id: None,
            environment: Environment::Staging,
        },
        guardrails: InvestigationTaskGuardrails {
            not_before_at: None,
            expires_at: None,
            forbidden_tools: vec![FILESYSTEM_WRITE.into()],
            forbidden_path_prefixes: vec!["/srv/app/private".into()],
            command_grant: None,
        },
        mode: InvestigationRunMode::Goal,
        permission_mode: InvestigationPermissionMode::Ask,
        automation_level: TaskAutomationLevel::Propose,
        created_by: "test".into(),
        phase: TaskPhase::Investigating,
        status: TaskStatus::Investigating,
        budget: TaskBudget {
            max_steps: 4,
            max_run_ms: 60_000,
            max_attempts: 1,
        },
        created_at: "2026-09-20T00:00:00Z".into(),
        updated_at: "2026-09-20T00:00:00Z".into(),
        completed_at: None,
        active_run_id: None,
        last_failure: None,
    };
    let tool = task_guardrail_violation(&task, FILESYSTEM_WRITE, &json!({"path": "/etc/app.env"}));
    assert_eq!(
        tool.map(|violation| violation.code),
        Some(yukinal_database::models::PlanDeviationCode::ScopeForbidden)
    );
    let path = task_guardrail_violation(
        &task,
        SERVER_INFO,
        &json!({"path": "/srv/app/private/config"}),
    );
    assert_eq!(
        path.map(|violation| violation.code),
        Some(yukinal_database::models::PlanDeviationCode::ScopeForbidden)
    );
    let batch_paths = task_guardrail_violation(
        &task,
        FILESYSTEM_BACKUP_CLEANUP,
        &json!({
            "items": [{
                "path": "/srv/app/private/config",
                "backupPath": "/srv/app/private/config.backup",
                "expectedRevision": "a".repeat(64),
            }],
        }),
    );
    assert_eq!(
        batch_paths.map(|violation| violation.code),
        Some(yukinal_database::models::PlanDeviationCode::ScopeForbidden)
    );

    let mut timed = task;
    timed.guardrails.not_before_at = Some("2999-01-01T00:00:00Z".into());
    let window =
        task_guardrail_violation(&timed, SERVER_INFO, &json!({})).expect("future window must deny");
    assert_eq!(
        window.code,
        yukinal_database::models::PlanDeviationCode::OutsideTimeWindow
    );
}

#[test]
fn evidence_comparison_reports_bounded_json_paths_without_values() {
    let left = json!({ "status": "ok", "nested": { "count": 1 }, "items": ["a"] });
    let right = json!({ "status": "degraded", "nested": { "count": 2 }, "items": ["a", "b"] });
    let mut diff = super::JsonDiff::default();
    collect_json_diff(&left, &right, "$", 0, &mut diff);
    assert!(diff.paths.iter().any(|path| path == "$.status"));
    assert!(diff.paths.iter().any(|path| path == "$.nested.count"));
    assert!(diff.paths.iter().any(|path| path == "$.items[1]"));
    assert!(!diff.paths.iter().any(|path| path.contains("degraded")));
    assert!(!diff.truncated);
}

#[test]
fn evidence_comparison_reports_text_counts_and_caps_large_output() {
    let (comparison, truncated) =
        compare_text_content(&json!("one\ntwo"), &json!("one\nthree\nfour"));
    let comparison = comparison.expect("text content should produce counts");
    assert_eq!(comparison.left_line_count, 2);
    assert_eq!(comparison.right_line_count, 3);
    assert_eq!(comparison.changed_line_count, 2);
    assert_eq!(comparison.added_line_count, 1);
    assert!(!truncated);

    let huge = (0..100_001).map(|_| "x").collect::<Vec<_>>().join("\n");
    let (_, truncated) = compare_text_content(&json!(huge), &json!("x"));
    assert!(truncated);
}

#[test]
fn evidence_correlation_returns_same_run_metadata_without_raw_bodies() {
    let directory = std::env::temp_dir().join(format!(
        "yukinal-host-evidence-correlation-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    let state = AppState::bootstrap(&directory).expect("bootstrap host state");
    let scope = InvestigationTarget {
        host: InvestigationTargetHost::Remote,
        server_id: Some("srv_fixture".into()),
        workspace_id: None,
        environment: Environment::Staging,
    };
    let task = InvestigationTask {
        id: "task_correlation".into(),
        workspace_id: None,
        server_id: Some("srv_fixture".into()),
        objective: "correlate evidence".into(),
        success_criteria: vec!["return bounded metadata".into()],
        scope: scope.clone(),
        guardrails: Default::default(),
        mode: InvestigationRunMode::Readonly,
        permission_mode: InvestigationPermissionMode::Ask,
        automation_level: TaskAutomationLevel::Readonly,
        created_by: "test".into(),
        phase: TaskPhase::Investigating,
        status: TaskStatus::Investigating,
        budget: TaskBudget {
            max_steps: 10,
            max_run_ms: 60_000,
            max_attempts: 1,
        },
        created_at: "2026-09-20T00:00:00Z".into(),
        updated_at: "2026-09-20T00:00:00Z".into(),
        completed_at: None,
        active_run_id: Some("run_correlation".into()),
        last_failure: None,
    };
    state
        .database
        .investigations()
        .create_task(&task)
        .expect("create correlation task");
    state
        .database
        .investigations()
        .create_run(&InvestigationRun {
            id: "run_correlation".into(),
            task_id: task.id.clone(),
            session_id: None,
            message_id: None,
            trace_id: None,
            attempt: 1,
            phase: TaskPhase::Investigating,
            status: InvestigationRunStatus::Running,
            started_at: "2026-09-20T00:00:00Z".into(),
            updated_at: "2026-09-20T00:00:00Z".into(),
            ended_at: None,
            checkpoint: None,
            failure: None,
        })
        .expect("create correlation run");

    let insert_evidence =
        |id: &str, source_tool: &str, kind: EvidenceKind, content: serde_json::Value| {
            let content_hash = format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&content).unwrap())
            );
            state
                .database
                .investigations()
                .add_evidence(&Evidence {
                    id: id.into(),
                    task_id: task.id.clone(),
                    run_id: Some("run_correlation".into()),
                    scope: scope.clone(),
                    kind,
                    source_tool: source_tool.into(),
                    collected_at: "2026-09-20T00:00:01Z".into(),
                    input_summary: format!("{source_tool} summary"),
                    content_type: EvidenceContentType::Json,
                    content,
                    content_hash,
                    truncated: false,
                    redaction_status: EvidenceRedactionStatus::Clean,
                })
                .expect("insert correlation evidence");
        };
    insert_evidence(
        "ev_correlation_anchor",
        SERVER_INFO,
        EvidenceKind::Snapshot,
        json!({ "hostname": "fixture", "secret": "must stay host-side" }),
    );
    insert_evidence(
        "ev_correlation_logs",
        DOCKER_LOGS,
        EvidenceKind::Log,
        json!({ "lines": ["request latency increased"] }),
    );

    let result = handle_evidence_correlation(
        &state,
        json!({
            "taskId": task.id,
            "anchorEvidenceId": "ev_correlation_anchor",
            "limit": 8,
        }),
    )
    .expect("correlation handler succeeds");
    assert_eq!(result["status"], json!("success"));
    assert_eq!(result["correlation"]["matchedBy"], json!("same_run"));
    let summaries = result["correlation"]["evidence"]
        .as_array()
        .expect("evidence summaries");
    assert_eq!(summaries.len(), 2);
    assert_eq!(
        result["correlation"]["sourceTools"],
        json!([DOCKER_LOGS, SERVER_INFO])
    );
    assert!(result["correlation"]["anchor"].get("content").is_none());
    assert!(summaries
        .iter()
        .all(|summary| summary.get("content").is_none()));
    drop(state);
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn filesystem_backup_ledger_binding_is_stable_and_task_scoped() {
    let first = backup_record_id("srv_fixture", "/etc/.yukinal-backup-a");
    assert_eq!(
        first,
        backup_record_id("srv_fixture", "/etc/.yukinal-backup-a")
    );
    assert_ne!(
        first,
        backup_record_id("srv_fixture", "/etc/.yukinal-backup-b")
    );

    let request = guarded_write_request("one");
    let record = FilesystemBackupRecord {
        id: first,
        server_id: "srv_fixture".into(),
        task_id: Some("task_fixture".into()),
        plan_id: Some("plan_fixture".into()),
        plan_step_id: Some("backup_step".into()),
        trace_id: Some(request.trace_id.clone()),
        call_id: Some(request.call_id.clone()),
        path: "/etc/app.env".into(),
        backup_path: "/etc/.yukinal-backup-a".into(),
        revision: "a".repeat(64),
        bytes_backed_up: 3,
        status: FilesystemBackupStatus::Available,
        created_at: "2026-09-20T00:00:00Z".into(),
        updated_at: "2026-09-20T00:00:00Z".into(),
        restored_at: None,
        deleted_at: None,
    };
    assert!(backup_owner_matches_request(&record, &request));
    let mut other = request.clone();
    other.task_id = Some("task_other".into());
    assert!(!backup_owner_matches_request(&record, &other));
}

#[tokio::test]
async fn host_action_replay_is_cached_but_a_different_payload_is_refused() {
    let directory =
        std::env::temp_dir().join(format!("yukinal-host-idempotency-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    let state = AppState::bootstrap(&directory).expect("bootstrap host state");
    let request = guarded_write_request("one");

    let token = match prepare_host_tool_call(&state, &request)
        .await
        .expect("first claim")
    {
        HostToolCallDecision::Execute(Some(token)) => token,
        other => panic!("first action was not claimed: {other:?}"),
    };
    let response_value = success(json!({
        "path": "/etc/app.env",
        "bytesWritten": 3,
    }));
    let response = Ok(response_value.clone());
    assert_eq!(
        host_tool_call_status(&response),
        yukinal_database::repositories::HostToolCallStatus::Success
    );
    finish_host_tool_call(&state, &request, Some(token), &response).expect("finish action");

    // The host returns the exact cached response and does not claim a second execution.
    match prepare_host_tool_call(&state, &request)
        .await
        .expect("same call")
    {
        HostToolCallDecision::Respond(replayed) => assert_eq!(replayed, response_value),
        other => panic!("same action was not replayed: {other:?}"),
    }

    // A resumed task may receive a fresh provider call ID. The logical
    // plan binding still identifies it as the same remote action.
    let mut resumed = request.clone();
    resumed.call_id = "call_guarded_write_after_restart".into();
    match prepare_host_tool_call(&state, &resumed)
        .await
        .expect("new call id")
    {
        HostToolCallDecision::Respond(ref refusal) => {
            assert_eq!(refusal["error"]["code"], json!("plan_deviation"));
            assert_eq!(refusal["error"]["detail"]["code"], json!("duplicate_call"));
        }
        other => panic!("new call id was not refused: {other:?}"),
    }

    let changed = guarded_write_request("two");
    match prepare_host_tool_call(&state, &changed)
        .await
        .expect("changed call")
    {
        HostToolCallDecision::Respond(ref refusal) => {
            assert_eq!(refusal["error"]["code"], json!("plan_deviation"));
            assert_eq!(refusal["error"]["detail"]["code"], json!("duplicate_call"));
        }
        other => panic!("changed action was not refused: {other:?}"),
    }
    drop(state);

    // A desktop restart has the durable row but no raw response cache. It
    // must fail closed instead of reconstructing and repeating the write.
    let reopened = AppState::bootstrap(&directory).expect("reopen host state");
    match prepare_host_tool_call(&reopened, &request)
        .await
        .expect("replayed after restart")
    {
        HostToolCallDecision::Respond(ref refusal) => {
            assert_eq!(refusal["error"]["code"], json!("plan_deviation"));
            assert_eq!(refusal["error"]["detail"]["status"], json!("success"));
        }
        other => panic!("restart replay was not refused: {other:?}"),
    }
    drop(reopened);
    let _ = std::fs::remove_dir_all(&directory);
}

#[tokio::test]
async fn planned_read_replay_is_refused_before_a_second_remote_execution() {
    let directory =
        std::env::temp_dir().join(format!("yukinal-host-read-fence-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    let state = AppState::bootstrap(&directory).expect("bootstrap host state");
    let request = HostToolExecuteRequest {
        call_id: "call_server_info_1".into(),
        run_id: Some("run_server_info_1".into()),
        trace_id: "trace_server_info".into(),
        tool_name: SERVER_INFO.into(),
        input: json!({}),
        target: HostToolTarget {
            host: "remote".into(),
            server_id: Some("srv_fixture".into()),
            workspace_id: None,
            environment: Environment::Staging,
        },
        task_id: Some("task_read_fence".into()),
        plan_id: Some("plan_read_fence".into()),
        plan_step_id: Some("step_read_fence".into()),
        evidence_ids: Some(vec![]),
        approval_id: None,
    };

    let token = match prepare_host_tool_call(&state, &request)
        .await
        .expect("first read claim")
    {
        HostToolCallDecision::Execute(Some(token)) => token,
        other => panic!("first planned read was not claimed: {other:?}"),
    };
    let response = Ok(success(json!({ "hostname": "fixture" })));
    finish_host_tool_call(&state, &request, Some(token), &response).expect("finish planned read");

    // A provider retry commonly has a new call ID. The durable plan step,
    // not that transient ID, is the identity of the logical observation.
    let mut retry = request.clone();
    retry.call_id = "call_server_info_2".into();
    match prepare_host_tool_call(&state, &retry)
        .await
        .expect("replayed planned read")
    {
        HostToolCallDecision::Respond(ref refusal) => {
            assert_eq!(refusal["error"]["code"], json!("plan_deviation"));
            assert_eq!(refusal["error"]["detail"]["code"], json!("duplicate_call"));
        }
        other => panic!("planned read reached execution twice: {other:?}"),
    }

    // The fence is scoped to a durable plan. Ordinary chat reads retain
    // their previous retryable behavior.
    let mut ordinary = request;
    ordinary.task_id = None;
    ordinary.plan_id = None;
    ordinary.plan_step_id = None;
    ordinary.call_id = "call_server_info_chat".into();
    assert!(matches!(
        prepare_host_tool_call(&state, &ordinary)
            .await
            .expect("ordinary read claim"),
        HostToolCallDecision::Execute(None)
    ));

    drop(state);
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn the_edit_failures_map_onto_the_existing_failure_codes() {
    let cancel = CancellationToken::new();

    // 过期 revision：这是「你的入参旧了，重读再来一次」，所以可重试，并且把两个 revision
    // 都放进 detail —— 模型据此知道文件确实变了，而不是自己抄错了。
    let mismatch = filesystem_failure(
        FilesystemError::RevisionMismatch {
            expected: "aa".repeat(32),
            actual: "bb".repeat(32),
        },
        &cancel,
    );
    assert_eq!(mismatch["status"], json!("failed"));
    assert_eq!(mismatch["error"]["code"], json!("invalid_input"));
    assert_eq!(mismatch["error"]["retryable"], json!(true));
    assert_eq!(
        mismatch["error"]["detail"]["expectedRevision"],
        json!("aa".repeat(32))
    );
    assert_eq!(
        mismatch["error"]["detail"]["actualRevision"],
        json!("bb".repeat(32))
    );

    // 文件超过编辑上限：重试同一个调用永远不会成功（要变的是文件），所以不可重试；文案里
    // 必须出现 `filesystem.write`，因为那才是用户/模型该走的下一步。
    let too_large = filesystem_failure(
        FilesystemError::FileTooLargeToEdit { limit: 524_288 },
        &cancel,
    );
    assert_eq!(too_large["error"]["code"], json!("invalid_input"));
    assert_eq!(too_large["error"]["retryable"], json!(false));
    assert_eq!(
        too_large["error"]["detail"]["maxEditableBytes"],
        json!(524_288)
    );
    let message = too_large["error"]["message"]
        .as_str()
        .expect("the refusal carries a message");
    assert!(message.contains("524288"), "{message}");
    assert!(message.contains("truncate"), "{message}");
    assert!(message.contains("filesystem.write"), "{message}");

    let backup_too_large = filesystem_failure(
        FilesystemError::FileTooLargeToBackup { limit: 524_288 },
        &cancel,
    );
    assert_eq!(backup_too_large["error"]["code"], json!("invalid_input"));
    assert_eq!(backup_too_large["error"]["retryable"], json!(false));
    assert_eq!(
        backup_too_large["error"]["detail"]["maxBackupBytes"],
        json!(524_288)
    );

    // 「远端做不到安全替换」有自己的码：把它塞进 invalid_input 会让模型把「换条路重试」
    // 当成正确反应，而这里要变的是服务器或这个文件。
    let unsafe_write = filesystem_failure(
        FilesystemError::UnsafeRemoteWrite(
            "/etc/app.env has 3 hard links; replacing it would leave the other names \
                 pointing at the old content"
                .to_string(),
        ),
        &cancel,
    );
    assert_eq!(unsafe_write["error"]["code"], json!("unsupported"));
    assert_eq!(unsafe_write["error"]["retryable"], json!(false));

    // 并发修改与过期的 revision 是同一类：重读之后带着新 revision 重试是对的。
    let concurrent = filesystem_failure(
        FilesystemError::ConcurrentChange(
            "/etc/app.env changed between the read and the metadata check (12 bytes → 15 \
                 bytes); re-read it and retry"
                .to_string(),
        ),
        &cancel,
    );
    assert_eq!(concurrent["error"]["code"], json!("invalid_input"));
    assert_eq!(concurrent["error"]["retryable"], json!(true));
    assert!(concurrent["error"]["message"]
        .as_str()
        .expect("message")
        .contains("re-read"));

    // metadata 保不住：点名是哪几项，同一份清单也进 detail。
    let metadata = filesystem_failure(
        FilesystemError::MetadataNotPreserved {
            message: "the remote would not keep the file's owner, group; the edit was not \
                          published"
                .to_string(),
            missing: vec!["owner".to_string(), "group".to_string()],
        },
        &cancel,
    );
    assert_eq!(metadata["error"]["code"], json!("unsupported"));
    assert_eq!(metadata["error"]["retryable"], json!(false));
    assert_eq!(
        metadata["error"]["detail"]["missingMetadata"],
        json!(["owner", "group"])
    );

    // 已有的两类映射不变：入参问题可重试，策略拒绝不可重试。
    let invalid = filesystem_failure(FilesystemError::InvalidInput("bad".to_string()), &cancel);
    assert_eq!(invalid["error"]["code"], json!("invalid_input"));
    assert_eq!(invalid["error"]["retryable"], json!(true));

    let denied = filesystem_failure(
        FilesystemError::DeniedByPolicy("blocked".to_string()),
        &cancel,
    );
    assert_eq!(denied["error"]["code"], json!("denied_by_policy"));
    assert_eq!(denied["error"]["retryable"], json!(false));

    // 传输失败与取消仍然走 `transport_or_cancel`：取消后报 cancelled，否则报 transport。
    let transport = filesystem_failure(
        FilesystemError::Transport(yukinal_filesystem::TransportError::new("link down")),
        &cancel,
    );
    assert_eq!(transport["error"]["code"], json!("transport"));
    cancel.cancel();
    let cancelled = filesystem_failure(
        FilesystemError::Transport(yukinal_filesystem::TransportError::new("link down")),
        &cancel,
    );
    assert_eq!(cancelled["error"]["code"], json!("cancelled"));
}

#[test]
fn cancellation_registry_cancels_and_removes_a_running_request() {
    let registry: HostCancellationRegistry = Arc::new(Mutex::new(HashMap::new()));
    let token = CancellationToken::new();
    registry
        .lock()
        .expect("registry lock")
        .insert(7, token.clone());

    let result = cancel_sidecar_request(&registry, json!({ "requestId": 7 }))
        .expect("cancellation response");
    assert_eq!(result["cancelled"], json!(true));
    assert!(token.is_cancelled());

    let result = cancel_sidecar_request(&registry, json!({ "requestId": 7 }))
        .expect("second cancellation response");
    assert_eq!(result["cancelled"], json!(false));
}

#[test]
fn host_plan_binding_fails_closed_for_missing_or_stale_metadata() {
    let plan = InvestigationPlan {
        id: "plan_1".into(),
        task_id: "task_1".into(),
        revision: 1,
        status: PlanStatus::Active,
        created_at: "2026-09-20T00:00:00Z".into(),
        updated_at: "2026-09-20T00:00:00Z".into(),
        current_step_id: Some("step_1".into()),
        approval: None,
        observation_window: None,
        steps: vec![InvestigationPlanStep {
            id: "step_1".into(),
            ordinal: 0,
            kind: PlanStepKind::Evidence,
            title: "Read baseline".into(),
            purpose: "Establish the current state".into(),
            allowed_tools: vec!["server.info".into()],
            input_bindings: None,
            idempotency: Some(yukinal_database::models::PlanIdempotency::Safe),
            risk_level: Some(yukinal_database::models::RiskLevel::Read),
            requires_baseline: None,
            preconditions: Some(vec!["target identity is verified".into()]),
            verification_criteria: Some(vec!["a snapshot exists".into()]),
            preview: None,
            rollback: None,
            target: None,
            evidence_ids: vec!["ev_1".into()],
            success_criteria: vec!["A snapshot exists".into()],
            requires_approval: false,
            max_attempts: 1,
            attempts: 0,
            status: PlanStepStatus::Running,
            started_at: None,
            ended_at: None,
            last_deviation: None,
        }],
    };
    let step = &plan.steps[0];
    assert!(plan_binding_matches(
        &plan,
        step,
        Some("plan_1"),
        Some("step_1"),
        Some(&["ev_1".to_string()]),
    ));
    assert!(plan_binding_matches(&plan, step, None, None, None));
    assert!(!plan_binding_matches(
        &plan,
        step,
        None,
        Some("step_1"),
        Some(&["ev_1".to_string()]),
    ));
    assert!(!plan_binding_matches(
        &plan,
        step,
        Some("plan_0"),
        Some("step_1"),
        Some(&["ev_1".to_string()]),
    ));
    assert!(!plan_binding_matches(
        &plan,
        step,
        Some("plan_1"),
        Some("step_1"),
        Some(&[]),
    ));

    let mut bound_step = step.clone();
    bound_step.input_bindings = Some(HashMap::from([("path".into(), "/etc/app.env".into())]));
    assert!(input_bindings_match(
        &bound_step,
        &json!({ "path": "/etc/app.env", "oldString": "a" }),
    ));
    assert!(!input_bindings_match(
        &bound_step,
        &json!({ "path": "/etc/other.env" }),
    ));
    assert!(!input_bindings_match(&bound_step, &json!({})));
    assert!(input_bindings_match(
        step,
        &json!({ "anything": "is accepted for legacy steps" })
    ));

    // A binding whose tool input is a JSON array pins structural input: `backup_rotation`
    // binds its whole item list this way (ADR 0076). Object key order must not matter, but
    // every path/backupPath/revision must match exactly.
    let mut batch_step = step.clone();
    let items = json!([{
        "path": "/etc/app.env",
        "backupPath": "/etc/.b1",
        "expectedRevision": "a".repeat(64),
    }]);
    batch_step.input_bindings = Some(HashMap::from([(
        "items".into(),
        serde_json::to_string(&items).expect("json"),
    )]));
    assert!(input_bindings_match(
        &batch_step,
        &json!({ "items": items.clone() })
    ));
    assert!(!input_bindings_match(
        &batch_step,
        &json!({ "items": [{ "path": "/etc/other.env", "backupPath": "/etc/.b1", "expectedRevision": "a".repeat(64) }] })
    ));

    let mut observation_plan = plan.clone();
    observation_plan.steps[0].kind = PlanStepKind::Verification;
    observation_plan.observation_window = Some(InvestigationObservationWindow {
        duration_seconds: 60,
        interval_seconds: 10,
        allowed_tools: vec![SERVER_INFO.into()],
        success_criteria: vec!["samples remain healthy".into()],
        status: ObservationWindowStatus::Running,
        sample_count: 1,
        started_at: Some("2026-09-20T00:00:00Z".into()),
        deadline_at: Some("2026-09-20T00:01:00Z".into()),
        deadline_epoch_seconds: Some(1_600),
        last_sample_at: Some("2026-09-20T00:00:00Z".into()),
        last_sample_epoch_seconds: Some(1_540),
        last_failure: None,
    });
    assert!(plan_allows_repeated_observation(
        &observation_plan,
        "step_1"
    ));
    observation_plan.observation_window.as_mut().unwrap().status = ObservationWindowStatus::Pending;
    assert!(!plan_allows_repeated_observation(
        &observation_plan,
        "step_1"
    ));
}

#[test]
fn plan_replay_preserves_host_progress_but_cannot_reuse_an_approved_definition_for_changes() {
    let previous = InvestigationPlan {
        id: "plan_replay".into(),
        task_id: "task_1".into(),
        revision: 2,
        status: PlanStatus::Active,
        created_at: "2026-09-20T00:00:00Z".into(),
        updated_at: "2026-09-20T00:00:02Z".into(),
        current_step_id: Some("step_1".into()),
        approval: Some(InvestigationPlanApproval {
            status: PlanApprovalStatus::Approved,
            source: Some(PlanApprovalSource::User),
            option_id: Some("option_1".into()),
            approved_at: Some("2026-09-20T00:00:01Z".into()),
            note: None,
        }),
        observation_window: None,
        steps: vec![InvestigationPlanStep {
            id: "step_1".into(),
            ordinal: 0,
            kind: PlanStepKind::Action,
            title: "Apply exact edit".into(),
            purpose: "Change one bound path".into(),
            allowed_tools: vec![FILESYSTEM_WRITE.into()],
            input_bindings: Some(HashMap::from([("path".into(), "/etc/app.env".into())])),
            idempotency: Some(yukinal_database::models::PlanIdempotency::Conditional),
            risk_level: Some(RiskLevel::Medium),
            requires_baseline: Some(true),
            preconditions: Some(vec!["baseline exists".into()]),
            verification_criteria: Some(vec!["file is readable".into()]),
            preview: Some("replace one exact value".into()),
            rollback: Some("restore the baseline".into()),
            target: None,
            evidence_ids: vec!["ev_1".into()],
            success_criteria: vec!["edit succeeds".into()],
            requires_approval: true,
            max_attempts: 1,
            attempts: 2,
            status: PlanStepStatus::Running,
            started_at: Some("2026-09-20T00:00:02Z".into()),
            ended_at: None,
            last_deviation: None,
        }],
    };
    let mut replay = previous.clone();
    replay.status = PlanStatus::Draft;
    replay.updated_at = "2026-09-20T00:00:03Z".into();
    replay.current_step_id = None;
    replay.approval = None;
    replay.steps[0].attempts = 0;
    replay.steps[0].status = PlanStepStatus::Pending;
    replay.steps[0].started_at = None;

    assert_eq!(
        plan_definition(&previous).expect("previous definition"),
        plan_definition(&replay).expect("replay definition")
    );
    merge_plan_runtime(&mut replay, &previous);
    assert_eq!(replay.status, PlanStatus::Active);
    assert_eq!(replay.current_step_id.as_deref(), Some("step_1"));
    assert_eq!(
        replay.approval.as_ref().map(|approval| approval.status),
        Some(PlanApprovalStatus::Approved)
    );
    assert_eq!(
        replay
            .approval
            .as_ref()
            .and_then(|approval| approval.source),
        Some(PlanApprovalSource::User)
    );
    assert_eq!(replay.steps[0].attempts, 2);
    assert_eq!(replay.steps[0].status, PlanStepStatus::Running);

    replay.steps[0].input_bindings =
        Some(HashMap::from([("path".into(), "/etc/other.env".into())]));
    assert_ne!(
        plan_definition(&previous).expect("previous definition"),
        plan_definition(&replay).expect("changed definition")
    );

    let mut untrusted = previous;
    untrusted.status = PlanStatus::Completed;
    untrusted.current_step_id = None;
    untrusted.steps[0].attempts = 99;
    untrusted.steps[0].status = PlanStepStatus::Succeeded;
    untrusted.steps[0].started_at = None;
    normalize_new_plan_runtime(&mut untrusted, "2026-09-20T00:03:00Z");
    assert_eq!(untrusted.status, PlanStatus::Active);
    assert_eq!(untrusted.current_step_id.as_deref(), Some("step_1"));
    assert_eq!(untrusted.steps[0].attempts, 0);
    assert_eq!(untrusted.steps[0].status, PlanStepStatus::Running);
    assert_eq!(untrusted.created_at, "2026-09-20T00:03:00Z");
}

#[test]
fn a_required_baseline_must_belong_to_the_current_plan() {
    let plan = |id: &str, evidence_status| InvestigationPlan {
        id: id.into(),
        task_id: "task_1".into(),
        revision: 1,
        status: yukinal_database::models::PlanStatus::Active,
        created_at: "2026-09-20T00:00:00Z".into(),
        updated_at: "2026-09-20T00:00:01Z".into(),
        current_step_id: Some("action_1".into()),
        approval: None,
        observation_window: None,
        steps: vec![
            InvestigationPlanStep {
                id: "evidence_1".into(),
                ordinal: 0,
                kind: PlanStepKind::Evidence,
                title: "Collect baseline".into(),
                purpose: "Read current state".into(),
                allowed_tools: vec![SERVER_INFO.into()],
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
                success_criteria: vec!["baseline exists".into()],
                requires_approval: false,
                max_attempts: 1,
                attempts: 1,
                status: evidence_status,
                started_at: None,
                ended_at: None,
                last_deviation: None,
            },
            InvestigationPlanStep {
                id: "action_1".into(),
                ordinal: 1,
                kind: PlanStepKind::Action,
                title: "Apply change".into(),
                purpose: "Change state".into(),
                allowed_tools: vec![FILESYSTEM_WRITE.into()],
                input_bindings: None,
                idempotency: None,
                risk_level: None,
                requires_baseline: Some(true),
                preconditions: None,
                verification_criteria: Some(vec!["health check".into()]),
                preview: Some("write a staged change".into()),
                rollback: Some("restore baseline".into()),
                target: None,
                evidence_ids: vec![],
                success_criteria: vec!["change applied".into()],
                requires_approval: true,
                max_attempts: 1,
                attempts: 0,
                status: PlanStepStatus::Pending,
                started_at: None,
                ended_at: None,
                last_deviation: None,
            },
        ],
    };
    let artifact = |plan_id: Option<&str>, status| InvestigationArtifact {
        id: "baseline_1".into(),
        task_id: "task_1".into(),
        run_id: None,
        plan_id: plan_id.map(str::to_string),
        plan_step_id: Some("evidence_1".into()),
        phase: TaskPhase::Investigating,
        kind: TaskArtifactKind::Baseline,
        status,
        title: "基线".into(),
        summary: "fixture baseline".into(),
        content: json!({ "healthy": true }),
        evidence_ids: vec!["evidence_1".into()],
        created_at: "2026-09-20T00:00:00Z".into(),
        updated_at: "2026-09-20T00:00:01Z".into(),
    };
    let current_plan = plan("plan_new", PlanStepStatus::Succeeded);
    assert!(!baseline_artifact_matches_plan(
        &[artifact(Some("plan_old"), TaskArtifactStatus::Succeeded)],
        &current_plan
    ));
    assert!(!baseline_artifact_matches_plan(
        &[artifact(Some("plan_new"), TaskArtifactStatus::Draft)],
        &current_plan
    ));
    let mut missing_evidence = artifact(Some("plan_new"), TaskArtifactStatus::Ready);
    missing_evidence.evidence_ids.clear();
    assert!(!baseline_artifact_matches_plan(
        &[missing_evidence],
        &current_plan
    ));
    assert!(baseline_artifact_matches_plan(
        &[artifact(Some("plan_new"), TaskArtifactStatus::Ready)],
        &current_plan
    ));
}

#[test]
fn playbook_action_steps_require_preview_verification_and_safe_rollback() {
    let mut step = InvestigationPlanStep {
        id: "action_1".into(),
        ordinal: 0,
        kind: PlanStepKind::Action,
        title: "Publish config".into(),
        purpose: "Apply the approved change".into(),
        allowed_tools: vec!["filesystem.write".into()],
        input_bindings: None,
        idempotency: Some(yukinal_database::models::PlanIdempotency::Unsafe),
        risk_level: Some(yukinal_database::models::RiskLevel::High),
        requires_baseline: Some(true),
        preconditions: Some(vec!["baseline exists".into()]),
        verification_criteria: Some(vec!["health check passes".into()]),
        preview: None,
        rollback: None,
        target: None,
        evidence_ids: vec![],
        success_criteria: vec![],
        requires_approval: false,
        max_attempts: 1,
        attempts: 0,
        status: PlanStepStatus::Pending,
        started_at: None,
        ended_at: None,
        last_deviation: None,
    };
    assert!(validate_playbook_step(&step).unwrap().contains("preview"));
    step.preview = Some("write staging file, then publish".into());
    assert!(validate_playbook_step(&step)
        .unwrap()
        .contains("require approval"));
    step.requires_approval = true;
    assert!(validate_playbook_step(&step).unwrap().contains("rollback"));
    step.rollback = Some("restore the captured baseline".into());
    assert!(validate_playbook_step(&step).is_none());
}

#[test]
fn only_an_explicit_auto_executable_dev_or_staging_task_may_omit_medium_approval() {
    let scope = InvestigationTarget {
        host: InvestigationTargetHost::Remote,
        server_id: Some("srv_auto".into()),
        workspace_id: None,
        environment: Environment::Staging,
    };
    let task = InvestigationTask {
        id: "task_auto".into(),
        workspace_id: None,
        server_id: Some("srv_auto".into()),
        objective: "apply a guarded config edit".into(),
        success_criteria: vec!["the file is verified".into()],
        scope: scope.clone(),
        guardrails: Default::default(),
        mode: InvestigationRunMode::Goal,
        permission_mode: InvestigationPermissionMode::Auto,
        automation_level: TaskAutomationLevel::Execute,
        created_by: "test".into(),
        phase: TaskPhase::Execution,
        status: TaskStatus::Executing,
        budget: TaskBudget {
            max_steps: 10,
            max_run_ms: 60_000,
            max_attempts: 1,
        },
        created_at: "2026-09-20T00:00:00Z".into(),
        updated_at: "2026-09-20T00:00:00Z".into(),
        completed_at: None,
        active_run_id: Some("run_auto".into()),
        last_failure: None,
    };
    let step = InvestigationPlanStep {
        id: "action_auto".into(),
        ordinal: 0,
        kind: PlanStepKind::Action,
        title: "Apply edit".into(),
        purpose: "Change one exact path".into(),
        allowed_tools: vec![FILESYSTEM_EDIT.into()],
        input_bindings: None,
        idempotency: Some(PlanIdempotency::Conditional),
        risk_level: Some(RiskLevel::Medium),
        requires_baseline: Some(true),
        preconditions: Some(vec!["baseline exists".into()]),
        verification_criteria: Some(vec!["file is readable".into()]),
        preview: Some("replace one exact value".into()),
        rollback: Some("restore the captured baseline".into()),
        target: None,
        evidence_ids: vec![],
        success_criteria: vec!["edit succeeds".into()],
        requires_approval: false,
        max_attempts: 1,
        attempts: 0,
        status: PlanStepStatus::Pending,
        started_at: None,
        ended_at: None,
        last_deviation: None,
    };
    assert!(task_allows_auto_medium_action(&task, &step));
    let mut backup_step = step.clone();
    backup_step.allowed_tools = vec![FILESYSTEM_BACKUP.into()];
    assert!(task_allows_auto_medium_action(&task, &backup_step));

    // ADR 0073: whole-file write, and the common "back up, then write/edit" pair, are
    // now inside the delegated boundary too.
    let mut write_step = step.clone();
    write_step.allowed_tools = vec![FILESYSTEM_WRITE.into()];
    assert!(task_allows_auto_medium_action(&task, &write_step));
    let mut backup_edit_step = step.clone();
    backup_edit_step.allowed_tools = vec![FILESYSTEM_BACKUP.into(), FILESYSTEM_EDIT.into()];
    assert!(task_allows_auto_medium_action(&task, &backup_edit_step));
    let mut backup_write_step = step.clone();
    backup_write_step.allowed_tools = vec![FILESYSTEM_BACKUP.into(), FILESYSTEM_WRITE.into()];
    assert!(task_allows_auto_medium_action(&task, &backup_write_step));

    // Everything else stays behind a per-step approval.
    let mut ask_task = task.clone();
    ask_task.permission_mode = InvestigationPermissionMode::Ask;
    assert!(!task_allows_auto_medium_action(&ask_task, &step));
    let mut production_task = task.clone();
    production_task.scope.environment = Environment::Production;
    assert!(!task_allows_auto_medium_action(&production_task, &step));
    let mut local_task = task.clone();
    local_task.scope.host = InvestigationTargetHost::Local;
    local_task.scope.server_id = None;
    assert!(!task_allows_auto_medium_action(&local_task, &step));
    let mut high_step = step.clone();
    high_step.risk_level = Some(RiskLevel::High);
    assert!(!task_allows_auto_medium_action(&task, &high_step));
    let mut cleanup_step = step.clone();
    cleanup_step.allowed_tools = vec![FILESYSTEM_BACKUP_CLEANUP.into()];
    assert!(!task_allows_auto_medium_action(&task, &cleanup_step));
    let mut restore_step = step.clone();
    restore_step.allowed_tools = vec![FILESYSTEM_RESTORE.into()];
    assert!(!task_allows_auto_medium_action(&task, &restore_step));
    // A pair with no backup is not the "back up first" shape, and three tools is wider
    // than the delegated boundary.
    let mut edit_and_write_step = step.clone();
    edit_and_write_step.allowed_tools = vec![FILESYSTEM_EDIT.into(), FILESYSTEM_WRITE.into()];
    assert!(!task_allows_auto_medium_action(&task, &edit_and_write_step));
    let mut three_tools_step = step.clone();
    three_tools_step.allowed_tools = vec![
        FILESYSTEM_BACKUP.into(),
        FILESYSTEM_EDIT.into(),
        FILESYSTEM_WRITE.into(),
    ];
    assert!(!task_allows_auto_medium_action(&task, &three_tools_step));
}

#[test]
fn observation_window_requires_final_verification_and_preserves_only_its_configuration() {
    let verification = InvestigationPlanStep {
        id: "verify_1".into(),
        ordinal: 0,
        kind: PlanStepKind::Verification,
        title: "Observe health".into(),
        purpose: "Confirm the service stays healthy".into(),
        allowed_tools: vec![SERVER_INFO.into(), DOCKER_LOGS.into()],
        input_bindings: None,
        idempotency: None,
        risk_level: Some(RiskLevel::Read),
        requires_baseline: None,
        preconditions: None,
        verification_criteria: Some(vec!["health remains good".into()]),
        preview: None,
        rollback: None,
        target: None,
        evidence_ids: vec![],
        success_criteria: vec!["no new error".into()],
        requires_approval: false,
        max_attempts: 8,
        attempts: 0,
        status: PlanStepStatus::Running,
        started_at: None,
        ended_at: None,
        last_deviation: None,
    };
    let mut window = InvestigationObservationWindow {
        duration_seconds: 60,
        interval_seconds: 10,
        allowed_tools: vec![SERVER_INFO.into()],
        success_criteria: vec!["no new error".into()],
        status: ObservationWindowStatus::Pending,
        sample_count: 0,
        started_at: None,
        deadline_at: None,
        deadline_epoch_seconds: None,
        last_sample_at: None,
        last_sample_epoch_seconds: None,
        last_failure: None,
    };
    assert!(validate_observation_window(&window, std::slice::from_ref(&verification)).is_none());
    reset_observation_window(&mut window);
    assert_eq!(window.status, ObservationWindowStatus::Pending);
    assert_eq!(window.sample_count, 0);
    assert!(window.deadline_at.is_none());

    let mut changed = window.clone();
    changed.allowed_tools.push(DOCKER_LOGS.into());
    assert!(!observation_configuration_matches(&window, &changed));
    changed.allowed_tools = window.allowed_tools.clone();
    assert!(observation_configuration_matches(&window, &changed));

    let mut not_final = verification.clone();
    not_final.kind = PlanStepKind::Evidence;
    assert!(validate_observation_window(&window, &[not_final]).is_some());
}

/* ── 备份生命周期（ADR 0076） ─────────────────────────────────────────── */

fn lifecycle_backup(
    id: &str,
    path: &str,
    backup_path: &str,
    created_at: &str,
) -> FilesystemBackupRecord {
    FilesystemBackupRecord {
        id: id.into(),
        server_id: "srv_fixture".into(),
        task_id: Some("task_lifecycle".into()),
        plan_id: None,
        plan_step_id: None,
        trace_id: None,
        call_id: None,
        path: path.into(),
        backup_path: backup_path.into(),
        revision: "a".repeat(64),
        bytes_backed_up: 3,
        status: FilesystemBackupStatus::Available,
        created_at: created_at.into(),
        updated_at: created_at.into(),
        restored_at: None,
        deleted_at: None,
    }
}

fn lifecycle_task() -> InvestigationTask {
    InvestigationTask {
        id: "task_lifecycle".into(),
        workspace_id: None,
        server_id: Some("srv_fixture".into()),
        objective: "rotate backups".into(),
        success_criteria: vec!["the ledger stays bounded".into()],
        scope: InvestigationTarget {
            host: InvestigationTargetHost::Remote,
            server_id: Some("srv_fixture".into()),
            workspace_id: None,
            environment: Environment::Staging,
        },
        guardrails: Default::default(),
        mode: InvestigationRunMode::Goal,
        permission_mode: InvestigationPermissionMode::Auto,
        automation_level: TaskAutomationLevel::Execute,
        created_by: "test".into(),
        phase: TaskPhase::Execution,
        status: TaskStatus::Executing,
        budget: TaskBudget {
            max_steps: 10,
            max_run_ms: 60_000,
            max_attempts: 1,
        },
        created_at: "2026-09-20T00:00:00Z".into(),
        updated_at: "2026-09-20T00:00:00Z".into(),
        completed_at: None,
        active_run_id: Some("run_lifecycle".into()),
        last_failure: None,
    }
}

fn lifecycle_request(tool_name: &str) -> HostToolExecuteRequest {
    HostToolExecuteRequest {
        call_id: "call_lifecycle".into(),
        run_id: Some("run_lifecycle".into()),
        trace_id: "trace_lifecycle".into(),
        tool_name: tool_name.into(),
        input: json!({}),
        target: HostToolTarget {
            host: "remote".into(),
            server_id: Some("srv_fixture".into()),
            workspace_id: None,
            environment: Environment::Staging,
        },
        task_id: Some("task_lifecycle".into()),
        plan_id: None,
        plan_step_id: None,
        evidence_ids: Some(vec![]),
        approval_id: None,
    }
}

#[test]
fn retention_candidates_group_by_path_and_intersect_the_two_filters() {
    let now = 1_700_000_000u64;
    let day = 86_400u64;
    let stamp = |days_ago: u64| yukinal_time::iso8601_utc(now - days_ago * day);
    let records = vec![
        lifecycle_backup("a_new", "/etc/a", "/etc/.a-new", &stamp(2)),
        lifecycle_backup("a_old", "/etc/a", "/etc/.a-old", &stamp(40)),
        lifecycle_backup("b_new", "/etc/b", "/etc/.b-new", &stamp(1)),
        lifecycle_backup("b_old", "/etc/b", "/etc/.b-old", &stamp(80)),
    ];
    let ids = |records: &[FilesystemBackupRecord]| {
        records
            .iter()
            .map(|record| record.id.clone())
            .collect::<Vec<_>>()
    };

    // keepLatest keeps the newest per path, regardless of age.
    let (candidates, kept) = retention_candidates(records.clone(), Some(1), None, now);
    assert_eq!(ids(&candidates), vec!["a_old", "b_old"]);
    assert_eq!(kept, 2);

    // olderThanDays is an absolute age cutoff.
    let (candidates, kept) = retention_candidates(records.clone(), None, Some(30), now);
    assert_eq!(ids(&candidates), vec!["a_old", "b_old"]);
    assert_eq!(kept, 2);

    // Both filters intersect: a record must be beyond the keep count AND old enough.
    let (candidates, _) = retention_candidates(records, Some(1), Some(30), now);
    assert_eq!(ids(&candidates), vec!["a_old", "b_old"]);
}

#[test]
fn cleanup_input_resolves_single_and_batch_and_rejects_mixed_forms() {
    let item = |path: &str, backup: &str| json!({ "path": path, "backupPath": backup, "expectedRevision": "a".repeat(64) });
    let single: FilesystemBackupCleanupInput =
        serde_json::from_value(item("/etc/app.env", "/etc/.b1")).expect("single parses");
    assert!(matches!(
        single.resolve(),
        Ok(BackupCleanupRequest::Single(_))
    ));

    let batch: FilesystemBackupCleanupInput = serde_json::from_value(json!({
        "items": [item("/etc/app.env", "/etc/.b1"), item("/etc/app.env", "/etc/.b2")],
    }))
    .expect("batch parses");
    match batch.resolve() {
        Ok(BackupCleanupRequest::Batch(items)) => assert_eq!(items.len(), 2),
        other => panic!("expected a batch, got {other:?}"),
    }

    let mixed: FilesystemBackupCleanupInput = serde_json::from_value(json!({
        "path": "/etc/app.env",
        "items": [item("/etc/app.env", "/etc/.b1")],
    }))
    .expect("mixed parses but is refused by resolve");
    assert!(mixed.resolve().is_err());

    let empty: FilesystemBackupCleanupInput =
        serde_json::from_value(json!({ "items": [] })).expect("empty batch parses");
    assert!(empty.resolve().is_err());

    let duplicate: FilesystemBackupCleanupInput = serde_json::from_value(json!({
        "items": [item("/etc/app.env", "/etc/.b1"), item("/etc/app.env", "/etc/.b1")],
    }))
    .expect("duplicate batch parses");
    assert!(duplicate.resolve().is_err());
}

#[tokio::test]
async fn backup_retention_handler_truncates_at_32_candidates() {
    let directory =
        std::env::temp_dir().join(format!("yukinal-host-retention-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    let state = AppState::bootstrap(&directory).expect("bootstrap host state");
    state
        .database
        .investigations()
        .create_task(&lifecycle_task())
        .expect("create task");
    let old =
        yukinal_time::iso8601_utc(yukinal_time::now_epoch_seconds().saturating_sub(40 * 86_400));
    for index in 0..40 {
        let record = lifecycle_backup(
            &format!("backup_{index}"),
            "/etc/a",
            &format!("/etc/.b{index}"),
            &old,
        );
        state
            .database
            .filesystem_backups()
            .insert(&record)
            .expect("insert backup");
    }
    // Eight fresh rows on the same path are never candidates under an age cutoff, so the
    // result must report them as kept rather than silently drop them.
    let recent = yukinal_time::iso8601_utc(yukinal_time::now_epoch_seconds());
    for index in 40..48 {
        let record = lifecycle_backup(
            &format!("backup_{index}"),
            "/etc/a",
            &format!("/etc/.b{index}"),
            &recent,
        );
        state
            .database
            .filesystem_backups()
            .insert(&record)
            .expect("insert recent backup");
    }

    let request = lifecycle_request("filesystem.backup.retention");
    let response = filesystem_backup_retention(
        &state,
        "srv_fixture",
        &json!({ "olderThanDays": 30 }),
        &request,
    )
    .await
    .expect("handler");
    assert_eq!(response["status"], json!("success"));
    let output = &response["output"];
    assert_eq!(
        output["candidates"].as_array().expect("candidates").len(),
        32
    );
    assert_eq!(output["truncated"], json!(true));
    assert_eq!(output["scannedCount"], json!(48));
    assert_eq!(output["keptCount"], json!(8));

    let mut unbound_request = lifecycle_request("filesystem.backup.retention");
    unbound_request.task_id = None;
    let unbound = filesystem_backup_retention(
        &state,
        "srv_fixture",
        &json!({ "pathPrefix": "/etc/a", "olderThanDays": 30 }),
        &unbound_request,
    )
    .await
    .expect("read-only retention does not need a durable task");
    assert_eq!(unbound["status"], json!("success"));

    let other_server = filesystem_backup_retention(
        &state,
        "srv_other",
        &json!({ "olderThanDays": 30 }),
        &request,
    )
    .await
    .expect("handler");
    assert_eq!(other_server["status"], json!("success"));
    assert_eq!(other_server["output"]["candidates"], json!([]));

    drop(state);
    let _ = std::fs::remove_dir_all(&directory);
}

#[tokio::test]
async fn cross_task_cleanup_requires_an_approval_bound_rotation_step() {
    let directory = std::env::temp_dir().join(format!(
        "yukinal-host-rotation-binding-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    let state = AppState::bootstrap(&directory).expect("bootstrap host state");
    state
        .database
        .investigations()
        .create_task(&lifecycle_task())
        .expect("create task");

    let items = vec![json!({
        "path": "/etc/app.env",
        "backupPath": "/etc/.b1",
        "expectedRevision": "a".repeat(64),
    })];
    let bindings = HashMap::from([(
        "items".to_string(),
        serde_json::to_string(&items).expect("json"),
    )]);
    let mut step = InvestigationPlanStep {
        id: "step_rotation".into(),
        ordinal: 0,
        kind: PlanStepKind::Action,
        title: "Rotate backups".into(),
        purpose: "Keep the ledger bounded".into(),
        allowed_tools: vec![FILESYSTEM_BACKUP_CLEANUP.into()],
        input_bindings: Some(bindings.clone()),
        idempotency: Some(PlanIdempotency::Conditional),
        risk_level: Some(RiskLevel::Medium),
        requires_baseline: Some(true),
        preconditions: Some(vec!["the ledger is available".into()]),
        verification_criteria: Some(vec!["the row is consumed".into()]),
        preview: Some("delete one host-owned backup".into()),
        rollback: Some("create a new backup".into()),
        target: None,
        evidence_ids: vec![],
        success_criteria: vec!["the row is consumed".into()],
        requires_approval: true,
        max_attempts: 1,
        attempts: 0,
        status: PlanStepStatus::Pending,
        started_at: None,
        ended_at: None,
        last_deviation: None,
    };
    let plan = InvestigationPlan {
        id: "plan_rotation".into(),
        task_id: "task_lifecycle".into(),
        revision: 1,
        status: PlanStatus::Active,
        created_at: "2026-09-20T00:00:00Z".into(),
        updated_at: "2026-09-20T00:00:00Z".into(),
        current_step_id: Some("step_rotation".into()),
        approval: None,
        observation_window: None,
        steps: vec![step.clone()],
    };
    state
        .database
        .investigations()
        .save_plan(&plan)
        .expect("save plan");

    let mut request = lifecycle_request("filesystem.backup.cleanup");
    request.plan_id = Some("plan_rotation".into());
    request.plan_step_id = Some("step_rotation".into());

    let matching = FilesystemBackupCleanupItemInput {
        path: "/etc/app.env".into(),
        backup_path: "/etc/.b1".into(),
        expected_revision: "a".repeat(64),
    };
    assert!(cross_task_cleanup_is_plan_bound(
        &state, &request, &matching
    ));

    let other_path = FilesystemBackupCleanupItemInput {
        path: "/etc/app.env".into(),
        backup_path: "/etc/.other".into(),
        expected_revision: "a".repeat(64),
    };
    assert!(!cross_task_cleanup_is_plan_bound(
        &state,
        &request,
        &other_path
    ));

    // A step that does not require approval can never authorise a cross-task delete.
    step.requires_approval = false;
    let mut unapproved = plan.clone();
    unapproved.revision = 2;
    unapproved.steps = vec![step];
    state
        .database
        .investigations()
        .save_plan(&unapproved)
        .expect("save unapproved plan");
    assert!(!cross_task_cleanup_is_plan_bound(
        &state, &request, &matching
    ));

    // A request with no plan binding is refused outright.
    let unbound = lifecycle_request("filesystem.backup.cleanup");
    assert!(!cross_task_cleanup_is_plan_bound(
        &state, &unbound, &matching
    ));

    drop(state);
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn a_rotation_action_step_is_valid_but_never_auto_approved() {
    let bindings = HashMap::from([(
        "items".to_string(),
        serde_json::to_string(
            &(0..32)
                .map(|index| {
                    let path = format!("/etc/file{index}.conf");
                    let backup_path = yukinal_filesystem::backup_path_for(&path, &"a".repeat(32))
                        .expect("backup path");
                    json!({
                        "path": path,
                        "backupPath": backup_path,
                        "expectedRevision": "a".repeat(64),
                    })
                })
                .collect::<Vec<_>>(),
        )
        .expect("json"),
    )]);
    let step = InvestigationPlanStep {
        id: "step_rotation".into(),
        ordinal: 0,
        kind: PlanStepKind::Action,
        title: "Rotate backups".into(),
        purpose: "Keep the ledger bounded".into(),
        allowed_tools: vec![FILESYSTEM_BACKUP_CLEANUP.into()],
        input_bindings: Some(bindings),
        idempotency: Some(PlanIdempotency::Conditional),
        risk_level: Some(RiskLevel::Medium),
        requires_baseline: Some(true),
        preconditions: Some(vec!["the ledger is available".into()]),
        verification_criteria: Some(vec!["the rows are consumed".into()]),
        preview: Some("delete 32 host-owned backups".into()),
        rollback: Some("create new backups".into()),
        target: None,
        evidence_ids: vec![],
        success_criteria: vec!["the rows are consumed".into()],
        requires_approval: true,
        max_attempts: 1,
        attempts: 0,
        status: PlanStepStatus::Pending,
        started_at: None,
        ended_at: None,
        last_deviation: None,
    };

    // The 32-item JSON binding fits the raised cap, so the step validates.
    assert!(validate_playbook_step(&step).is_none());
    // ... and a rotation step is never part of the restricted auto delegation.
    assert!(!task_allows_auto_medium_action(&lifecycle_task(), &step));
    // Even without `requiresApproval`, cleanup is not in the auto delegation list.
    let mut unapproved = step;
    unapproved.requires_approval = false;
    assert!(validate_playbook_step(&unapproved)
        .expect("rotation must stay user-approved")
        .contains("require approval"));
    assert!(!task_allows_auto_medium_action(
        &lifecycle_task(),
        &unapproved
    ));

    let mut too_many = unapproved;
    too_many.requires_approval = true;
    too_many.input_bindings = Some(HashMap::from([(
        "items".to_string(),
        serde_json::to_string(
            &(0..33)
                .map(|index| {
                    let path = format!("/etc/file{index}.conf");
                    let backup_path = yukinal_filesystem::backup_path_for(&path, &"a".repeat(32))
                        .expect("backup path");
                    json!({
                        "path": path,
                        "backupPath": backup_path,
                        "expectedRevision": "a".repeat(64),
                    })
                })
                .collect::<Vec<_>>(),
        )
        .expect("json"),
    )]));
    assert!(validate_playbook_step(&too_many)
        .expect("rotation item count must be bounded")
        .contains("between 1 and 32"));

    let mut relative = too_many;
    relative.input_bindings = Some(HashMap::from([(
        "items".to_string(),
        serde_json::to_string(&vec![json!({
            "path": "relative.conf",
            "backupPath": "/etc/.yukinal-backup",
            "expectedRevision": "a".repeat(64),
        })])
        .expect("json"),
    )]));
    assert!(validate_playbook_step(&relative)
        .expect("rotation paths must be absolute")
        .contains("invalid path"));
}
