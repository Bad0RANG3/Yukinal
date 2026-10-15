//! Host-side execution for bounded Agent tools (read-only plus permission-gated writes).
//!
//! The sidecar can request a tool, but it cannot query the credential store or choose an
//! unscoped SSH target. This module validates planned file actions and resolves their one-target
//! C++ execution context from Rust-owned state.
//!
//! The file tools (`filesystem.read` / `filesystem.write` / `filesystem.edit` /
//! `filesystem.backup` / `filesystem.restore` / `filesystem.backup.cleanup`) delegate to
//! `yukinal-filesystem`: path policy, byte caps and bounded decoding live there, and this module
//! only maps the capability's typed failures onto the host protocol's failure codes
//! (see [`filesystem_failure`]). Backup listing and retention planning run in Python; this
//! module exposes only bounded ledger reads after rechecking their task, target and plan.
//!
//! Agent filesystem mutations are approved by Python and get a narrowly scoped C++ SFTP target only
//! after this module revalidates their durable action plan. Backup ownership and restore/cleanup
//! consumption metadata remain in the host database. Listing and retention use bounded host
//! reads with Python formatting and planning. `filesystem.edit` is a guarded read-then-modify, `filesystem.backup` creates a
//! host-owned sibling copy, `filesystem.restore` publishes it only with a current revision guard,
//! and cleanup verifies exact backup metadata before consuming ownership. The capability's crate
//! docs state what these guarantees do **not** cover (SFTP has no compare-and-swap, so checks and
//! publication are multiple round trips apart).

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use crate::commands::files::remote_file_service;
use crate::commands::logs::ServerLogsInput;
use crate::commands::mcp;
use crate::commands::services::ServerServicesInput;
use crate::state::AppState;
use crate::state::{HostActionExecutionBinding, HostActionTicketError};
use rand::Rng as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tauri::AppHandle;
use tokio_util::sync::CancellationToken;
use yukinal_core::docker::{
    is_safe_container_ref, truncate_text, DockerLogsResult, DockerRestartResult, DEFAULT_LOG_TAIL,
    DEFAULT_RESTART_TIMEOUT, MAX_LOG_TAIL, MAX_RESTART_TIMEOUT,
};
use yukinal_core::package::{
    is_safe_package_ref, is_safe_package_version, is_supported_package_manager,
    PackageInstallResult, DEFAULT_PACKAGE_INSTALL_TIMEOUT, MAX_PACKAGE_INSTALL_TIMEOUT,
};
use yukinal_core::service::{
    is_safe_systemd_service_ref, SystemdRestartResult, DEFAULT_SYSTEMD_RESTART_TIMEOUT,
    MAX_SYSTEMD_RESTART_TIMEOUT,
};
use yukinal_database::models::{
    DecisionBrief, Environment, Evidence, EvidenceContentType, EvidenceKind,
    EvidenceRedactionStatus, FailureOptionAction, Finding, FindingKind, InvestigationArtifact,
    InvestigationFailureOption, InvestigationObservationWindow, InvestigationPermissionMode,
    InvestigationPlan, InvestigationPlanApproval, InvestigationPlanDeviation, InvestigationRun,
    InvestigationRunMode, InvestigationStep, InvestigationTarget, InvestigationTargetHost,
    InvestigationTask, ObservationWindowStatus, PlanApprovalStatus, PlanDeviationAction,
    PlanDeviationCode, PlanIdempotency, PlanStepKind, PlanStepStatus, RiskLevel, TaskArtifactKind,
    TaskArtifactStatus, TaskAutomationLevel, TaskFailureCode, TaskPhase, TaskStatus,
};
use yukinal_database::repositories::{
    EvidenceSearchQuery, FilesystemBackupRecord, FilesystemBackupStatus, HostToolCallClaim,
    HostToolCallInput, HostToolCallStatus, InvestigationRetentionPreview,
};
use yukinal_database::DatabaseError;
use yukinal_filesystem::{
    validate_remote_path, AgentBackupRequest, AgentCleanupBackupRequest, AgentEditRequest,
    AgentReadRequest, AgentRestoreRequest, AgentWriteRequest, Error as FilesystemError,
};

mod context;
mod evidence;
mod plan;
mod tools;

// 执行体与证据/计划/上下文处理分别在子模块；glob 带回根命名空间，分发按方法名直接调用。
use context::*;
use evidence::*;
use plan::*;
use tools::*;

const HOST_TOOL_EXECUTE: &str = "host.tool.execute";
const HOST_SSH_SERVICE_TARGET: &str = "host.ssh.service_target";
const HOST_SSH_FILESYSTEM_READ_TARGET: &str = "host.ssh.filesystem_read_target";
const HOST_SSH_FILESYSTEM_WRITE_TARGET: &str = "host.ssh.filesystem_write_target";
const HOST_SSH_FILESYSTEM_EDIT_TARGET: &str = "host.ssh.filesystem_edit_target";
const HOST_SSH_FILESYSTEM_BACKUP_TARGET: &str = "host.ssh.filesystem_backup_target";
const HOST_FILESYSTEM_BACKUP_REGISTER: &str = "host.investigation.filesystem_backup.register";
const HOST_SSH_FILESYSTEM_RESTORE_TARGET: &str = "host.ssh.filesystem_restore_target";
const HOST_FILESYSTEM_BACKUP_RESTORED: &str = "host.investigation.filesystem_backup.restored";
const HOST_SSH_FILESYSTEM_CLEANUP_TARGET: &str = "host.ssh.filesystem_cleanup_target";
const HOST_FILESYSTEM_BACKUP_DELETED: &str = "host.investigation.filesystem_backup.deleted";
const HOST_FILESYSTEM_BACKUP_LIST_QUERY: &str = "host.filesystem.backup.list_query";
const HOST_FILESYSTEM_BACKUP_RETENTION_SCAN: &str = "host.filesystem.backup.retention_scan";
const HOST_SSH_EFFECTFUL_ACTION_TARGET: &str = "host.ssh.effectful_action_target";
const HOST_SSH_EFFECTFUL_ACTION_FINISHED: &str = "host.ssh.effectful_action_finished";
const HOST_CONTEXT_FETCH: &str = "host.context.fetch";
const HOST_EVIDENCE_RECORD: &str = "host.investigation.evidence.record";
const HOST_EVIDENCE_FETCH: &str = "host.investigation.evidence.fetch";
const HOST_EVIDENCE_SEARCH: &str = "host.investigation.evidence.search";
const HOST_EVIDENCE_CORRELATE: &str = "host.investigation.evidence.correlate";
const HOST_EVIDENCE_COMPARE: &str = "host.investigation.evidence.compare";
const HOST_RETENTION_PREVIEW: &str = "host.investigation.retention.preview";
const HOST_FINDING_RECORD: &str = "host.investigation.finding.record";
const HOST_BRIEF_RECORD: &str = "host.investigation.brief.record";
const HOST_PLAN_RECORD: &str = "host.investigation.plan.record";
const HOST_PLAN_CHECK: &str = "host.investigation.plan.check";
const HOST_PLAN_STEP_RESULT: &str = "host.investigation.plan.step_result";
const HOST_ARTIFACT_RECORD: &str = "host.investigation.artifact.record";
/// `host.mcp.catalog`（ADR 0014）：启用的 MCP 服务器 + 它们的工具描述符。
pub(crate) const HOST_MCP_CATALOG: &str = "host.mcp.catalog";
pub(crate) const HOST_TOOL_CANCEL: &str = "host.tool.cancel";
const SERVER_INFO: &str = "server.info";
const SERVER_EXEC: &str = "server.exec";
const SERVER_LOGS: &str = "server.logs";
const SERVER_SERVICES: &str = "server.services";
const DOCKER_PS: &str = "docker.ps";
const DOCKER_LOGS: &str = "docker.logs";
const DOCKER_INSPECT: &str = "docker.inspect";
const DOCKER_RESTART: &str = "docker.restart";
const SYSTEMD_INSPECT: &str = "systemd.inspect";
const SYSTEMD_RESTART: &str = "systemd.restart";
const PACKAGE_INSPECT: &str = "package.inspect";
const PACKAGE_INSTALL: &str = "package.install";
const FILESYSTEM_READ: &str = "filesystem.read";
const FILESYSTEM_WRITE: &str = "filesystem.write";
const FILESYSTEM_EDIT: &str = "filesystem.edit";
const FILESYSTEM_BACKUP: &str = "filesystem.backup";
const FILESYSTEM_BACKUP_LIST: &str = "filesystem.backup.list";
const FILESYSTEM_BACKUP_RETENTION: &str = "filesystem.backup.retention";
const FILESYSTEM_BACKUP_CLEANUP: &str = "filesystem.backup.cleanup";
const FILESYSTEM_RESTORE: &str = "filesystem.restore";
const MAX_HOST_TOOL_REPLAY_CACHE: usize = 256;
const MAX_EVIDENCE_SEARCH_LIMIT: usize = 64;
const DEFAULT_EVIDENCE_CORRELATION_WINDOW_SECONDS: u64 = 300;
const MAX_EVIDENCE_CORRELATION_WINDOW_SECONDS: u64 = 3_600;
const DEFAULT_RETENTION_DAYS: u64 = 30;
const DEFAULT_RETENTION_LIMIT: usize = 64;
const MAX_RETENTION_LIMIT: usize = 128;
/// Evidence is a point-in-time observation. The host exposes this conservative,
/// versioned policy as metadata; it does not silently delete or mutate old rows.
const EVIDENCE_FRESHNESS_STALE_AFTER_SECONDS: u64 = 15 * 60;
const EVIDENCE_FRESHNESS_EXPIRES_AFTER_SECONDS: u64 = 24 * 60 * 60;
const EVIDENCE_FRESHNESS_POLICY: &str = "default-v1";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostToolExecuteRequest {
    call_id: String,
    run_id: Option<String>,
    trace_id: String,
    tool_name: String,
    input: Value,
    target: HostToolTarget,
    task_id: Option<String>,
    plan_id: Option<String>,
    plan_step_id: Option<String>,
    evidence_ids: Option<Vec<String>>,
    approval_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostSshFilesystemWriteTargetRequest {
    server_id: String,
    path: String,
    content: String,
    task_id: String,
    plan_id: String,
    plan_step_id: String,
    evidence_ids: Vec<String>,
    target: HostToolTarget,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostSshFilesystemEditTargetRequest {
    server_id: String,
    path: String,
    expected_revision: String,
    old_string: String,
    new_string: String,
    task_id: String,
    plan_id: String,
    plan_step_id: String,
    evidence_ids: Vec<String>,
    target: HostToolTarget,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostSshFilesystemBackupTargetRequest {
    server_id: String,
    path: String,
    task_id: String,
    plan_id: String,
    plan_step_id: String,
    evidence_ids: Vec<String>,
    target: HostToolTarget,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostFilesystemBackupRegisterRequest {
    server_id: String,
    path: String,
    backup_path: String,
    revision: String,
    bytes_backed_up: usize,
    task_id: String,
    plan_id: String,
    plan_step_id: String,
    trace_id: String,
    call_id: String,
    evidence_ids: Vec<String>,
    target: HostToolTarget,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostSshFilesystemRestoreTargetRequest {
    server_id: String,
    path: String,
    backup_path: String,
    expected_revision: String,
    task_id: String,
    plan_id: String,
    plan_step_id: String,
    evidence_ids: Vec<String>,
    target: HostToolTarget,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostFilesystemBackupRestoredRequest {
    server_id: String,
    path: String,
    backup_path: String,
    expected_revision: String,
    revision: String,
    bytes_before: usize,
    bytes_after: usize,
    task_id: String,
    plan_id: String,
    plan_step_id: String,
    trace_id: String,
    call_id: String,
    evidence_ids: Vec<String>,
    target: HostToolTarget,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostSshFilesystemCleanupTargetRequest {
    server_id: String,
    input: Value,
    task_id: String,
    plan_id: String,
    plan_step_id: String,
    run_id: String,
    trace_id: String,
    call_id: String,
    evidence_ids: Vec<String>,
    target: HostToolTarget,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostFilesystemBackupDeletedRequest {
    server_id: String,
    input: Value,
    item: FilesystemBackupCleanupItemInput,
    revision: String,
    bytes_deleted: usize,
    task_id: String,
    plan_id: String,
    plan_step_id: String,
    trace_id: String,
    call_id: String,
    evidence_ids: Vec<String>,
    target: HostToolTarget,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostFilesystemBackupListQueryRequest {
    tool_call: HostToolExecuteRequest,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostFilesystemBackupRetentionScanRequest {
    tool_call: HostToolExecuteRequest,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostSshEffectfulActionTargetRequest {
    tool_call: HostToolExecuteRequest,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostSshEffectfulActionFinishedRequest {
    tool_call: HostToolExecuteRequest,
    outcome: String,
    #[serde(default)]
    result: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostServerExecResult {
    state: String,
    exit_code: i32,
    stdout: String,
    stderr: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
    duration_ms: u64,
}

#[derive(Debug, Clone)]
struct HostToolCallToken {
    key: (String, String),
}

#[derive(Debug)]
enum HostToolCallDecision {
    Execute(Option<HostToolCallToken>),
    Respond(Value),
}

/// Only actions whose remote effect cannot be safely inferred from a failed
/// response use the durable call ledger. Ordinary reads remain retryable. A
/// read bound to a concrete durable plan step is the exception: the same
/// logical sample must not be sent to the target twice before the plan can
/// decide whether it produced new evidence. MCP is guarded wholesale because
/// a third-party tool may mutate state and its annotations do not define a
/// trustworthy risk tier unless the user explicitly trusts that one server (ADR 0014,
/// revised by ADR 0074).
async fn requires_host_tool_idempotency(
    state: &AppState,
    request: &HostToolExecuteRequest,
) -> Result<bool, String> {
    if is_effectful_host_tool(state, &request.tool_name).await {
        return Ok(true);
    }
    let is_planned_observation = request.task_id.is_some()
        && request.plan_id.is_some()
        && request.plan_step_id.is_some()
        && is_plan_bound_observation_tool(&request.tool_name);
    if !is_planned_observation {
        return Ok(false);
    }

    // Observation windows intentionally reuse one verification step while the
    // host advances its sample clock. The interval/deadline gate in
    // `host.investigation.plan.step_result` is the dedupe boundary for those
    // samples, so the ordinary one-call-per-plan-step fence must not reject a
    // legitimate later sample.
    if let (Some(task_id), Some(plan_step_id)) =
        (request.task_id.as_deref(), request.plan_step_id.as_deref())
    {
        if let Some(plan) = state
            .database
            .investigations()
            .latest_plan(task_id)
            .map_err(|error| error.to_string())?
        {
            if plan_allows_repeated_observation(&plan, plan_step_id) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn plan_allows_repeated_observation(plan: &InvestigationPlan, plan_step_id: &str) -> bool {
    plan.current_step_id.as_deref() == Some(plan_step_id)
        && plan
            .steps
            .iter()
            .find(|step| step.id == plan_step_id)
            .is_some_and(|step| step.kind == PlanStepKind::Verification)
        && plan
            .observation_window
            .as_ref()
            .is_some_and(|window| window.status == ObservationWindowStatus::Running)
}

/// Whether this host tool needs the durable idempotency ledger and a ChangePlan.
///
/// Built-in state-changing tools are effectful by name. MCP tools are effectful unless
/// the host can resolve one of them to `low` (ADR 0074): the default is still
/// `critical`, and anything the host cannot resolve — a stopped server, a name that no
/// running catalog provides, a read that fails — stays effectful. This fails closed
/// where the old wholesale rule (ADR 0014) was simply always-true.
async fn is_effectful_host_tool(state: &AppState, tool_name: &str) -> bool {
    if matches!(
        tool_name,
        DOCKER_RESTART
            | SERVER_EXEC
            | SYSTEMD_RESTART
            | PACKAGE_INSTALL
            | FILESYSTEM_WRITE
            | FILESYSTEM_EDIT
            | FILESYSTEM_BACKUP
            | FILESYSTEM_BACKUP_CLEANUP
            | FILESYSTEM_RESTORE
    ) {
        return true;
    }
    if mcp::is_mcp_tool_name(tool_name) {
        // Only a host-resolved `low` tool is non-effectful. Everything else — including
        // `None` ("cannot resolve") and `high`/`critical` — is effectful.
        return mcp::tool_effective_risk(state, tool_name).await != Some("low");
    }
    false
}

/// Every Agent-side effectful call must carry all three durable identifiers.
///
/// The interactive terminal is deliberately outside this function: it is a user-operated
/// manual session, not an Agent tool, and therefore cannot become an accidental autonomous
/// write path by omitting plan metadata.
async fn effectful_tool_requires_durable_plan(
    state: &AppState,
    request: &HostToolExecuteRequest,
) -> bool {
    is_effectful_host_tool(state, &request.tool_name).await
        && !(request
            .task_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            && request
                .plan_id
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            && request
                .plan_step_id
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()))
}

fn is_plan_bound_observation_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        SERVER_INFO
            | SERVER_LOGS
            | SERVER_SERVICES
            | DOCKER_PS
            | DOCKER_LOGS
            | DOCKER_INSPECT
            | SYSTEMD_INSPECT
            | PACKAGE_INSPECT
            | FILESYSTEM_READ
            | FILESYSTEM_BACKUP_LIST
            | FILESYSTEM_BACKUP_RETENTION
    )
}

fn host_tool_request_fingerprint(request: &HostToolExecuteRequest) -> Result<String, String> {
    let canonical = json!({
        "runId": request.run_id,
        "traceId": request.trace_id,
        "callId": request.call_id,
        "toolName": request.tool_name,
        "input": request.input,
        "target": {
            "host": request.target.host,
            "serverId": request.target.server_id,
            "workspaceId": request.target.workspace_id,
            "environment": request.target.environment,
        },
        "taskId": request.task_id,
        "planId": request.plan_id,
        "planStepId": request.plan_step_id,
        "evidenceIds": request.evidence_ids,
    });
    let bytes = serde_json::to_vec(&canonical).map_err(|error| error.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

/// Match the sidecar's `actionFingerprint` for JSON inputs. `serde_json::Map`
/// uses lexicographically ordered keys, as does the Agent's UTF-8 canonicalizer.
pub(crate) fn host_tool_input_fingerprint(input: &Value) -> Result<String, String> {
    fn canonicalize(value: &Value) -> Value {
        match value {
            Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
            Value::Object(fields) => {
                let mut ordered = serde_json::Map::new();
                let mut keys = fields.keys().collect::<Vec<_>>();
                keys.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
                for key in keys {
                    if let Some(value) = fields.get(key) {
                        ordered.insert(key.clone(), canonicalize(value));
                    }
                }
                Value::Object(ordered)
            }
            scalar => scalar.clone(),
        }
    }

    let bytes = serde_json::to_vec(&canonicalize(input)).map_err(|error| error.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

/// Fingerprint the logical plan action without provider-generated identities.
/// It is only used when the request is bound to a durable task plan; ordinary
/// chat calls remain scoped to their exact `(traceId, callId)` pair.
fn host_tool_action_fingerprint(request: &HostToolExecuteRequest) -> Result<String, String> {
    let canonical = json!({
        "toolName": request.tool_name,
        "input": request.input,
        "target": {
            "host": request.target.host,
            "serverId": request.target.server_id,
            "workspaceId": request.target.workspace_id,
            "environment": request.target.environment,
        },
        "taskId": request.task_id,
        "planId": request.plan_id,
        "planStepId": request.plan_step_id,
        "evidenceIds": request.evidence_ids,
    });
    let bytes = serde_json::to_vec(&canonical).map_err(|error| error.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

async fn prepare_host_tool_call(
    state: &AppState,
    request: &HostToolExecuteRequest,
) -> Result<HostToolCallDecision, String> {
    if !requires_host_tool_idempotency(state, request).await? {
        return Ok(HostToolCallDecision::Execute(None));
    }
    if request.call_id.trim().is_empty() || request.trace_id.trim().is_empty() {
        return Ok(HostToolCallDecision::Respond(failed(
            "invalid_input",
            "guarded host tool requests require non-empty traceId and callId",
            false,
            None,
        )));
    }

    let fingerprint = host_tool_request_fingerprint(request)?;
    let action_fingerprint = match (
        request.task_id.as_deref(),
        request.plan_id.as_deref(),
        request.plan_step_id.as_deref(),
    ) {
        (Some(_), Some(_), Some(_)) => Some(host_tool_action_fingerprint(request)?),
        _ => None,
    };
    let claim = state
        .database
        .host_tool_calls()
        .claim(HostToolCallInput {
            trace_id: &request.trace_id,
            call_id: &request.call_id,
            task_id: request.task_id.as_deref(),
            plan_id: request.plan_id.as_deref(),
            plan_step_id: request.plan_step_id.as_deref(),
            tool_name: &request.tool_name,
            request_fingerprint: &fingerprint,
            action_fingerprint: action_fingerprint.as_deref(),
            started_at: &yukinal_core::sidecar::iso8601_now(),
        })
        .map_err(|error| error.to_string())?;
    let key = (request.trace_id.clone(), request.call_id.clone());
    match claim {
        HostToolCallClaim::Claimed => Ok(HostToolCallDecision::Execute(Some(
            HostToolCallToken { key },
        ))),
        HostToolCallClaim::Existing {
            request_fingerprint,
            status,
        } if request_fingerprint != fingerprint => Ok(HostToolCallDecision::Respond(
            failed(
                "plan_deviation",
                "callId was already used for a different host action; refusing to guess which request is authoritative",
                false,
                Some(json!({
                    "code": PlanDeviationCode::DuplicateCall,
                    "action": PlanDeviationAction::Deny,
                    "status": status.as_str(),
                })),
            ),
        )),
        HostToolCallClaim::Existing { status, .. } => {
            let cached = state
                .host_tool_replays
                .lock()
                .map_err(|_| "host tool replay cache is poisoned".to_string())?
                .get(&key)
                .cloned();
            if let Some(response) = cached {
                return Ok(HostToolCallDecision::Respond(response));
            }
            Ok(HostToolCallDecision::Respond(failed(
                "plan_deviation",
                "this host action was already started, but its response is no longer available; reconcile the target before issuing a new call",
                false,
                Some(json!({
                    "code": PlanDeviationCode::DuplicateCall,
                    "action": PlanDeviationAction::WaitUser,
                    "status": status.as_str(),
                    "traceId": request.trace_id,
                    "callId": request.call_id,
                })),
            )))
        }
    }
}

async fn consume_python_host_action_approval(
    state: &AppState,
    request: &HostToolExecuteRequest,
) -> Result<(), HostActionTicketError> {
    if !matches!(
        request.tool_name.as_str(),
        DOCKER_RESTART | SYSTEMD_RESTART | PACKAGE_INSTALL
    ) {
        return Ok(());
    }
    consume_host_action_approval(state, request).await
}

async fn consume_effectful_ssh_action_approval(
    state: &AppState,
    request: &HostToolExecuteRequest,
) -> Result<(), HostActionTicketError> {
    if !matches!(
        request.tool_name.as_str(),
        DOCKER_RESTART | SYSTEMD_RESTART | PACKAGE_INSTALL | SERVER_EXEC
    ) {
        return Ok(());
    }
    consume_host_action_approval(state, request).await
}

async fn consume_host_action_approval(
    state: &AppState,
    request: &HostToolExecuteRequest,
) -> Result<(), HostActionTicketError> {
    let Some(approval_id) = request.approval_id.as_deref() else {
        return Err(HostActionTicketError::Missing);
    };
    let input_fingerprint =
        host_tool_input_fingerprint(&request.input).map_err(|_| HostActionTicketError::Mismatch)?;
    let binding = HostActionExecutionBinding {
        approval_id,
        run_id: request.run_id.as_deref(),
        trace_id: &request.trace_id,
        call_id: &request.call_id,
        tool_name: &request.tool_name,
        input_fingerprint: &input_fingerprint,
        target_host: &request.target.host,
        server_id: request.target.server_id.as_deref(),
        workspace_id: request.target.workspace_id.as_deref(),
        environment: request.target.environment.as_str(),
        task_id: request.task_id.as_deref(),
        plan_id: request.plan_id.as_deref(),
        plan_step_id: request.plan_step_id.as_deref(),
        evidence_ids: request.evidence_ids.clone(),
    };
    state
        .host_action_approvals
        .consume(binding, yukinal_time::now_epoch_seconds)
        .await
}

fn host_action_approval_failure(error: HostActionTicketError) -> Value {
    let message = match error {
        HostActionTicketError::Missing => "host action has no host-issued approval ticket",
        HostActionTicketError::Expired => "host action approval ticket has expired",
        HostActionTicketError::Mismatch => {
            "host action does not match the approved run, call, target, plan, or input"
        }
        HostActionTicketError::NotApproved => "host action approval has not been accepted",
    };
    failed(
        "denied_by_policy",
        message,
        false,
        Some(json!({ "code": "approval_ticket" })),
    )
}

fn host_tool_call_status(response: &Result<Value, String>) -> HostToolCallStatus {
    let Ok(response) = response else {
        // A protocol/transport error gives no proof that a remote action did
        // not happen. Treat it as uncertain so the next call cannot replay it.
        return HostToolCallStatus::Uncertain;
    };
    match response.get("status").and_then(Value::as_str) {
        Some("success") => HostToolCallStatus::Success,
        Some("cancelled") => HostToolCallStatus::Uncertain,
        Some("failed") => match response
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(Value::as_str)
        {
            Some("transport") | Some("timeout") | Some("cancelled") | Some("execution_failed") => {
                HostToolCallStatus::Uncertain
            }
            _ => HostToolCallStatus::Failed,
        },
        _ => HostToolCallStatus::Uncertain,
    }
}

fn finish_host_tool_call(
    state: &AppState,
    request: &HostToolExecuteRequest,
    token: Option<HostToolCallToken>,
    response: &Result<Value, String>,
) -> Result<(), String> {
    let Some(token) = token else {
        return Ok(());
    };
    let status = host_tool_call_status(response);
    state
        .database
        .host_tool_calls()
        .finish(
            &request.trace_id,
            &request.call_id,
            status,
            &yukinal_core::sidecar::iso8601_now(),
        )
        .map_err(|error| error.to_string())?;

    // Never persist raw remote output in SQLite. A successful or deterministic
    // validation failure may be replayed only while this host process still has
    // the response in its bounded cache. Uncertain outcomes intentionally are
    // not cached.
    if status != HostToolCallStatus::Uncertain {
        if let Ok(value) = response {
            let mut cache = state
                .host_tool_replays
                .lock()
                .map_err(|_| "host tool replay cache is poisoned".to_string())?;
            if cache.len() >= MAX_HOST_TOOL_REPLAY_CACHE && !cache.contains_key(&token.key) {
                if let Some(old_key) = cache.keys().next().cloned() {
                    cache.remove(&old_key);
                }
            }
            cache.insert(token.key, value.clone());
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostToolCancelRequest {
    request_id: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostEvidenceFetchRequest {
    task_id: String,
    evidence_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostEvidenceSearchRequest {
    task_id: String,
    source_tool: Option<String>,
    kind: Option<EvidenceKind>,
    from: Option<String>,
    to: Option<String>,
    target: Option<HostToolTarget>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostEvidenceCorrelationRequest {
    task_id: String,
    anchor_evidence_id: String,
    window_seconds: Option<u64>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostEvidenceCompareRequest {
    task_id: String,
    left_evidence_id: String,
    right_evidence_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostRetentionPreviewRequest {
    task_id: String,
    cutoff_at: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EvidenceSummary {
    id: String,
    task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<String>,
    scope: yukinal_database::models::InvestigationTarget,
    kind: EvidenceKind,
    source_tool: String,
    collected_at: String,
    input_summary: String,
    content_type: EvidenceContentType,
    content_hash: String,
    truncated: bool,
    redaction_status: EvidenceRedactionStatus,
    freshness: EvidenceFreshness,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EvidenceTextComparison {
    left_line_count: usize,
    right_line_count: usize,
    changed_line_count: usize,
    added_line_count: usize,
    removed_line_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EvidenceComparison {
    status: &'static str,
    shape: &'static str,
    left: EvidenceSummary,
    right: EvidenceSummary,
    changed_paths: Vec<String>,
    changed_path_count: usize,
    diff_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<EvidenceTextComparison>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EvidenceFreshness {
    status: &'static str,
    policy: &'static str,
    evaluated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    age_seconds: Option<u64>,
    stale_after_seconds: u64,
    expires_after_seconds: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

pub(crate) fn evidence_freshness_at(
    collected_at: &str,
    evaluated_at_epoch: u64,
) -> EvidenceFreshness {
    let evaluated_at = yukinal_time::iso8601_utc(evaluated_at_epoch);
    let base = |status, age_seconds, reason| EvidenceFreshness {
        status,
        policy: EVIDENCE_FRESHNESS_POLICY,
        evaluated_at: evaluated_at.clone(),
        age_seconds,
        stale_after_seconds: EVIDENCE_FRESHNESS_STALE_AFTER_SECONDS,
        expires_after_seconds: EVIDENCE_FRESHNESS_EXPIRES_AFTER_SECONDS,
        reason,
    };
    let Some(collected_epoch) = yukinal_time::parse_iso8601_utc(collected_at) else {
        return base(
            "unknown",
            None,
            Some("collection timestamp is not a valid UTC ISO-8601 value".into()),
        );
    };
    if collected_epoch > evaluated_at_epoch {
        return base(
            "unknown",
            None,
            Some("collection timestamp is in the future relative to the host clock".into()),
        );
    }
    let age_seconds = evaluated_at_epoch.saturating_sub(collected_epoch);
    let status = if age_seconds <= EVIDENCE_FRESHNESS_STALE_AFTER_SECONDS {
        "fresh"
    } else if age_seconds <= EVIDENCE_FRESHNESS_EXPIRES_AFTER_SECONDS {
        "stale"
    } else {
        "expired"
    };
    base(status, Some(age_seconds), None)
}

pub(crate) fn evidence_json_with_freshness_at(
    evidence: &Evidence,
    evaluated_at_epoch: u64,
) -> Value {
    let mut value = serde_json::to_value(evidence).expect("evidence model must serialize");
    if let Value::Object(object) = &mut value {
        object.insert(
            "freshness".into(),
            serde_json::to_value(evidence_freshness_at(
                &evidence.collected_at,
                evaluated_at_epoch,
            ))
            .expect("evidence freshness must serialize"),
        );
    }
    value
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostPlanCheckRequest {
    task_id: String,
    tool_name: String,
    input: Value,
    target: HostToolTarget,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostPlanStepResultRequest {
    task_id: String,
    plan_id: String,
    step_id: String,
    status: String,
    retryable: bool,
    output_summary: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostArtifactRecordRequest {
    artifact: InvestigationArtifact,
    plan_id: Option<String>,
    plan_step_id: Option<String>,
    evidence_ids: Option<Vec<String>>,
}

pub(crate) type HostCancellationRegistry = Arc<Mutex<HashMap<i64, CancellationToken>>>;

/// Cancel a host request that is already executing. The response is deliberately
/// best-effort: a request may have completed between the Agent's abort and this
/// frame arriving at the host.
pub(crate) fn cancel_sidecar_request(
    registry: &HostCancellationRegistry,
    params: Value,
) -> Result<Value, String> {
    let request = serde_json::from_value::<HostToolCancelRequest>(params)
        .map_err(|error| format!("invalid host cancellation request: {error}"))?;
    if request.request_id <= 0 {
        return Err("host cancellation request id must be positive".to_string());
    }
    let token = registry
        .lock()
        .map_err(|_| "host cancellation registry is poisoned".to_string())?
        .remove(&request.request_id);
    let cancelled = token.is_some();
    if let Some(token) = token {
        token.cancel();
    }
    Ok(json!({ "cancelled": cancelled }))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostToolTarget {
    host: String,
    server_id: Option<String>,
    workspace_id: Option<String>,
    environment: Environment,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostContextRequest {
    kind: HostContextKind,
    id: String,
}

/// Context is deliberately narrower than the desktop task detail response. The
/// Agent needs evidence and artifact metadata to orient itself, not their bodies;
/// those remain behind explicit, task-scoped evidence fetches.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InvestigationEvidenceSummary {
    id: String,
    task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<String>,
    scope: InvestigationTarget,
    kind: EvidenceKind,
    source_tool: String,
    collected_at: String,
    input_summary: String,
    content_type: EvidenceContentType,
    content_hash: String,
    truncated: bool,
    redaction_status: EvidenceRedactionStatus,
    freshness: EvidenceFreshness,
}

impl From<Evidence> for InvestigationEvidenceSummary {
    fn from(evidence: Evidence) -> Self {
        Self::from_at(evidence, yukinal_time::now_epoch_seconds())
    }
}

impl InvestigationEvidenceSummary {
    fn from_at(evidence: Evidence, evaluated_at_epoch: u64) -> Self {
        let freshness = evidence_freshness_at(&evidence.collected_at, evaluated_at_epoch);
        Self {
            id: evidence.id,
            task_id: evidence.task_id,
            run_id: evidence.run_id,
            scope: evidence.scope,
            kind: evidence.kind,
            source_tool: evidence.source_tool,
            collected_at: evidence.collected_at,
            input_summary: evidence.input_summary,
            content_type: evidence.content_type,
            content_hash: evidence.content_hash,
            truncated: evidence.truncated,
            redaction_status: evidence.redaction_status,
            freshness,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InvestigationArtifactSummary {
    id: String,
    task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plan_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plan_step_id: Option<String>,
    phase: TaskPhase,
    kind: TaskArtifactKind,
    status: TaskArtifactStatus,
    title: String,
    summary: String,
    evidence_ids: Vec<String>,
    created_at: String,
    updated_at: String,
}

impl From<InvestigationArtifact> for InvestigationArtifactSummary {
    fn from(artifact: InvestigationArtifact) -> Self {
        Self {
            id: artifact.id,
            task_id: artifact.task_id,
            run_id: artifact.run_id,
            plan_id: artifact.plan_id,
            plan_step_id: artifact.plan_step_id,
            phase: artifact.phase,
            kind: artifact.kind,
            status: artifact.status,
            title: artifact.title,
            summary: artifact.summary,
            evidence_ids: artifact.evidence_ids,
            created_at: artifact.created_at,
            updated_at: artifact.updated_at,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InvestigationContextResponse {
    task: InvestigationTask,
    evidence: Vec<InvestigationEvidenceSummary>,
    findings: Vec<Finding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    decision_brief: Option<DecisionBrief>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plan: Option<InvestigationPlan>,
    artifacts: Vec<InvestigationArtifactSummary>,
    runs: Vec<InvestigationRun>,
    steps: Vec<InvestigationStep>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum HostContextKind {
    Server,
    Snapshot,
    Workspace,
    Investigation,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DockerPsInput {
    all: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DockerLogsInput {
    container: String,
    tail: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DockerInspectInput {
    container: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DockerRestartInput {
    container: String,
    timeout_seconds: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SystemdInspectInput {
    service: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SystemdRestartInput {
    service: String,
    timeout_seconds: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PackageInspectInput {
    manager: String,
    package: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PackageInstallInput {
    manager: String,
    package: String,
    version: Option<String>,
    timeout_seconds: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FilesystemReadInput {
    path: String,
    max_bytes: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FilesystemWriteInput {
    path: String,
    content: String,
}

/// `filesystem.edit` 的宿主入参。`expectedRevision` 是 `filesystem.read` 返回的那个 revision，
/// `oldString` 必须**恰好一次**出现在文件里。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FilesystemEditInput {
    path: String,
    expected_revision: String,
    old_string: String,
    new_string: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FilesystemBackupInput {
    path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FilesystemBackupListInput {
    status: Option<String>,
    path: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FilesystemBackupRetentionInput {
    path_prefix: Option<String>,
    keep_latest: Option<usize>,
    older_than_days: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FilesystemBackupCleanupItemInput {
    path: String,
    backup_path: String,
    expected_revision: String,
}

/// The single-item shape and the batch shape share one struct so the dispatcher can tell
/// them apart and reject a call that mixes both (ADR 0076). A union is enforced here rather
/// than in serde so the error message can name the problem.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FilesystemBackupCleanupInput {
    path: Option<String>,
    backup_path: Option<String>,
    expected_revision: Option<String>,
    items: Option<Vec<FilesystemBackupCleanupItemInput>>,
}

#[derive(Debug)]
enum BackupCleanupRequest {
    Single(FilesystemBackupCleanupItemInput),
    Batch(Vec<FilesystemBackupCleanupItemInput>),
}

impl FilesystemBackupCleanupInput {
    fn resolve(self) -> Result<BackupCleanupRequest, String> {
        match self {
            Self {
                items: Some(items),
                path: None,
                backup_path: None,
                expected_revision: None,
            } => {
                if items.is_empty() || items.len() > 32 {
                    return Err(
                        "filesystem.backup.cleanup items must contain between 1 and 32 entries"
                            .to_string(),
                    );
                }
                let mut seen = BTreeSet::new();
                for item in &items {
                    if !seen.insert((item.path.as_str(), item.backup_path.as_str())) {
                        return Err(
                            "filesystem.backup.cleanup items must be unique by path and backupPath"
                                .to_string(),
                        );
                    }
                }
                Ok(BackupCleanupRequest::Batch(items))
            }
            Self {
                items: None,
                path: Some(path),
                backup_path: Some(backup_path),
                expected_revision: Some(expected_revision),
            } => Ok(BackupCleanupRequest::Single(
                FilesystemBackupCleanupItemInput {
                    path,
                    backup_path,
                    expected_revision,
                },
            )),
            _ => Err(
                "filesystem.backup.cleanup accepts either one exact backup (path, backupPath, \
                 expectedRevision) or a bounded items array, never both"
                    .to_string(),
            ),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FilesystemRestoreInput {
    path: String,
    backup_path: String,
    expected_revision: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FilesystemReadResult {
    path: String,
    content: String,
    truncated: bool,
    /// The content revision of the bytes that were read (SHA-256, lowercase hex).
    revision: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FilesystemWriteResult {
    path: String,
    bytes_written: usize,
}

/// `filesystem.edit` 的宿主返回：写回之后的 revision 与一个小结。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FilesystemEditResult {
    path: String,
    revision: String,
    bytes_before: usize,
    bytes_after: usize,
    line_delta: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FilesystemBackupResult {
    path: String,
    backup_path: String,
    revision: String,
    bytes_backed_up: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FilesystemBackupCleanupBatchItemResult {
    path: String,
    backup_path: String,
    /// `removed` | `skipped` | `failed`.
    outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes_deleted: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FilesystemBackupCleanupBatchResult {
    items: Vec<FilesystemBackupCleanupBatchItemResult>,
    partial: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FilesystemBackupCleanupResult {
    path: String,
    backup_path: String,
    revision: String,
    bytes_deleted: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FilesystemRestoreResult {
    path: String,
    backup_path: String,
    revision: String,
    bytes_before: usize,
    bytes_after: usize,
}

/// Host request entry point with a cancellation token owned by the sidecar
/// dispatcher. Keeping the token explicit lets cancellation reach whichever SSH
/// transport owns the active operation.
#[cfg(test)]
pub(crate) async fn handle_sidecar_request_with_cancel(
    state: &AppState,
    method: &str,
    params: Value,
    cancel: CancellationToken,
) -> Result<Value, String> {
    handle_sidecar_request_with_context(state, None, method, params, cancel).await
}

pub(crate) async fn handle_sidecar_request_with_app_and_cancel(
    app: &AppHandle,
    state: &AppState,
    method: &str,
    params: Value,
    cancel: CancellationToken,
) -> Result<Value, String> {
    handle_sidecar_request_with_context(state, Some(app), method, params, cancel).await
}

async fn handle_sidecar_request_with_context(
    state: &AppState,
    app: Option<&AppHandle>,
    method: &str,
    params: Value,
    cancel: CancellationToken,
) -> Result<Value, String> {
    if method == HOST_SSH_SERVICE_TARGET {
        return crate::commands::sidecar::resolve_ssh_service_target(state, params);
    }
    if method == HOST_SSH_FILESYSTEM_READ_TARGET {
        return crate::commands::sidecar::resolve_ssh_service_filesystem_read_target(state, params);
    }
    if method == HOST_SSH_FILESYSTEM_WRITE_TARGET {
        return resolve_ssh_filesystem_write_target(state, params);
    }
    if method == HOST_SSH_FILESYSTEM_EDIT_TARGET {
        return resolve_ssh_filesystem_edit_target(state, params);
    }
    if method == HOST_SSH_FILESYSTEM_BACKUP_TARGET {
        return resolve_ssh_filesystem_backup_target(state, params);
    }
    if method == HOST_FILESYSTEM_BACKUP_REGISTER {
        return register_ssh_filesystem_backup(state, params);
    }
    if method == HOST_SSH_FILESYSTEM_RESTORE_TARGET {
        return resolve_ssh_filesystem_restore_target(state, params);
    }
    if method == HOST_FILESYSTEM_BACKUP_RESTORED {
        return mark_ssh_filesystem_backup_restored(state, params);
    }
    if method == HOST_SSH_FILESYSTEM_CLEANUP_TARGET {
        return resolve_ssh_filesystem_cleanup_target(state, params);
    }
    if method == HOST_FILESYSTEM_BACKUP_DELETED {
        return mark_ssh_filesystem_backup_deleted(state, params);
    }
    if method == HOST_FILESYSTEM_BACKUP_LIST_QUERY {
        return query_ssh_filesystem_backup_list(state, params).await;
    }
    if method == HOST_FILESYSTEM_BACKUP_RETENTION_SCAN {
        return scan_ssh_filesystem_backups_for_retention(state, params).await;
    }
    if method == HOST_SSH_EFFECTFUL_ACTION_TARGET {
        return resolve_ssh_effectful_action_target(state, params).await;
    }
    if method == HOST_SSH_EFFECTFUL_ACTION_FINISHED {
        return finish_ssh_effectful_action(state, params);
    }
    if method == HOST_CONTEXT_FETCH {
        return handle_context_request(state, params);
    }
    if method == HOST_EVIDENCE_RECORD {
        return handle_evidence_record(state, params);
    }
    if method == HOST_EVIDENCE_FETCH {
        return handle_evidence_fetch(state, params);
    }
    if method == HOST_EVIDENCE_SEARCH {
        return handle_evidence_search(state, params);
    }
    if method == HOST_EVIDENCE_CORRELATE {
        return handle_evidence_correlation(state, params);
    }
    if method == HOST_EVIDENCE_COMPARE {
        return handle_evidence_compare(state, params);
    }
    if method == HOST_RETENTION_PREVIEW {
        return handle_retention_preview(state, params);
    }
    if method == HOST_FINDING_RECORD {
        return handle_finding_record(state, params);
    }
    if method == HOST_BRIEF_RECORD {
        return handle_brief_record(state, params);
    }
    if method == HOST_PLAN_RECORD {
        return handle_plan_record(state, params);
    }
    if method == HOST_PLAN_CHECK {
        return handle_plan_check(state, params);
    }
    if method == HOST_PLAN_STEP_RESULT {
        return handle_plan_step_result(state, params);
    }
    if method == HOST_ARTIFACT_RECORD {
        return handle_artifact_record(state, params);
    }
    if method == HOST_MCP_CATALOG {
        // 目录是 sidecar 唯一能知道 MCP 存在的地方，所以它也就是 `capabilities.mcp`
        // 的唯一依据（ADR 0014）。
        let catalog = mcp::catalog(&state.database, &state.mcp, state.credentials.clone()).await?;
        return serde_json::to_value(catalog).map_err(|error| error.to_string());
    }
    if method != HOST_TOOL_EXECUTE {
        return Err(format!("unknown host method `{method}`"));
    }

    let request = serde_json::from_value::<HostToolExecuteRequest>(params)
        .map_err(|error| format!("invalid host tool request: {error}"))?;

    if effectful_tool_requires_durable_plan(state, &request).await {
        return Ok(failed(
            "denied_by_policy",
            "effectful host tools require a durable task, ChangePlan, and plan step before execution",
            false,
            Some(json!({ "code": "durable_plan_required" })),
        ));
    }

    if let Some(task_id) = request.task_id.as_deref() {
        let task = match state.database.investigations().get_task(task_id) {
            Ok(task) => task,
            Err(DatabaseError::NotFound) => {
                return Ok(failed(
                    "not_found",
                    format!("investigation task `{task_id}` was not found"),
                    false,
                    None,
                ))
            }
            Err(error) => return Ok(failed("internal", error.to_string(), false, None)),
        };
        if let Some(violation) = task_guardrail_violation(&task, &request.tool_name, &request.input)
        {
            return Ok(failed(
                "denied_by_policy",
                violation.message,
                false,
                Some(json!({ "code": violation.code.as_str() })),
            ));
        }
        match check_plan_for_tool(
            state,
            task_id,
            &request.tool_name,
            &request.target,
            PlanCheckBinding {
                input: &request.input,
                plan_id: request.plan_id.as_deref(),
                step_id: request.plan_step_id.as_deref(),
                evidence_ids: request.evidence_ids.as_deref(),
            },
        )? {
            Ok(()) => {}
            Err(deviation) => {
                return Ok(failed(
                    "plan_deviation",
                    deviation.message.clone(),
                    false,
                    Some(serde_json::to_value(deviation).map_err(|error| error.to_string())?),
                ));
            }
        }
    }

    // MCP 工具先分流，而且必须在目标校验**之前**：它们是宿主的本地子进程，不是远端 SSH 上
    // 的工具，所以既没有 `srv_` 目标、也不该被要求有一个。这个判断放在 match 里做不到 ——
    // match 在目标校验之后，那时候 `mcp.<server>.<tool>` 已经被「remote host tools require
    // a concrete serverId」拒掉了。其余工具的那条路一个字节都没变。
    if mcp::is_mcp_tool_name(&request.tool_name) {
        let token = match prepare_host_tool_call(state, &request).await? {
            HostToolCallDecision::Respond(response) => return Ok(response),
            HostToolCallDecision::Execute(token) => token,
        };
        let response = mcp::execute(&state.mcp, &request.tool_name, &request.input, &cancel).await;
        finish_host_tool_call(state, &request, token, &response)?;
        return response;
    }

    let Some(server_id) = request.target.server_id.as_deref() else {
        return Ok(failed(
            "invalid_input",
            "remote host tools require a concrete serverId",
            false,
            None,
        ));
    };
    if request.target.host != "remote" || !server_id.starts_with("srv_") {
        return Ok(failed(
            "denied_by_policy",
            "host tools only accept a resolved remote srv_ target",
            false,
            None,
        ));
    }

    let server = match state.database.servers().get(server_id) {
        Ok(server) => server,
        Err(error) => {
            return Ok(failed(
                "not_found",
                format!("target server `{server_id}` was not found: {error}"),
                false,
                None,
            ))
        }
    };
    if server.metadata.environment != request.target.environment {
        return Ok(failed(
            "denied_by_policy",
            "tool target environment does not match the registered server",
            false,
            Some(json!({
                "serverEnvironment": server.metadata.environment,
                "targetEnvironment": request.target.environment,
            })),
        ));
    }
    if let Some(workspace_id) = request.target.workspace_id.as_deref() {
        let belongs = server
            .metadata
            .workspace_ids
            .as_ref()
            .is_some_and(|ids| ids.iter().any(|id| id == workspace_id));
        if !belongs {
            return Ok(failed(
                "denied_by_policy",
                "tool target workspace is not attached to the registered server",
                false,
                None,
            ));
        }
    }

    let token = match prepare_host_tool_call(state, &request).await? {
        HostToolCallDecision::Respond(response) => return Ok(response),
        HostToolCallDecision::Execute(token) => token,
    };
    let response = match consume_python_host_action_approval(state, &request).await {
        Err(error) => Ok(host_action_approval_failure(error)),
        Ok(()) => match request.tool_name.as_str() {
            SERVER_INFO => server_info(app, state, server_id, &request.input, &cancel).await,
            SERVER_EXEC => server_exec(app, state, server_id, &request, &cancel).await,
            SERVER_LOGS => server_logs(app, state, server_id, &request.input, &cancel).await,
            SERVER_SERVICES => {
                server_services(app, state, server_id, &request.input, &cancel).await
            }
            DOCKER_PS => docker_ps(app, state, server_id, &request.input, &cancel).await,
            DOCKER_LOGS => docker_logs(app, state, server_id, &request.input, &cancel).await,
            DOCKER_INSPECT => docker_inspect(app, state, server_id, &request.input, &cancel).await,
            DOCKER_RESTART => docker_restart(app, state, server_id, &request.input, &cancel).await,
            SYSTEMD_INSPECT => {
                systemd_inspect(app, state, server_id, &request.input, &cancel).await
            }
            SYSTEMD_RESTART => {
                systemd_restart(app, state, server_id, &request.input, &cancel).await
            }
            PACKAGE_INSPECT => {
                package_inspect(app, state, server_id, &request.input, &cancel).await
            }
            PACKAGE_INSTALL => {
                package_install(app, state, server_id, &request.input, &cancel).await
            }
            FILESYSTEM_READ => {
                filesystem_read(app, state, server_id, &request.input, &cancel).await
            }
            FILESYSTEM_WRITE => {
                filesystem_write(app, state, server_id, &request.input, &cancel).await
            }
            FILESYSTEM_EDIT => {
                filesystem_edit(app, state, server_id, &request.input, &cancel).await
            }
            FILESYSTEM_BACKUP => {
                filesystem_backup(app, state, server_id, &request.input, &cancel, &request).await
            }
            FILESYSTEM_BACKUP_CLEANUP => {
                filesystem_backup_cleanup(app, state, server_id, &request.input, &cancel, &request)
                    .await
            }
            FILESYSTEM_RESTORE => {
                filesystem_restore(app, state, server_id, &request.input, &cancel, &request).await
            }
            other => Ok(failed(
                "not_found",
                format!("host tool `{other}` is not enabled"),
                false,
                None,
            )),
        },
    };
    finish_host_tool_call(state, &request, token, &response)?;
    response
}

fn record_failure(code: &str, message: impl Into<String>, retryable: bool) -> Value {
    json!({
        "recorded": false,
        "error": { "code": code, "message": message.into(), "retryable": retryable }
    })
}

/// Resolve one C++ credential context only for an exact, currently authorized planned file write.
fn resolve_ssh_filesystem_write_target(state: &AppState, params: Value) -> Result<Value, String> {
    let request = serde_json::from_value::<HostSshFilesystemWriteTargetRequest>(params)
        .map_err(|_| "the SSH filesystem write target request is invalid".to_string())?;
    let input = json!({
        "path": request.path.clone(),
        "content": request.content.clone(),
    });
    validate_ssh_filesystem_action_binding(
        state,
        SshFilesystemActionBinding {
            server_id: &request.server_id,
            task_id: &request.task_id,
            tool_name: FILESYSTEM_WRITE,
            input: &input,
            target: &request.target,
            plan_id: &request.plan_id,
            plan_step_id: &request.plan_step_id,
            evidence_ids: &request.evidence_ids,
        },
    )?;
    crate::commands::sidecar::resolve_ssh_service_filesystem_write_target(
        state,
        json!({
            "serverId": request.server_id,
            "path": input["path"],
            "content": input["content"],
        }),
    )
}

fn resolve_ssh_filesystem_edit_target(state: &AppState, params: Value) -> Result<Value, String> {
    let request = serde_json::from_value::<HostSshFilesystemEditTargetRequest>(params)
        .map_err(|_| "the SSH filesystem edit target request is invalid".to_string())?;
    AgentEditRequest::check(
        &request.path,
        &request.expected_revision,
        request.old_string.clone(),
        request.new_string.clone(),
    )
    .map_err(|error| error.to_string())?;
    let input = json!({
        "path": request.path.clone(),
        "expectedRevision": request.expected_revision.clone(),
        "oldString": request.old_string.clone(),
        "newString": request.new_string.clone(),
    });
    validate_ssh_filesystem_action_binding(
        state,
        SshFilesystemActionBinding {
            server_id: &request.server_id,
            task_id: &request.task_id,
            tool_name: FILESYSTEM_EDIT,
            input: &input,
            target: &request.target,
            plan_id: &request.plan_id,
            plan_step_id: &request.plan_step_id,
            evidence_ids: &request.evidence_ids,
        },
    )?;
    crate::commands::sidecar::resolve_ssh_service_filesystem_edit_target(
        state,
        json!({
            "serverId": request.server_id,
            "path": request.path,
            "expectedRevision": request.expected_revision,
            "oldString": request.old_string,
            "newString": request.new_string,
        }),
    )
}

fn resolve_ssh_filesystem_backup_target(state: &AppState, params: Value) -> Result<Value, String> {
    let request = serde_json::from_value::<HostSshFilesystemBackupTargetRequest>(params)
        .map_err(|_| "the SSH filesystem backup target request is invalid".to_string())?;
    let path = request.path;
    let input = json!({ "path": path.clone() });
    validate_ssh_filesystem_action_binding(
        state,
        SshFilesystemActionBinding {
            server_id: &request.server_id,
            task_id: &request.task_id,
            tool_name: FILESYSTEM_BACKUP,
            input: &input,
            target: &request.target,
            plan_id: &request.plan_id,
            plan_step_id: &request.plan_step_id,
            evidence_ids: &request.evidence_ids,
        },
    )?;
    let token = tools::backup_token();
    yukinal_filesystem::AgentBackupRequest::check(&path, &token)
        .map_err(|error| error.to_string())?;
    let backup_path =
        yukinal_filesystem::backup_path_for(&path, &token).map_err(|error| error.to_string())?;
    crate::commands::sidecar::resolve_ssh_service_filesystem_backup_target(
        state,
        json!({
            "serverId": request.server_id,
            "path": path,
            "backupPath": backup_path,
        }),
    )
}

fn register_ssh_filesystem_backup(state: &AppState, params: Value) -> Result<Value, String> {
    let request = serde_json::from_value::<HostFilesystemBackupRegisterRequest>(params)
        .map_err(|_| "the filesystem backup registration request is invalid".to_string())?;
    if request.bytes_backed_up > yukinal_filesystem::MAX_AGENT_BACKUP_BYTES {
        return Err("filesystem backup exceeds the host size limit".into());
    }
    yukinal_filesystem::AgentCleanupBackupRequest::check(
        &request.path,
        &request.backup_path,
        &request.revision,
    )
    .map_err(|error| error.to_string())?;
    if request.trace_id.trim().is_empty()
        || request.call_id.trim().is_empty()
        || request.trace_id.len() > 256
        || request.call_id.len() > 256
        || request.trace_id.chars().any(char::is_control)
        || request.call_id.chars().any(char::is_control)
    {
        return Err("filesystem backup registration is missing its durable call identity".into());
    }
    let input = json!({ "path": request.path.clone() });
    validate_ssh_filesystem_action_binding(
        state,
        SshFilesystemActionBinding {
            server_id: &request.server_id,
            task_id: &request.task_id,
            tool_name: FILESYSTEM_BACKUP,
            input: &input,
            target: &request.target,
            plan_id: &request.plan_id,
            plan_step_id: &request.plan_step_id,
            evidence_ids: &request.evidence_ids,
        },
    )?;
    let now = yukinal_core::sidecar::iso8601_now();
    let record = FilesystemBackupRecord {
        id: tools::backup_record_id(&request.server_id, &request.backup_path),
        server_id: request.server_id,
        task_id: Some(request.task_id),
        plan_id: Some(request.plan_id),
        plan_step_id: Some(request.plan_step_id),
        trace_id: Some(request.trace_id),
        call_id: Some(request.call_id),
        path: request.path,
        backup_path: request.backup_path.clone(),
        revision: request.revision,
        bytes_backed_up: i64::try_from(request.bytes_backed_up).map_err(|_| {
            "filesystem backup byte count exceeds the host ledger range".to_string()
        })?,
        status: FilesystemBackupStatus::Available,
        created_at: now.clone(),
        updated_at: now,
        restored_at: None,
        deleted_at: None,
    };
    state
        .database
        .filesystem_backups()
        .insert(&record)
        .map_err(|_| {
            "backup exists remotely but could not be registered in the host ledger; preserve it and reconcile before restoring".to_string()
        })?;
    Ok(json!({ "registered": true, "backupPath": record.backup_path }))
}

fn resolve_ssh_filesystem_restore_target(state: &AppState, params: Value) -> Result<Value, String> {
    let request = serde_json::from_value::<HostSshFilesystemRestoreTargetRequest>(params)
        .map_err(|_| "the SSH filesystem restore target request is invalid".to_string())?;
    yukinal_filesystem::AgentRestoreRequest::check(
        &request.path,
        &request.backup_path,
        &request.expected_revision,
    )
    .map_err(|error| error.to_string())?;
    let input = json!({
        "path": request.path.clone(),
        "backupPath": request.backup_path.clone(),
        "expectedRevision": request.expected_revision.clone(),
    });
    validate_ssh_filesystem_action_binding(
        state,
        SshFilesystemActionBinding {
            server_id: &request.server_id,
            task_id: &request.task_id,
            tool_name: FILESYSTEM_RESTORE,
            input: &input,
            target: &request.target,
            plan_id: &request.plan_id,
            plan_step_id: &request.plan_step_id,
            evidence_ids: &request.evidence_ids,
        },
    )?;
    let backup = state
        .database
        .filesystem_backups()
        .find_available(&request.server_id, &request.path, &request.backup_path)
        .map_err(|_| "could not read the filesystem backup ledger".to_string())?
        .ok_or_else(|| "filesystem.restore requires an available host-owned backup".to_string())?;
    if backup.task_id.as_deref() != Some(request.task_id.as_str()) {
        return Err(
            "filesystem.restore cannot use a backup owned by another investigation task".into(),
        );
    }
    if backup.bytes_backed_up > yukinal_filesystem::MAX_AGENT_BACKUP_BYTES as i64 {
        return Err("filesystem backup exceeds the host restore limit".into());
    }
    let bytes_backed_up = usize::try_from(backup.bytes_backed_up)
        .map_err(|_| "filesystem backup byte count is invalid".to_string())?;
    crate::commands::sidecar::resolve_ssh_service_filesystem_restore_target(
        state,
        json!({
            "serverId": request.server_id,
            "path": request.path,
            "backupPath": request.backup_path,
            "expectedRevision": request.expected_revision,
            "revision": backup.revision,
            "bytesBackedUp": bytes_backed_up,
        }),
    )
}

fn validate_filesystem_backup_query_scope(
    state: &AppState,
    server_id: &str,
    task_id: &str,
    target: &HostToolTarget,
) -> Result<(), Value> {
    if target.host != "remote" || target.server_id.as_deref() != Some(server_id) {
        return Err(failed(
            "denied_by_policy",
            "filesystem backup metadata target does not match the selected server",
            false,
            None,
        ));
    }
    let server = state.database.servers().get(server_id).map_err(|_| {
        failed(
            "not_found",
            "the selected filesystem backup server was not found",
            false,
            None,
        )
    })?;
    if server.metadata.environment != target.environment {
        return Err(failed(
            "denied_by_policy",
            "filesystem backup metadata environment does not match the selected server",
            false,
            None,
        ));
    }
    if let Some(workspace_id) = target.workspace_id.as_deref() {
        if !server
            .metadata
            .workspace_ids
            .as_ref()
            .is_some_and(|ids| ids.iter().any(|id| id == workspace_id))
        {
            return Err(failed(
                "denied_by_policy",
                "filesystem backup metadata target is not attached to that workspace",
                false,
                None,
            ));
        }
    }
    let task = state
        .database
        .investigations()
        .get_task(task_id)
        .map_err(|_| {
            failed(
                "not_found",
                "the durable investigation task was not found",
                false,
                None,
            )
        })?;
    if task.server_id.as_deref() != Some(server_id) {
        return Err(failed(
            "denied_by_policy",
            "filesystem backup metadata target does not match the task's registered server",
            false,
            None,
        ));
    }
    let requested_scope = host_target_to_investigation(target).map_err(|_| {
        failed(
            "denied_by_policy",
            "filesystem backup metadata target is invalid",
            false,
            None,
        )
    })?;
    if !same_investigation_target(&task.scope, &requested_scope) {
        return Err(failed(
            "denied_by_policy",
            "filesystem backup metadata target does not match the task scope",
            false,
            None,
        ));
    }
    Ok(())
}

fn filesystem_backup_record_value(record: &FilesystemBackupRecord) -> Value {
    json!({
        "id": record.id.clone(),
        "serverId": record.server_id.clone(),
        "taskId": record.task_id.clone(),
        "path": record.path.clone(),
        "backupPath": record.backup_path.clone(),
        "revision": record.revision.clone(),
        "bytesBackedUp": record.bytes_backed_up,
        "status": record.status.as_str(),
        "createdAt": record.created_at.clone(),
        "updatedAt": record.updated_at.clone(),
        "restoredAt": record.restored_at.clone(),
        "deletedAt": record.deleted_at.clone(),
    })
}

async fn prepare_filesystem_backup_query_call(
    state: &AppState,
    request: &HostToolExecuteRequest,
    expected_tool: &str,
) -> Result<HostToolCallDecision, String> {
    if request.tool_name != expected_tool {
        return Ok(HostToolCallDecision::Respond(failed(
            "denied_by_policy",
            "filesystem backup query tool name does not match its host endpoint",
            false,
            None,
        )));
    }
    let (Some(server_id), Some(task_id)) = (
        request.target.server_id.as_deref(),
        request.task_id.as_deref(),
    ) else {
        return Ok(HostToolCallDecision::Respond(failed(
            "durable_task_required",
            "filesystem backup metadata requires a durable investigation task",
            false,
            None,
        )));
    };
    if let Err(response) =
        validate_filesystem_backup_query_scope(state, server_id, task_id, &request.target)
    {
        return Ok(HostToolCallDecision::Respond(response));
    }
    let task = match state.database.investigations().get_task(task_id) {
        Ok(task) => task,
        Err(error) => {
            return Ok(HostToolCallDecision::Respond(failed(
                "not_found",
                error.to_string(),
                false,
                None,
            )))
        }
    };
    if let Some(violation) = task_guardrail_violation(&task, &request.tool_name, &request.input) {
        return Ok(HostToolCallDecision::Respond(failed(
            "denied_by_policy",
            violation.message,
            false,
            Some(json!({ "code": violation.code.as_str() })),
        )));
    }
    match check_plan_for_tool(
        state,
        task_id,
        &request.tool_name,
        &request.target,
        PlanCheckBinding {
            input: &request.input,
            plan_id: request.plan_id.as_deref(),
            step_id: request.plan_step_id.as_deref(),
            evidence_ids: request.evidence_ids.as_deref(),
        },
    )? {
        Ok(()) => prepare_host_tool_call(state, request).await,
        Err(deviation) => Ok(HostToolCallDecision::Respond(failed(
            "plan_deviation",
            deviation.message.clone(),
            false,
            Some(serde_json::to_value(deviation).map_err(|error| error.to_string())?),
        ))),
    }
}

async fn query_ssh_filesystem_backup_list(
    state: &AppState,
    params: Value,
) -> Result<Value, String> {
    let request = serde_json::from_value::<HostFilesystemBackupListQueryRequest>(params)
        .map_err(|_| "the filesystem backup list query is invalid".to_string())?;
    let tool_call = request.tool_call;
    if tool_call.tool_name != FILESYSTEM_BACKUP_LIST {
        return Ok(failed(
            "denied_by_policy",
            "filesystem backup list query has an unexpected tool name",
            false,
            None,
        ));
    }
    let input = match serde_json::from_value::<FilesystemBackupListInput>(tool_call.input.clone()) {
        Ok(input) => input,
        Err(_) => {
            return Ok(failed(
                "invalid_input",
                "filesystem.backup.list input is invalid",
                false,
                None,
            ));
        }
    };
    if let Some(path) = input.path.as_deref() {
        if let Err(error) = validate_remote_path(path) {
            return Ok(failed("invalid_input", error, false, None));
        }
    }
    let status = match input.status.as_deref() {
        None => None,
        Some(raw) => match FilesystemBackupStatus::parse(raw) {
            Some(status) => Some(status),
            None => {
                return Ok(failed(
                    "invalid_input",
                    "filesystem.backup.list status must be available, restored, or deleted",
                    false,
                    None,
                ));
            }
        },
    };
    let limit = input.limit.unwrap_or(64);
    if !(1..=128).contains(&limit) {
        return Ok(failed(
            "invalid_input",
            "filesystem.backup.list limit must be between 1 and 128",
            false,
            None,
        ));
    }
    let token =
        match prepare_filesystem_backup_query_call(state, &tool_call, FILESYSTEM_BACKUP_LIST)
            .await?
        {
            HostToolCallDecision::Respond(response) => return Ok(response),
            HostToolCallDecision::Execute(token) => token,
        };
    let server_id = tool_call
        .target
        .server_id
        .as_deref()
        .expect("validated server id");
    let task_id = tool_call.task_id.as_deref().expect("validated task id");
    let response = match state.database.filesystem_backups().list_for_task(
        server_id,
        task_id,
        status,
        input.path.as_deref(),
        limit,
    ) {
        Ok((records, truncated)) => {
            let records = records
                .iter()
                .map(filesystem_backup_record_value)
                .collect::<Vec<_>>();
            success(json!({ "records": records, "truncated": truncated }))
        }
        Err(DatabaseError::Validation(message)) => failed("invalid_input", message, false, None),
        Err(error) => failed("internal", error.to_string(), false, None),
    };
    finish_host_tool_call(state, &tool_call, token, &Ok(response.clone()))?;
    Ok(response)
}

async fn scan_ssh_filesystem_backups_for_retention(
    state: &AppState,
    params: Value,
) -> Result<Value, String> {
    let request = serde_json::from_value::<HostFilesystemBackupRetentionScanRequest>(params)
        .map_err(|_| "the filesystem backup retention scan is invalid".to_string())?;
    let tool_call = request.tool_call;
    if tool_call.tool_name != FILESYSTEM_BACKUP_RETENTION {
        return Ok(failed(
            "denied_by_policy",
            "filesystem backup retention scan has an unexpected tool name",
            false,
            None,
        ));
    }
    let input =
        match serde_json::from_value::<FilesystemBackupRetentionInput>(tool_call.input.clone()) {
            Ok(input) => input,
            Err(_) => {
                return Ok(failed(
                    "invalid_input",
                    "filesystem.backup.retention input is invalid",
                    false,
                    None,
                ));
            }
        };
    if input.keep_latest.is_none() && input.older_than_days.is_none() {
        return Ok(failed(
            "invalid_input",
            "filesystem.backup.retention requires keepLatest or olderThanDays",
            false,
            None,
        ));
    }
    if input
        .keep_latest
        .is_some_and(|keep| !(1..=64).contains(&keep))
    {
        return Ok(failed(
            "invalid_input",
            "filesystem.backup.retention keepLatest must be between 1 and 64",
            false,
            None,
        ));
    }
    if input
        .older_than_days
        .is_some_and(|days| !(1..=3650).contains(&days))
    {
        return Ok(failed(
            "invalid_input",
            "filesystem.backup.retention olderThanDays must be between 1 and 3650",
            false,
            None,
        ));
    }
    if let Some(prefix) = input.path_prefix.as_deref() {
        if let Err(error) = validate_remote_path(prefix) {
            return Ok(failed("invalid_input", error, false, None));
        }
    }
    let token =
        match prepare_filesystem_backup_query_call(state, &tool_call, FILESYSTEM_BACKUP_RETENTION)
            .await?
        {
            HostToolCallDecision::Respond(response) => return Ok(response),
            HostToolCallDecision::Execute(token) => token,
        };
    let server_id = tool_call
        .target
        .server_id
        .as_deref()
        .expect("validated server id");
    let response = match state
        .database
        .filesystem_backups()
        .list_available_for_server_with_path_prefix(server_id, input.path_prefix.as_deref(), 512)
    {
        Ok((records, scan_truncated)) => {
            let records = records
                .iter()
                .filter(|record| {
                    input
                        .path_prefix
                        .as_deref()
                        .is_none_or(|prefix| record.path.starts_with(prefix))
                })
                .map(|record| {
                    json!({
                        "id": record.id.clone(),
                        "path": record.path.clone(),
                        "backupPath": record.backup_path.clone(),
                        "revision": record.revision.clone(),
                        "taskId": record.task_id.clone(),
                        "createdAt": record.created_at.clone(),
                        "bytesBackedUp": record.bytes_backed_up,
                    })
                })
                .collect::<Vec<_>>();
            let evaluated_at_epoch_seconds = yukinal_time::now_epoch_seconds();
            success(json!({
                "records": records,
                "truncated": scan_truncated,
                "evaluatedAtEpochSeconds": evaluated_at_epoch_seconds,
            }))
        }
        Err(DatabaseError::Validation(message)) => failed("invalid_input", message, false, None),
        Err(error) => failed("internal", error.to_string(), false, None),
    };
    finish_host_tool_call(state, &tool_call, token, &Ok(response.clone()))?;
    Ok(response)
}

fn validate_ssh_effectful_action_input(tool_call: &HostToolExecuteRequest) -> Result<(), Value> {
    match tool_call.tool_name.as_str() {
        SYSTEMD_RESTART => {
            let input = serde_json::from_value::<SystemdRestartInput>(tool_call.input.clone())
                .map_err(|_| {
                    failed(
                        "invalid_input",
                        "systemd.restart input is invalid",
                        false,
                        None,
                    )
                })?;
            if !is_safe_systemd_service_ref(&input.service) {
                return Err(failed(
                    "invalid_input",
                    "service must be a .service unit without shell metacharacters",
                    false,
                    None,
                ));
            }
            let timeout = input
                .timeout_seconds
                .unwrap_or(DEFAULT_SYSTEMD_RESTART_TIMEOUT);
            if !(1..=MAX_SYSTEMD_RESTART_TIMEOUT).contains(&timeout) {
                return Err(failed(
                    "invalid_input",
                    format!("timeoutSeconds must be between 1 and {MAX_SYSTEMD_RESTART_TIMEOUT}"),
                    false,
                    None,
                ));
            }
            Ok(())
        }
        DOCKER_RESTART => {
            let input = serde_json::from_value::<DockerRestartInput>(tool_call.input.clone())
                .map_err(|_| {
                    failed(
                        "invalid_input",
                        "docker.restart input is invalid",
                        false,
                        None,
                    )
                })?;
            if !is_safe_container_ref(&input.container) {
                return Err(failed(
                    "invalid_input",
                    "container must be a Docker name or id without shell metacharacters",
                    false,
                    None,
                ));
            }
            let timeout = input.timeout_seconds.unwrap_or(DEFAULT_RESTART_TIMEOUT);
            if !(1..=MAX_RESTART_TIMEOUT).contains(&timeout) {
                return Err(failed(
                    "invalid_input",
                    format!("timeoutSeconds must be between 1 and {MAX_RESTART_TIMEOUT}"),
                    false,
                    None,
                ));
            }
            Ok(())
        }
        PACKAGE_INSTALL => {
            let input = serde_json::from_value::<PackageInstallInput>(tool_call.input.clone())
                .map_err(|_| {
                    failed(
                        "invalid_input",
                        "package.install input is invalid",
                        false,
                        None,
                    )
                })?;
            if !is_supported_package_manager(&input.manager) {
                return Err(failed(
                    "invalid_input",
                    "package manager must be apt or dnf",
                    false,
                    None,
                ));
            }
            if !is_safe_package_ref(&input.package) {
                return Err(failed(
                    "invalid_input",
                    "package must be a safe reference without shell metacharacters",
                    false,
                    None,
                ));
            }
            if input
                .version
                .as_deref()
                .is_some_and(|version| !is_safe_package_version(version))
            {
                return Err(failed(
                    "invalid_input",
                    "package version contains unsupported characters",
                    false,
                    None,
                ));
            }
            let timeout = input
                .timeout_seconds
                .unwrap_or(DEFAULT_PACKAGE_INSTALL_TIMEOUT);
            if !(1..=MAX_PACKAGE_INSTALL_TIMEOUT).contains(&timeout) {
                return Err(failed(
                    "invalid_input",
                    format!("timeoutSeconds must be between 1 and {MAX_PACKAGE_INSTALL_TIMEOUT}"),
                    false,
                    None,
                ));
            }
            Ok(())
        }
        SERVER_EXEC => {
            let input = serde_json::from_value::<ServerExecInput>(tool_call.input.clone())
                .map_err(|_| {
                    failed("invalid_input", "server.exec input is invalid", false, None)
                })?;
            if let Err(message) = validate_server_exec_input(&input) {
                return Err(failed("invalid_input", message, false, None));
            }
            Ok(())
        }
        _ => Err(failed(
            "denied_by_policy",
            "the requested SSH action is not supported by this route",
            false,
            None,
        )),
    }
}

fn parse_host_server_exec_result(
    value: &Value,
    input: &ServerExecInput,
) -> Result<HostServerExecResult, ()> {
    let result = serde_json::from_value::<HostServerExecResult>(value.clone()).map_err(|_| ())?;
    if result.state != "completed"
        || !(0..=255).contains(&result.exit_code)
        || result.stdout.len().saturating_add(result.stderr.len())
            > input.max_output_bytes.saturating_mul(3)
        || result.duration_ms > input.timeout_ms.saturating_add(10_000)
    {
        return Err(());
    }
    let _stream_truncation = (result.stdout_truncated, result.stderr_truncated);
    Ok(result)
}

async fn resolve_ssh_effectful_action_target(
    state: &AppState,
    params: Value,
) -> Result<Value, String> {
    let request = serde_json::from_value::<HostSshEffectfulActionTargetRequest>(params)
        .map_err(|_| "the SSH action target request is invalid".to_string())?;
    let tool_call = request.tool_call;
    if let Err(response) = validate_ssh_effectful_action_input(&tool_call) {
        return Ok(response);
    }
    let tool_name = tool_call.tool_name.as_str();
    let (Some(server_id), Some(task_id)) = (
        tool_call.target.server_id.as_deref(),
        tool_call.task_id.as_deref(),
    ) else {
        return Ok(failed(
            "durable_plan_required",
            "this SSH action requires a durable remote action plan",
            false,
            None,
        ));
    };
    if tool_call.target.host != "remote" || !server_id.starts_with("srv_") {
        return Ok(failed(
            "denied_by_policy",
            "this SSH action requires a resolved remote server",
            false,
            None,
        ));
    }
    if !tool_call.run_id.as_deref().is_some_and(|run_id| {
        !run_id.trim().is_empty() && run_id.len() <= 256 && !run_id.chars().any(char::is_control)
    }) {
        return Ok(failed(
            "invalid_input",
            "the SSH action requires a valid durable run identity",
            false,
            None,
        ));
    }
    if effectful_tool_requires_durable_plan(state, &tool_call).await {
        return Ok(failed(
            "durable_plan_required",
            "this SSH action requires a durable task, ChangePlan and action step",
            false,
            Some(json!({ "code": "durable_plan_required" })),
        ));
    }
    let server = match state.database.servers().get(server_id) {
        Ok(server) => server,
        Err(_) => {
            return Ok(failed(
                "not_found",
                "the selected server was not found",
                false,
                None,
            ))
        }
    };
    if server.metadata.environment != tool_call.target.environment {
        return Ok(failed(
            "denied_by_policy",
            "the SSH action target environment does not match the selected server",
            false,
            None,
        ));
    }
    if let Some(workspace_id) = tool_call.target.workspace_id.as_deref() {
        if !server
            .metadata
            .workspace_ids
            .as_ref()
            .is_some_and(|ids| ids.iter().any(|id| id == workspace_id))
        {
            return Ok(failed(
                "denied_by_policy",
                "the SSH action target is not attached to that workspace",
                false,
                None,
            ));
        }
    }
    let task = match state.database.investigations().get_task(task_id) {
        Ok(task) => task,
        Err(DatabaseError::NotFound) => {
            return Ok(failed(
                "not_found",
                "the durable investigation task was not found",
                false,
                None,
            ));
        }
        Err(error) => return Ok(failed("internal", error.to_string(), false, None)),
    };
    if task.server_id.as_deref() != Some(server_id) {
        return Ok(failed(
            "denied_by_policy",
            "the SSH action target does not match the task's registered server",
            false,
            None,
        ));
    }
    let requested_scope = match host_target_to_investigation(&tool_call.target) {
        Ok(scope) => scope,
        Err(_) => {
            return Ok(failed(
                "denied_by_policy",
                "the SSH action target is invalid",
                false,
                None,
            ));
        }
    };
    if !same_investigation_target(&task.scope, &requested_scope) {
        return Ok(failed(
            "denied_by_policy",
            "the SSH action target does not match the task scope",
            false,
            None,
        ));
    }
    if let Some(violation) = task_guardrail_violation(&task, tool_name, &tool_call.input) {
        return Ok(failed(
            "denied_by_policy",
            violation.message,
            false,
            Some(json!({ "code": violation.code.as_str() })),
        ));
    }
    match check_plan_for_tool(
        state,
        task_id,
        tool_name,
        &tool_call.target,
        PlanCheckBinding {
            input: &tool_call.input,
            plan_id: tool_call.plan_id.as_deref(),
            step_id: tool_call.plan_step_id.as_deref(),
            evidence_ids: tool_call.evidence_ids.as_deref(),
        },
    )? {
        Ok(()) => {}
        Err(deviation) => {
            return Ok(failed(
                "plan_deviation",
                deviation.message.clone(),
                false,
                Some(serde_json::to_value(deviation).map_err(|error| error.to_string())?),
            ));
        }
    }
    match crate::commands::ssh_service::supports_cpp_target(state, server_id) {
        Ok(true) => {}
        Ok(false) => {
            return Ok(failed(
                "unsupported_route",
                "the selected SSH target is not supported by the C++ SSH service",
                false,
                None,
            ));
        }
        Err(error) => return Ok(failed("internal", error, false, None)),
    }

    let token = match prepare_host_tool_call(state, &tool_call).await? {
        HostToolCallDecision::Respond(response) => {
            if response.get("status").and_then(Value::as_str) == Some("success") {
                if tool_name == SERVER_EXEC {
                    let input = serde_json::from_value::<ServerExecInput>(tool_call.input.clone())
                        .map_err(|_| {
                            "server.exec input became invalid during replay".to_string()
                        })?;
                    let Some(output) = response.get("output") else {
                        return Ok(failed(
                            "invalid_response",
                            "the cached server.exec result is unavailable",
                            false,
                            None,
                        ));
                    };
                    if parse_host_server_exec_result(output, &input).is_err() {
                        return Ok(failed(
                            "invalid_response",
                            "the cached server.exec result is invalid",
                            false,
                            None,
                        ));
                    }
                    return Ok(json!({ "status": "already_completed", "output": output }));
                }
                return Ok(json!({ "status": "already_completed" }));
            }
            return Ok(response);
        }
        HostToolCallDecision::Execute(token) => token,
    };
    if let Err(error) = consume_effectful_ssh_action_approval(state, &tool_call).await {
        let response = host_action_approval_failure(error);
        finish_host_tool_call(state, &tool_call, token, &Ok(response.clone()))?;
        return Ok(response);
    }
    let target_data = match crate::commands::sidecar::resolve_ssh_service_target(
        state,
        json!({ "serverId": server_id }),
    ) {
        Ok(target_data) => target_data,
        Err(error) => {
            let response = failed("transport", error, false, None);
            finish_host_tool_call(state, &tool_call, token, &Ok(response.clone()))?;
            return Ok(response);
        }
    };
    let mut response = json!({ "status": "success", "targetData": target_data });
    if tool_name == SERVER_EXEC {
        let target = response
            .get_mut("targetData")
            .and_then(Value::as_object_mut)
            .and_then(|target_data| target_data.get_mut("target"))
            .and_then(Value::as_object_mut)
            .ok_or_else(|| "the resolved SSH target is invalid".to_string())?;
        if target.get("target_id").and_then(Value::as_str) != Some(server_id) {
            return Ok(failed(
                "denied_by_policy",
                "the resolved SSH target does not match the approved server",
                false,
                None,
            ));
        }
        target.insert("allow_server_exec".to_string(), Value::Bool(true));
    }
    Ok(response)
}

fn finish_ssh_effectful_action(state: &AppState, params: Value) -> Result<Value, String> {
    let request = serde_json::from_value::<HostSshEffectfulActionFinishedRequest>(params)
        .map_err(|_| "the SSH action completion request is invalid".to_string())?;
    if let Err(response) = validate_ssh_effectful_action_input(&request.tool_call) {
        return Ok(response);
    }
    if [
        request.tool_call.call_id.as_str(),
        request.tool_call.trace_id.as_str(),
    ]
    .iter()
    .any(|value| {
        value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control)
    }) {
        return Ok(failed(
            "invalid_input",
            "SSH action completion call identity is invalid",
            false,
            None,
        ));
    }
    if request.tool_call.tool_name != SERVER_EXEC && request.result.is_some() {
        return Ok(failed(
            "invalid_input",
            "only server.exec accepts a command result payload",
            false,
            None,
        ));
    }
    if request.tool_call.tool_name == SERVER_EXEC
        && request.outcome == "unknown"
        && request.result.is_some()
    {
        return Ok(failed(
            "invalid_input",
            "an unknown server.exec outcome cannot include a completed result",
            false,
            None,
        ));
    }
    let (output, server_exec_exit_code) = match request.tool_call.tool_name.as_str() {
        SYSTEMD_RESTART => {
            let input =
                serde_json::from_value::<SystemdRestartInput>(request.tool_call.input.clone())
                    .map_err(|_| "systemd.restart completion input is invalid".to_string())?;
            (
                Some(json!({ "service": input.service, "restarted": true })),
                None,
            )
        }
        DOCKER_RESTART => {
            let input =
                serde_json::from_value::<DockerRestartInput>(request.tool_call.input.clone())
                    .map_err(|_| "docker.restart completion input is invalid".to_string())?;
            (
                Some(json!({ "container": input.container, "restarted": true })),
                None,
            )
        }
        PACKAGE_INSTALL => {
            let input =
                serde_json::from_value::<PackageInstallInput>(request.tool_call.input.clone())
                    .map_err(|_| "package.install completion input is invalid".to_string())?;
            (
                Some(
                    serde_json::to_value(PackageInstallResult {
                        manager: input.manager,
                        package: input.package,
                        version: input.version,
                        installed: true,
                    })
                    .map_err(|error| error.to_string())?,
                ),
                None,
            )
        }
        SERVER_EXEC => {
            let input = serde_json::from_value::<ServerExecInput>(request.tool_call.input.clone())
                .map_err(|_| "server.exec completion input is invalid".to_string())?;
            let parsed = match request.result.as_ref() {
                Some(value) => match parse_host_server_exec_result(value, &input) {
                    Ok(result) => Some(result),
                    Err(()) => {
                        return Ok(failed(
                            "invalid_input",
                            "server.exec completion result is invalid",
                            false,
                            None,
                        ));
                    }
                },
                None => None,
            };
            if parsed.is_none() && request.outcome == "succeeded" {
                return Ok(failed(
                    "invalid_input",
                    "server.exec success requires its bounded command result",
                    false,
                    None,
                ));
            }
            (
                request.result.clone(),
                parsed.map(|result| result.exit_code),
            )
        }
        _ => {
            return Ok(failed(
                "denied_by_policy",
                "unsupported SSH action",
                false,
                None,
            ))
        }
    };
    let response = match request.outcome.as_str() {
        "succeeded" => {
            if request.tool_call.tool_name == SERVER_EXEC && server_exec_exit_code != Some(0) {
                return Ok(failed(
                    "invalid_input",
                    "server.exec success must have exit code zero",
                    false,
                    None,
                ));
            }
            let Some(output) = output else {
                return Ok(failed(
                    "invalid_input",
                    "SSH action result is missing",
                    false,
                    None,
                ));
            };
            success(output)
        }
        "failed" => {
            if request.tool_call.tool_name == SERVER_EXEC && server_exec_exit_code == Some(0) {
                return Ok(failed(
                    "invalid_input",
                    "server.exec failure cannot carry a successful exit code",
                    false,
                    None,
                ));
            }
            failed(
                "execution_failed",
                format!(
                    "{} did not complete successfully",
                    request.tool_call.tool_name
                ),
                false,
                if request.tool_call.tool_name == SERVER_EXEC {
                    output
                } else {
                    None
                },
            )
        }
        "unknown" => failed(
            "transport",
            format!("{} outcome is unknown", request.tool_call.tool_name),
            true,
            None,
        ),
        _ => {
            return Ok(failed(
                "invalid_input",
                "SSH action outcome is invalid",
                false,
                None,
            ))
        }
    };
    let token = HostToolCallToken {
        key: (
            request.tool_call.trace_id.clone(),
            request.tool_call.call_id.clone(),
        ),
    };
    finish_host_tool_call(state, &request.tool_call, Some(token), &Ok(response))?;
    Ok(json!({ "status": "success", "finished": true }))
}

fn resolve_ssh_filesystem_cleanup_target(state: &AppState, params: Value) -> Result<Value, String> {
    let request = serde_json::from_value::<HostSshFilesystemCleanupTargetRequest>(params)
        .map_err(|_| "the SSH filesystem cleanup target request is invalid".to_string())?;
    if [
        request.run_id.as_str(),
        request.trace_id.as_str(),
        request.call_id.as_str(),
    ]
    .iter()
    .any(|value| {
        value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control)
    }) {
        return Err("filesystem cleanup durable call identity is invalid".into());
    }
    let cleanup_input =
        serde_json::from_value::<FilesystemBackupCleanupInput>(request.input.clone())
            .map_err(|_| "filesystem.backup.cleanup input is invalid".to_string())?;
    let (items, batch) = match cleanup_input.resolve()? {
        BackupCleanupRequest::Single(item) => (vec![item], false),
        BackupCleanupRequest::Batch(items) => (items, true),
    };
    let context = HostToolExecuteRequest {
        call_id: request.call_id.clone(),
        run_id: Some(request.run_id.clone()),
        trace_id: request.trace_id.clone(),
        tool_name: FILESYSTEM_BACKUP_CLEANUP.to_string(),
        input: request.input.clone(),
        target: request.target.clone(),
        task_id: Some(request.task_id.clone()),
        plan_id: Some(request.plan_id.clone()),
        plan_step_id: Some(request.plan_step_id.clone()),
        evidence_ids: Some(request.evidence_ids.clone()),
        approval_id: None,
    };
    validate_ssh_filesystem_action_binding(
        state,
        SshFilesystemActionBinding {
            server_id: &request.server_id,
            task_id: &request.task_id,
            tool_name: FILESYSTEM_BACKUP_CLEANUP,
            input: &request.input,
            target: &request.target,
            plan_id: &request.plan_id,
            plan_step_id: &request.plan_step_id,
            evidence_ids: &request.evidence_ids,
        },
    )?;

    // Reject malformed paths for the entire request before reporting any item as executable.
    let checked = items
        .iter()
        .map(|item| {
            AgentCleanupBackupRequest::check(&item.path, &item.backup_path, &item.expected_revision)
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut bindings = Vec::with_capacity(items.len());
    let mut has_ready_item = false;
    for (item, _checked_request) in items.iter().zip(checked) {
        let mut binding = json!({
            "path": item.path.clone(),
            "backupPath": item.backup_path.clone(),
            "expectedRevision": item.expected_revision.clone(),
        });
        let backup = match state.database.filesystem_backups().find_available(
            &request.server_id,
            &item.path,
            &item.backup_path,
        ) {
            Ok(Some(record)) => record,
            Ok(None) if batch => {
                binding["status"] = json!("skipped");
                binding["reason"] =
                    json!("no available host ledger record for this server and backup path");
                bindings.push(binding);
                continue;
            }
            Ok(None) => {
                return Err("filesystem.backup.cleanup requires an available host-owned backup for this server and target path".into());
            }
            Err(error) if batch => {
                binding["status"] = json!("failed");
                binding["reason"] = json!(truncate_text(
                    &format!("could not read the filesystem backup ledger: {error}"),
                    1_024,
                ));
                bindings.push(binding);
                continue;
            }
            Err(error) => {
                return Err(format!(
                    "could not read the filesystem backup ledger: {error}"
                ))
            }
        };

        let same_task = backup.task_id.as_deref() == Some(request.task_id.as_str());
        let cross_task_allowed =
            batch && !same_task && tools::cross_task_cleanup_is_plan_bound(state, &context, item);
        if !same_task && !cross_task_allowed {
            if batch {
                binding["status"] = json!("skipped");
                binding["reason"] = json!("owned by another investigation task and not bound by the approved rotation step");
                bindings.push(binding);
                continue;
            }
            return Err(
                "filesystem.backup.cleanup cannot use a backup owned by another investigation task"
                    .into(),
            );
        }
        if !backup
            .revision
            .eq_ignore_ascii_case(&item.expected_revision)
        {
            if batch {
                binding["status"] = json!("failed");
                binding["reason"] =
                    json!("expectedRevision does not match the revision recorded for this backup");
                bindings.push(binding);
                continue;
            }
            return Err("expectedRevision does not match the revision recorded for this backup; preserve it and re-read the ledger".into());
        }
        let bytes_backed_up = usize::try_from(backup.bytes_backed_up)
            .ok()
            .filter(|bytes| *bytes <= yukinal_filesystem::MAX_AGENT_BACKUP_BYTES);
        let Some(bytes_backed_up) = bytes_backed_up else {
            if batch {
                binding["status"] = json!("failed");
                binding["reason"] = json!("the host backup ledger has an invalid byte count");
                bindings.push(binding);
                continue;
            }
            return Err("the host backup ledger has an invalid byte count".into());
        };
        binding["status"] = json!("ready");
        binding["revision"] = json!(backup.revision);
        binding["bytesBackedUp"] = json!(bytes_backed_up);
        binding["ownerTaskId"] = json!(backup.task_id);
        has_ready_item = true;
        bindings.push(binding);
    }

    let mut payload = if has_ready_item {
        crate::commands::sidecar::resolve_ssh_service_target(
            state,
            json!({ "serverId": request.server_id }),
        )?
    } else {
        json!({})
    };
    payload["filesystemCleanup"] = json!({ "items": bindings });
    Ok(payload)
}

fn mark_ssh_filesystem_backup_deleted(state: &AppState, params: Value) -> Result<Value, String> {
    let request = serde_json::from_value::<HostFilesystemBackupDeletedRequest>(params)
        .map_err(|_| "the filesystem cleanup registration request is invalid".to_string())?;
    if request.bytes_deleted > yukinal_filesystem::MAX_AGENT_BACKUP_BYTES
        || request.trace_id.trim().is_empty()
        || request.call_id.trim().is_empty()
        || request.trace_id.len() > 256
        || request.call_id.len() > 256
        || request.trace_id.chars().any(char::is_control)
        || request.call_id.chars().any(char::is_control)
    {
        return Err("filesystem cleanup result or durable call identity is invalid".into());
    }
    let cleanup_input =
        serde_json::from_value::<FilesystemBackupCleanupInput>(request.input.clone())
            .map_err(|_| "filesystem.backup.cleanup input is invalid".to_string())?;
    let (items, batch) = match cleanup_input.resolve()? {
        BackupCleanupRequest::Single(item) => (vec![item], false),
        BackupCleanupRequest::Batch(items) => (items, true),
    };
    if !items.iter().any(|candidate| {
        candidate.path == request.item.path
            && candidate.backup_path == request.item.backup_path
            && candidate
                .expected_revision
                .eq_ignore_ascii_case(&request.item.expected_revision)
    }) {
        return Err("the deleted backup is not part of the approved cleanup input".into());
    }
    AgentCleanupBackupRequest::check(
        &request.item.path,
        &request.item.backup_path,
        &request.item.expected_revision,
    )
    .map_err(|error| error.to_string())?;
    validate_ssh_filesystem_action_binding(
        state,
        SshFilesystemActionBinding {
            server_id: &request.server_id,
            task_id: &request.task_id,
            tool_name: FILESYSTEM_BACKUP_CLEANUP,
            input: &request.input,
            target: &request.target,
            plan_id: &request.plan_id,
            plan_step_id: &request.plan_step_id,
            evidence_ids: &request.evidence_ids,
        },
    )?;
    let context = HostToolExecuteRequest {
        call_id: request.call_id,
        run_id: None,
        trace_id: request.trace_id,
        tool_name: FILESYSTEM_BACKUP_CLEANUP.to_string(),
        input: request.input,
        target: request.target,
        task_id: Some(request.task_id.clone()),
        plan_id: Some(request.plan_id.clone()),
        plan_step_id: Some(request.plan_step_id.clone()),
        evidence_ids: Some(request.evidence_ids),
        approval_id: None,
    };
    let backup = state
        .database
        .filesystem_backups()
        .find_available(
            &request.server_id,
            &request.item.path,
            &request.item.backup_path,
        )
        .map_err(|_| "could not read the filesystem backup ledger".to_string())?
        .ok_or_else(|| "filesystem backup is no longer available".to_string())?;
    let same_task = backup.task_id.as_deref() == Some(request.task_id.as_str());
    if !same_task
        && (!batch || !tools::cross_task_cleanup_is_plan_bound(state, &context, &request.item))
    {
        return Err(
            "filesystem.backup.cleanup cannot use a backup owned by another investigation task"
                .into(),
        );
    }
    if !backup
        .revision
        .eq_ignore_ascii_case(&request.item.expected_revision)
        || !backup.revision.eq_ignore_ascii_case(&request.revision)
        || i64::try_from(request.bytes_deleted).ok() != Some(backup.bytes_backed_up)
    {
        return Err("filesystem cleanup result does not match the host backup ledger".into());
    }
    state
        .database
        .filesystem_backups()
        .mark_deleted(
            &request.server_id,
            &request.item.path,
            &request.item.backup_path,
            &yukinal_core::sidecar::iso8601_now(),
        )
        .map_err(|_| "filesystem backup could not be consumed in the host ledger".to_string())?;
    Ok(json!({
        "deleted": true,
        "path": request.item.path,
        "backupPath": request.item.backup_path,
        "revision": backup.revision,
        "bytesDeleted": request.bytes_deleted,
    }))
}

fn mark_ssh_filesystem_backup_restored(state: &AppState, params: Value) -> Result<Value, String> {
    let request = serde_json::from_value::<HostFilesystemBackupRestoredRequest>(params)
        .map_err(|_| "the filesystem restore registration request is invalid".to_string())?;
    if request.bytes_before > yukinal_filesystem::MAX_AGENT_BACKUP_BYTES
        || request.bytes_after > yukinal_filesystem::MAX_AGENT_BACKUP_BYTES
        || request.trace_id.trim().is_empty()
        || request.call_id.trim().is_empty()
        || request.trace_id.len() > 256
        || request.call_id.len() > 256
        || request.trace_id.chars().any(char::is_control)
        || request.call_id.chars().any(char::is_control)
    {
        return Err("filesystem restore result or durable call identity is invalid".into());
    }
    yukinal_filesystem::AgentRestoreRequest::check(
        &request.path,
        &request.backup_path,
        &request.expected_revision,
    )
    .map_err(|error| error.to_string())?;
    let input = json!({
        "path": request.path.clone(),
        "backupPath": request.backup_path.clone(),
        "expectedRevision": request.expected_revision.clone(),
    });
    validate_ssh_filesystem_action_binding(
        state,
        SshFilesystemActionBinding {
            server_id: &request.server_id,
            task_id: &request.task_id,
            tool_name: FILESYSTEM_RESTORE,
            input: &input,
            target: &request.target,
            plan_id: &request.plan_id,
            plan_step_id: &request.plan_step_id,
            evidence_ids: &request.evidence_ids,
        },
    )?;
    let backup = state
        .database
        .filesystem_backups()
        .find_available(&request.server_id, &request.path, &request.backup_path)
        .map_err(|_| "could not read the filesystem backup ledger".to_string())?
        .ok_or_else(|| "filesystem backup is no longer available".to_string())?;
    if backup.task_id.as_deref() != Some(request.task_id.as_str()) {
        return Err(
            "filesystem.restore cannot use a backup owned by another investigation task".into(),
        );
    }
    if !backup.revision.eq_ignore_ascii_case(&request.revision)
        || i64::try_from(request.bytes_after).ok() != Some(backup.bytes_backed_up)
    {
        return Err("filesystem restore result does not match the host backup ledger".into());
    }
    state
        .database
        .filesystem_backups()
        .mark_restored(
            &request.server_id,
            &request.path,
            &request.backup_path,
            &yukinal_core::sidecar::iso8601_now(),
        )
        .map_err(|_| "the remote file was restored but its host backup ledger could not be consumed; reconcile before another restore".to_string())?;
    Ok(json!({
        "restored": true,
        "path": request.path,
        "backupPath": request.backup_path,
        "revision": request.revision,
        "bytesBefore": request.bytes_before,
        "bytesAfter": request.bytes_after,
    }))
}

struct SshFilesystemActionBinding<'a> {
    server_id: &'a str,
    task_id: &'a str,
    tool_name: &'a str,
    input: &'a Value,
    target: &'a HostToolTarget,
    plan_id: &'a str,
    plan_step_id: &'a str,
    evidence_ids: &'a [String],
}

fn validate_ssh_filesystem_action_binding(
    state: &AppState,
    binding: SshFilesystemActionBinding<'_>,
) -> Result<(), String> {
    let SshFilesystemActionBinding {
        server_id,
        task_id,
        tool_name,
        input,
        target,
        plan_id,
        plan_step_id,
        evidence_ids,
    } = binding;
    if target.host != "remote"
        || target.server_id.as_deref() != Some(server_id)
        || evidence_ids.len() > 128
        || evidence_ids.iter().collect::<BTreeSet<_>>().len() != evidence_ids.len()
    {
        return Err("the SSH filesystem action target binding is invalid".into());
    }
    let server = state
        .database
        .servers()
        .get(server_id)
        .map_err(|_| "the selected SSH server is not available".to_string())?;
    if server.metadata.environment != target.environment {
        return Err("the SSH filesystem action environment does not match the server".into());
    }
    if let Some(workspace_id) = target.workspace_id.as_deref() {
        if !server
            .metadata
            .workspace_ids
            .as_ref()
            .is_some_and(|ids| ids.iter().any(|id| id == workspace_id))
        {
            return Err(
                "the SSH filesystem action target is not attached to that workspace".into(),
            );
        }
    }
    match check_plan_for_tool(
        state,
        task_id,
        tool_name,
        target,
        PlanCheckBinding {
            input,
            plan_id: Some(plan_id),
            step_id: Some(plan_step_id),
            evidence_ids: Some(evidence_ids),
        },
    )? {
        Ok(()) => {}
        Err(deviation) => return Err(deviation.message),
    }
    let plan = state
        .database
        .investigations()
        .latest_plan(task_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "the active filesystem action plan is unavailable".to_string())?;
    if plan.current_step_id.as_deref() != Some(plan_step_id)
        || !plan
            .steps
            .iter()
            .any(|step| step.id == plan_step_id && step.kind == PlanStepKind::Action)
    {
        return Err("filesystem mutation is available only in the current action plan step".into());
    }
    Ok(())
}

fn same_investigation_scope(task: &InvestigationTask, evidence: &Evidence) -> bool {
    serde_json::to_value(&task.scope).ok() == serde_json::to_value(&evidence.scope).ok()
}

fn host_target_to_investigation(
    target: &HostToolTarget,
) -> Result<yukinal_database::models::InvestigationTarget, String> {
    let host = match target.host.as_str() {
        "local" => yukinal_database::models::InvestigationTargetHost::Local,
        "remote" => yukinal_database::models::InvestigationTargetHost::Remote,
        other => return Err(format!("unknown tool target host `{other}`")),
    };
    Ok(yukinal_database::models::InvestigationTarget {
        host,
        server_id: target.server_id.clone(),
        workspace_id: target.workspace_id.clone(),
        environment: target.environment,
    })
}

fn same_investigation_target(
    left: &yukinal_database::models::InvestigationTarget,
    right: &yukinal_database::models::InvestigationTarget,
) -> bool {
    left.host == right.host
        && left.server_id == right.server_id
        && left.workspace_id == right.workspace_id
        && left.environment == right.environment
}

fn evidence_failure(code: &str, message: impl Into<String>, retryable: bool) -> Value {
    json!({
        "recorded": false,
        "error": { "code": code, "message": message.into(), "retryable": retryable }
    })
}

/// The three response shapes of this protocol, `pub(crate)` because the MCP path
/// (`commands/mcp.rs`) answers with the same envelope: two hand-written copies of
/// `{"status": "failed", "error": {...}}` is exactly how one of them ends up with a
/// field the schema does not accept.
pub(crate) fn success(output: Value) -> Value {
    json!({ "status": "success", "output": output })
}

pub(crate) fn failed(
    code: &str,
    message: impl Into<String>,
    retryable: bool,
    detail: Option<Value>,
) -> Value {
    let mut error = json!({ "code": code, "message": message.into(), "retryable": retryable });
    if let Some(detail) = detail {
        error["detail"] = detail;
    }
    json!({ "status": "failed", "error": error })
}

pub(crate) fn cancelled_failure() -> Value {
    failed("cancelled", "Host operation cancelled", false, None)
}

#[cfg(test)]
mod tests;
