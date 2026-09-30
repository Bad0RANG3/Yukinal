//! Host-side execution for bounded Agent tools (read-only plus permission-gated writes).
//!
//! The sidecar can describe and request a tool, but it cannot open SSH sessions or
//! resolve credentials. This module is the narrow, deny-by-default bridge from the
//! sidecar request to Rust-owned state.
//!
//! The file tools (`filesystem.read` / `filesystem.write` / `filesystem.edit` /
//! `filesystem.backup` / `filesystem.restore`) delegate to
//! `yukinal-filesystem`: path policy, byte caps and bounded decoding live there, and this module
//! only maps the capability's typed failures onto the host protocol's failure codes
//! (see [`filesystem_failure`]).
//!
//! `filesystem.write` overwrites, `filesystem.edit` is a guarded read-then-modify,
//! `filesystem.backup` creates a host-owned sibling copy, and `filesystem.restore` publishes
//! that copy only with a current revision guard; the capability's crate docs state what these
//! guarantees do **not** cover (SFTP has no compare-and-swap, so checks and publication are
//! multiple round trips apart).

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use rand::Rng as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use yukinal_core::docker::{
    bounded_log_lines, docker_restart_command, is_safe_container_ref, parse_docker_inspect,
    parse_docker_ps, shell_quote, truncate_text, DockerLogsResult, DockerRestartResult,
    DEFAULT_LOG_TAIL, DEFAULT_RESTART_TIMEOUT, DOCKER_PS_ALL_COMMAND, DOCKER_PS_COMMAND,
    MAX_LOG_TAIL, MAX_RESTART_TIMEOUT,
};
use yukinal_core::package::{
    is_safe_package_ref, is_safe_package_version, is_supported_package_manager,
    package_inspect_command, package_install_command, parse_package_inspect, PackageInstallResult,
    DEFAULT_PACKAGE_INSTALL_TIMEOUT, MAX_PACKAGE_INSTALL_TIMEOUT,
};
use yukinal_core::service::{
    is_safe_systemd_service_ref, parse_systemd_inspect, systemd_inspect_command,
    systemd_restart_command, SystemdRestartResult, DEFAULT_SYSTEMD_RESTART_TIMEOUT,
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
use yukinal_ssh::SshBackend;

use crate::commands::files::remote_file_service;
use crate::commands::logs::{log_discovery_command_for, parse_logs_output, ServerLogsInput};
use crate::commands::mcp;
use crate::commands::services::{
    filter_services, parse_services_output, service_discovery_command, ServerServicesInput,
};
use crate::commands::terminal::ensure_session;
use crate::state::AppState;

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
pub(super) fn server_exec_input_fingerprint(input: &Value) -> Result<String, String> {
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
struct FilesystemBackupLedgerItemResult {
    id: String,
    server_id: String,
    task_id: String,
    path: String,
    backup_path: String,
    revision: String,
    bytes_backed_up: i64,
    status: String,
    created_at: String,
    updated_at: String,
    restored_at: Option<String>,
    deleted_at: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FilesystemBackupListResult {
    backups: Vec<FilesystemBackupLedgerItemResult>,
    truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FilesystemBackupRetentionCandidateResult {
    path: String,
    backup_path: String,
    revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    created_at: String,
    bytes_backed_up: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FilesystemBackupRetentionResult {
    candidates: Vec<FilesystemBackupRetentionCandidateResult>,
    truncated: bool,
    kept_count: usize,
    scanned_count: usize,
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
/// dispatcher. Keeping the token explicit prevents a user Stop from ending only
/// the Node promise while an SSH operation continues in Rust.
pub(crate) async fn handle_sidecar_request_with_cancel(
    state: &AppState,
    method: &str,
    params: Value,
    cancel: CancellationToken,
) -> Result<Value, String> {
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
    let response = match request.tool_name.as_str() {
        SERVER_INFO => server_info(state, server_id, &request.input, &cancel).await,
        SERVER_EXEC => server_exec(state, server_id, &request, &cancel).await,
        SERVER_LOGS => server_logs(state, server_id, &request.input, &cancel).await,
        SERVER_SERVICES => server_services(state, server_id, &request.input, &cancel).await,
        DOCKER_PS => docker_ps(state, server_id, &request.input, &cancel).await,
        DOCKER_LOGS => docker_logs(state, server_id, &request.input, &cancel).await,
        DOCKER_INSPECT => docker_inspect(state, server_id, &request.input, &cancel).await,
        DOCKER_RESTART => docker_restart(state, server_id, &request.input, &cancel).await,
        SYSTEMD_INSPECT => systemd_inspect(state, server_id, &request.input, &cancel).await,
        SYSTEMD_RESTART => systemd_restart(state, server_id, &request.input, &cancel).await,
        PACKAGE_INSPECT => package_inspect(state, server_id, &request.input, &cancel).await,
        PACKAGE_INSTALL => package_install(state, server_id, &request.input, &cancel).await,
        FILESYSTEM_READ => filesystem_read(state, server_id, &request.input, &cancel).await,
        FILESYSTEM_WRITE => filesystem_write(state, server_id, &request.input, &cancel).await,
        FILESYSTEM_EDIT => filesystem_edit(state, server_id, &request.input, &cancel).await,
        FILESYSTEM_BACKUP_LIST => {
            filesystem_backup_list(state, server_id, &request.input, &request).await
        }
        FILESYSTEM_BACKUP_RETENTION => {
            filesystem_backup_retention(state, server_id, &request.input, &request).await
        }
        FILESYSTEM_BACKUP => {
            filesystem_backup(state, server_id, &request.input, &cancel, &request).await
        }
        FILESYSTEM_BACKUP_CLEANUP => {
            filesystem_backup_cleanup(state, server_id, &request.input, &cancel, &request).await
        }
        FILESYSTEM_RESTORE => {
            filesystem_restore(state, server_id, &request.input, &cancel, &request).await
        }
        other => Ok(failed(
            "not_found",
            format!("host tool `{other}` is not enabled"),
            false,
            None,
        )),
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
