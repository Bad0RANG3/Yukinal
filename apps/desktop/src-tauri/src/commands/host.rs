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
    trace_id: String,
    tool_name: String,
    input: Value,
    target: HostToolTarget,
    task_id: Option<String>,
    plan_id: Option<String>,
    plan_step_id: Option<String>,
    evidence_ids: Option<Vec<String>>,
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
/// trustworthy risk tier (ADR 0014).
fn requires_host_tool_idempotency(
    state: &AppState,
    request: &HostToolExecuteRequest,
) -> Result<bool, String> {
    if is_effectful_host_tool(&request.tool_name) {
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

fn is_effectful_host_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        DOCKER_RESTART
            | SYSTEMD_RESTART
            | PACKAGE_INSTALL
            | FILESYSTEM_WRITE
            | FILESYSTEM_EDIT
            | FILESYSTEM_BACKUP
            | FILESYSTEM_BACKUP_CLEANUP
            | FILESYSTEM_RESTORE
    ) || mcp::is_mcp_tool_name(tool_name)
}

/// Every Agent-side effectful call must carry all three durable identifiers.
///
/// The interactive terminal is deliberately outside this function: it is a user-operated
/// manual session, not an Agent tool, and therefore cannot become an accidental autonomous
/// write path by omitting plan metadata.
fn effectful_tool_requires_durable_plan(request: &HostToolExecuteRequest) -> bool {
    is_effectful_host_tool(&request.tool_name)
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
    )
}

fn host_tool_request_fingerprint(request: &HostToolExecuteRequest) -> Result<String, String> {
    let canonical = json!({
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

fn prepare_host_tool_call(
    state: &AppState,
    request: &HostToolExecuteRequest,
) -> Result<HostToolCallDecision, String> {
    if !requires_host_tool_idempotency(state, request)? {
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
struct FilesystemBackupCleanupInput {
    path: String,
    backup_path: String,
    expected_revision: String,
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

    if effectful_tool_requires_durable_plan(&request) {
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
        let token = match prepare_host_tool_call(state, &request)? {
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

    let token = match prepare_host_tool_call(state, &request)? {
        HostToolCallDecision::Respond(response) => return Ok(response),
        HostToolCallDecision::Execute(token) => token,
    };
    let response = match request.tool_name.as_str() {
        SERVER_INFO => server_info(state, server_id, &request.input, &cancel).await,
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

/// Persist one already-redacted read-only result for an existing investigation task.
///
/// The sidecar prepares the envelope so it can classify and bound the tool output. The host
/// remains the authority on task existence, target scope and the final database validation.
fn handle_evidence_record(state: &AppState, params: Value) -> Result<Value, String> {
    let evidence = match serde_json::from_value::<Evidence>(
        params.get("evidence").cloned().unwrap_or(Value::Null),
    ) {
        Ok(evidence) => evidence,
        Err(error) => {
            return Ok(evidence_failure(
                "invalid_input",
                format!("invalid evidence: {error}"),
                false,
            ))
        }
    };

    let task = match state.database.investigations().get_task(&evidence.task_id) {
        Ok(task) => task,
        Err(DatabaseError::NotFound) => {
            return Ok(evidence_failure(
                "not_found",
                format!("investigation task `{}` was not found", evidence.task_id),
                false,
            ))
        }
        Err(error) => return Ok(evidence_failure("internal", error.to_string(), false)),
    };

    if !same_investigation_scope(&task, &evidence) {
        return Ok(evidence_failure(
            "denied_by_policy",
            "evidence target does not match the investigation task scope",
            false,
        ));
    }

    // The sidecar may include a run id for context, but only the host knows which
    // investigation run is currently active. Overwrite the untrusted value so scheduled
    // samples cannot be attributed to another run by model output.
    let mut evidence = evidence;
    evidence.run_id = task.active_run_id.clone();

    // A model may repeat the same read or retry the persistence call after a
    // transport hiccup. Collapse only an identical observation inside the
    // currently active durable run; separate scheduled runs must retain their
    // own samples for comparison.
    if let Err(error) = state.database.investigations().validate_evidence(&evidence) {
        return match error {
            DatabaseError::Validation(message) => {
                Ok(evidence_failure("invalid_input", message, false))
            }
            error => Ok(evidence_failure("internal", error.to_string(), false)),
        };
    }

    match state
        .database
        .investigations()
        .find_evidence_in_run(&evidence)
    {
        Ok(Some(existing)) => {
            return Ok(json!({
                "recorded": true,
                "evidenceId": existing.id,
                "reused": true,
            }));
        }
        Ok(None) => {}
        Err(error) => return Ok(evidence_failure("internal", error.to_string(), false)),
    }

    match state.database.investigations().add_evidence(&evidence) {
        Ok(()) => Ok(json!({
            "recorded": true,
            "evidenceId": evidence.id,
            "reused": false,
        })),
        Err(DatabaseError::Validation(message)) => {
            Ok(evidence_failure("invalid_input", message, false))
        }
        Err(error) => Ok(evidence_failure("internal", error.to_string(), false)),
    }
}

fn handle_evidence_fetch(state: &AppState, params: Value) -> Result<Value, String> {
    let request = serde_json::from_value::<HostEvidenceFetchRequest>(params)
        .map_err(|error| format!("invalid evidence fetch request: {error}"))?;
    if request.task_id.trim().is_empty() || request.evidence_id.trim().is_empty() {
        return Ok(json!({
            "status": "failed",
            "error": { "code": "invalid_input", "message": "taskId and evidenceId are required", "retryable": false }
        }));
    }
    let evidence = match state
        .database
        .investigations()
        .get_evidence(&request.evidence_id)
    {
        Ok(evidence) => evidence,
        Err(DatabaseError::NotFound) => return Ok(json!({ "status": "not_found" })),
        Err(error) => {
            return Ok(json!({
                "status": "failed",
                "error": { "code": "internal", "message": error.to_string(), "retryable": false }
            }))
        }
    };
    if evidence.task_id != request.task_id {
        return Ok(json!({
            "status": "failed",
            "error": { "code": "denied_by_policy", "message": "evidence does not belong to the current investigation task", "retryable": false }
        }));
    }
    Ok(json!({
        "status": "success",
        "evidence": evidence_json_with_freshness_at(&evidence, yukinal_time::now_epoch_seconds()),
    }))
}

fn handle_evidence_search(state: &AppState, params: Value) -> Result<Value, String> {
    let request = match serde_json::from_value::<HostEvidenceSearchRequest>(params) {
        Ok(request) => request,
        Err(error) => {
            return Ok(json!({
                "status": "failed",
                "error": { "code": "invalid_input", "message": format!("invalid evidence search request: {error}"), "retryable": false }
            }))
        }
    };
    if request.task_id.trim().is_empty() || request.task_id.len() > 256 {
        return Ok(json!({
            "status": "failed",
            "error": { "code": "invalid_input", "message": "taskId must be between 1 and 256 characters", "retryable": false }
        }));
    }
    let limit = request.limit.unwrap_or(32);
    if !(1..=MAX_EVIDENCE_SEARCH_LIMIT).contains(&limit) {
        return Ok(json!({
            "status": "failed",
            "error": { "code": "invalid_input", "message": format!("limit must be between 1 and {MAX_EVIDENCE_SEARCH_LIMIT}"), "retryable": false }
        }));
    }
    if request.source_tool.as_deref().is_some_and(|source| {
        source.trim().is_empty()
            || source.chars().count() > 256
            || source.chars().any(char::is_control)
    }) {
        return Ok(json!({
            "status": "failed",
            "error": { "code": "invalid_input", "message": "sourceTool must be between 1 and 256 visible characters", "retryable": false }
        }));
    }
    for timestamp in [request.from.as_deref(), request.to.as_deref()]
        .into_iter()
        .flatten()
    {
        if yukinal_time::parse_iso8601_utc(timestamp).is_none() {
            return Ok(json!({
                "status": "failed",
                "error": { "code": "invalid_input", "message": "from/to must be UTC ISO-8601 timestamps", "retryable": false }
            }));
        }
    }
    if let (Some(from), Some(to)) = (request.from.as_deref(), request.to.as_deref()) {
        if yukinal_time::parse_iso8601_utc(from) > yukinal_time::parse_iso8601_utc(to) {
            return Ok(json!({
                "status": "failed",
                "error": { "code": "invalid_input", "message": "from must not be after to", "retryable": false }
            }));
        }
    }
    let task = match state.database.investigations().get_task(&request.task_id) {
        Ok(task) => task,
        Err(DatabaseError::NotFound) => {
            return Ok(json!({
                "status": "failed",
                "error": { "code": "not_found", "message": "investigation task was not found", "retryable": false }
            }))
        }
        Err(error) => {
            return Ok(json!({
                "status": "failed",
                "error": { "code": "internal", "message": error.to_string(), "retryable": false }
            }))
        }
    };
    let scope = match request.target.as_ref() {
        Some(target) => match host_target_to_investigation(target) {
            Ok(scope) if same_investigation_target(&task.scope, &scope) => scope,
            Ok(_) => {
                return Ok(json!({
                    "status": "failed",
                    "error": { "code": "denied_by_policy", "message": "evidence target does not match the investigation task scope", "retryable": false }
                }))
            }
            Err(error) => {
                return Ok(json!({
                    "status": "failed",
                    "error": { "code": "invalid_input", "message": error, "retryable": false }
                }))
            }
        },
        None => task.scope.clone(),
    };
    let evidence = match state.database.investigations().search_evidence(
        &task.id,
        &EvidenceSearchQuery {
            source_tool: request.source_tool,
            kind: request.kind,
            from: request.from,
            to: request.to,
            scope: Some(scope),
            limit,
        },
    ) {
        Ok(evidence) => evidence,
        Err(DatabaseError::Validation(message)) => {
            return Ok(json!({
                "status": "failed",
                "error": { "code": "invalid_input", "message": message, "retryable": false }
            }))
        }
        Err(error) => {
            return Ok(json!({
                "status": "failed",
                "error": { "code": "internal", "message": error.to_string(), "retryable": false }
            }))
        }
    };
    let evaluated_at_epoch = yukinal_time::now_epoch_seconds();
    let summaries = evidence
        .iter()
        .map(|item| evidence_summary_at(item, evaluated_at_epoch))
        .collect::<Vec<_>>();
    Ok(json!({ "status": "success", "evidence": summaries }))
}

/// Return metadata that was collected with one anchor observation. A host-owned run is the
/// strongest relation; legacy/manual evidence without a run falls back to a small time window.
/// This is deliberately correlation, not a causal or semantic conclusion: the model still has
/// to inspect the returned sources and cite the underlying evidence in a Finding.
fn handle_evidence_correlation(state: &AppState, params: Value) -> Result<Value, String> {
    let request = match serde_json::from_value::<HostEvidenceCorrelationRequest>(params) {
        Ok(request) => request,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("invalid evidence correlation request: {error}"),
                false,
                None,
            ))
        }
    };
    if request.task_id.trim().is_empty()
        || request.task_id.len() > 256
        || request.anchor_evidence_id.trim().is_empty()
        || request.anchor_evidence_id.len() > 256
    {
        return Ok(failed(
            "invalid_input",
            "taskId and anchorEvidenceId must be between 1 and 256 characters",
            false,
            None,
        ));
    }
    let window_seconds = request
        .window_seconds
        .unwrap_or(DEFAULT_EVIDENCE_CORRELATION_WINDOW_SECONDS);
    if !(1..=MAX_EVIDENCE_CORRELATION_WINDOW_SECONDS).contains(&window_seconds) {
        return Ok(failed(
            "invalid_input",
            format!(
                "windowSeconds must be between 1 and {MAX_EVIDENCE_CORRELATION_WINDOW_SECONDS}"
            ),
            false,
            None,
        ));
    }
    let limit = request.limit.unwrap_or(32);
    if !(1..=MAX_EVIDENCE_SEARCH_LIMIT).contains(&limit) {
        return Ok(failed(
            "invalid_input",
            format!("limit must be between 1 and {MAX_EVIDENCE_SEARCH_LIMIT}"),
            false,
            None,
        ));
    }

    let task = match state.database.investigations().get_task(&request.task_id) {
        Ok(task) => task,
        Err(DatabaseError::NotFound) => {
            return Ok(failed(
                "not_found",
                "investigation task was not found",
                false,
                None,
            ))
        }
        Err(error) => return Ok(failed("internal", error.to_string(), false, None)),
    };
    let anchor = match state
        .database
        .investigations()
        .get_evidence(&request.anchor_evidence_id)
    {
        Ok(evidence) => evidence,
        Err(DatabaseError::NotFound) => {
            return Ok(failed(
                "not_found",
                "anchor evidence was not found in the current task",
                false,
                None,
            ))
        }
        Err(error) => return Ok(failed("internal", error.to_string(), false, None)),
    };
    if anchor.task_id != task.id || !same_investigation_scope(&task, &anchor) {
        return Ok(failed(
            "denied_by_policy",
            "anchor evidence does not belong to the current investigation scope",
            false,
            None,
        ));
    }

    let (matched_by, related) = if let Some(run_id) = anchor.run_id.as_deref() {
        let evidence = match state
            .database
            .investigations()
            .list_evidence_for_run(run_id, limit)
        {
            Ok(evidence) => evidence,
            Err(error) => return Ok(failed("internal", error.to_string(), false, None)),
        };
        ("same_run", evidence)
    } else {
        let Some(anchor_epoch) = yukinal_time::parse_iso8601_utc(&anchor.collected_at) else {
            return Ok(failed(
                "invalid_input",
                "anchor evidence has no valid UTC collection timestamp for time-window correlation",
                false,
                None,
            ));
        };
        let from = yukinal_time::iso8601_utc(anchor_epoch.saturating_sub(window_seconds));
        let to = yukinal_time::iso8601_utc(anchor_epoch.saturating_add(window_seconds));
        let evidence = match state.database.investigations().search_evidence(
            &task.id,
            &EvidenceSearchQuery {
                source_tool: None,
                kind: None,
                from: Some(from),
                to: Some(to),
                scope: Some(task.scope.clone()),
                limit,
            },
        ) {
            Ok(evidence) => evidence,
            Err(error) => return Ok(failed("internal", error.to_string(), false, None)),
        };
        ("time_window", evidence)
    };

    let mut warnings = Vec::new();
    let evaluated_at_epoch = yukinal_time::now_epoch_seconds();
    let anchor_summary = evidence_summary_at(&anchor, evaluated_at_epoch);
    if anchor_summary.freshness.status != "fresh" {
        warnings.push(format!(
            "anchor evidence is {} under default-v1 freshness",
            anchor_summary.freshness.status
        ));
    }
    if anchor.truncated {
        warnings.push("anchor evidence was truncated at collection".into());
    }
    let mut source_tools = BTreeSet::new();
    let summaries = related
        .into_iter()
        .filter(|evidence| evidence.task_id == task.id && same_investigation_scope(&task, evidence))
        .take(limit)
        .map(|evidence| {
            source_tools.insert(evidence.source_tool.clone());
            let summary = evidence_summary_at(&evidence, evaluated_at_epoch);
            if summary.freshness.status != "fresh" && warnings.len() < 8 {
                warnings.push(format!(
                    "{} evidence is {} under default-v1 freshness",
                    evidence.source_tool, summary.freshness.status
                ));
            }
            if evidence.truncated && warnings.len() < 8 {
                warnings.push(format!(
                    "{} evidence was truncated at collection",
                    evidence.source_tool
                ));
            }
            summary
        })
        .collect::<Vec<_>>();
    source_tools.insert(anchor.source_tool.clone());
    if summaries.is_empty() {
        warnings.push("no other evidence matched the host-owned run or time window".into());
    }
    warnings.truncate(8);
    Ok(json!({
        "status": "success",
        "correlation": {
            "anchor": anchor_summary,
            "evidence": summaries,
            "matchedBy": matched_by,
            "windowSeconds": window_seconds,
            "sourceTools": source_tools.into_iter().take(32).collect::<Vec<_>>(),
            "warnings": warnings,
        },
    }))
}

fn evidence_summary_at(evidence: &Evidence, evaluated_at_epoch: u64) -> EvidenceSummary {
    EvidenceSummary {
        freshness: evidence_freshness_at(&evidence.collected_at, evaluated_at_epoch),
        id: evidence.id.clone(),
        task_id: evidence.task_id.clone(),
        run_id: evidence.run_id.clone(),
        scope: evidence.scope.clone(),
        kind: evidence.kind,
        source_tool: evidence.source_tool.clone(),
        collected_at: evidence.collected_at.clone(),
        input_summary: evidence.input_summary.clone(),
        content_type: evidence.content_type,
        content_hash: evidence.content_hash.clone(),
        truncated: evidence.truncated,
        redaction_status: evidence.redaction_status,
    }
}

/// Compare two persisted observations without returning either body. The model can ask for a
/// single body explicitly through `investigation.evidence`, but routine anomaly explanation gets
/// only host-derived paths/counts so a comparison cannot accidentally flood context or bypass the
/// evidence boundary.
fn handle_evidence_compare(state: &AppState, params: Value) -> Result<Value, String> {
    let request = match serde_json::from_value::<HostEvidenceCompareRequest>(params) {
        Ok(request) => request,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("invalid evidence comparison request: {error}"),
                false,
                None,
            ))
        }
    };
    if request.task_id.trim().is_empty()
        || request.task_id.len() > 256
        || request.left_evidence_id.trim().is_empty()
        || request.right_evidence_id.trim().is_empty()
        || request.left_evidence_id.len() > 256
        || request.right_evidence_id.len() > 256
    {
        return Ok(failed(
            "invalid_input",
            "taskId and both evidence ids must be between 1 and 256 characters",
            false,
            None,
        ));
    }
    let task = match state.database.investigations().get_task(&request.task_id) {
        Ok(task) => task,
        Err(DatabaseError::NotFound) => {
            return Ok(failed(
                "not_found",
                "investigation task was not found",
                false,
                None,
            ))
        }
        Err(error) => return Ok(failed("internal", error.to_string(), false, None)),
    };
    let load = |id: &str, side: &str| -> Result<Evidence, Value> {
        match state.database.investigations().get_evidence(id) {
            Ok(evidence) => {
                if evidence.task_id != task.id || !same_investigation_scope(&task, &evidence) {
                    Err(failed(
                        "denied_by_policy",
                        format!(
                            "{side} evidence does not belong to the current investigation scope"
                        ),
                        false,
                        None,
                    ))
                } else {
                    Ok(evidence)
                }
            }
            Err(DatabaseError::NotFound) => Err(failed(
                "not_found",
                format!("{side} evidence was not found in the current task"),
                false,
                None,
            )),
            Err(error) => Err(failed("internal", error.to_string(), false, None)),
        }
    };
    let left = match load(&request.left_evidence_id, "left") {
        Ok(evidence) => evidence,
        Err(response) => return Ok(response),
    };
    let right = match load(&request.right_evidence_id, "right") {
        Ok(evidence) => evidence,
        Err(response) => return Ok(response),
    };
    let evaluated_at_epoch = yukinal_time::now_epoch_seconds();
    let left_summary = evidence_summary_at(&left, evaluated_at_epoch);
    let right_summary = evidence_summary_at(&right, evaluated_at_epoch);
    let status = if left.content_hash == right.content_hash {
        "identical"
    } else {
        "changed"
    };
    let shape = match (left.content_type, right.content_type) {
        (EvidenceContentType::Json, EvidenceContentType::Json) => "json",
        (EvidenceContentType::Text, EvidenceContentType::Text) => "text",
        _ => "mixed",
    };
    let mut warnings = Vec::new();
    if left.truncated || right.truncated {
        warnings.push("one or both evidence bodies were truncated at collection".into());
    }
    if left.source_tool != right.source_tool || left.kind != right.kind {
        warnings.push("source or evidence kind differs; interpret correlation cautiously".into());
    }
    for (side, freshness) in [
        ("left", &left_summary.freshness),
        ("right", &right_summary.freshness),
    ] {
        if freshness.status != "fresh" {
            warnings.push(format!(
                "{side} evidence is {} under default-v1 freshness",
                freshness.status
            ));
        }
    }

    let (changed_paths, changed_path_count, diff_truncated, text) = if status == "identical" {
        (Vec::new(), 0, false, None)
    } else {
        match (left.content_type, right.content_type) {
            (EvidenceContentType::Json, EvidenceContentType::Json) => {
                let mut diff = JsonDiff::default();
                collect_json_diff(&left.content, &right.content, "$", 0, &mut diff);
                (diff.paths, diff.count, diff.truncated, None)
            }
            (EvidenceContentType::Text, EvidenceContentType::Text) => {
                let (text, truncated) = compare_text_content(&left.content, &right.content);
                (Vec::new(), 0, truncated, text)
            }
            _ => (vec!["$".into()], 1, false, None),
        }
    };
    if diff_truncated {
        warnings.push(
            "comparison details were bounded; inspect individual evidence ids for more context"
                .into(),
        );
    }
    if warnings.len() > 8 {
        warnings.truncate(8);
    }
    Ok(json!({
        "status": "success",
        "comparison": EvidenceComparison {
            status,
            shape,
            left: left_summary,
            right: right_summary,
            changed_paths,
            changed_path_count,
            diff_truncated,
            text,
            warnings,
        },
    }))
}

/// Report local retention candidates to the Agent without giving it a delete capability.
/// The destructive IPC command remains a separate, user-confirmed desktop action.
fn handle_retention_preview(state: &AppState, params: Value) -> Result<Value, String> {
    let request = match serde_json::from_value::<HostRetentionPreviewRequest>(params) {
        Ok(request) => request,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("invalid retention preview request: {error}"),
                false,
                None,
            ))
        }
    };
    if request.task_id.trim().is_empty()
        || request.task_id.chars().count() > 256
        || request.task_id.chars().any(char::is_control)
    {
        return Ok(failed(
            "invalid_input",
            "taskId must be between 1 and 256 visible characters",
            false,
            None,
        ));
    }
    if request.cutoff_at.as_deref().is_some_and(|cutoff| {
        cutoff.trim().is_empty()
            || cutoff.chars().count() > 80
            || cutoff.chars().any(char::is_control)
    }) {
        return Ok(failed(
            "invalid_input",
            "cutoffAt must be a bounded UTC ISO-8601 timestamp",
            false,
            None,
        ));
    }
    let cutoff_at = request.cutoff_at.unwrap_or_else(|| {
        yukinal_time::iso8601_utc(
            yukinal_time::now_epoch_seconds().saturating_sub(DEFAULT_RETENTION_DAYS * 86_400),
        )
    });
    if yukinal_time::parse_iso8601_utc(&cutoff_at).is_none() {
        return Ok(failed(
            "invalid_input",
            "cutoffAt must be a UTC ISO-8601 timestamp",
            false,
            None,
        ));
    }
    let limit = request.limit.unwrap_or(DEFAULT_RETENTION_LIMIT);
    if !(1..=MAX_RETENTION_LIMIT).contains(&limit) {
        return Ok(failed(
            "invalid_input",
            format!("limit must be between 1 and {MAX_RETENTION_LIMIT}"),
            false,
            None,
        ));
    }
    let preview =
        match state
            .database
            .investigation_retention()
            .preview(&request.task_id, &cutoff_at, limit)
        {
            Ok(preview) => preview,
            Err(DatabaseError::NotFound) => {
                return Ok(failed(
                    "not_found",
                    "investigation task was not found",
                    false,
                    None,
                ))
            }
            Err(DatabaseError::Validation(message)) => {
                return Ok(failed("invalid_input", message, false, None))
            }
            Err(error) => return Ok(failed("internal", error.to_string(), false, None)),
        };
    Ok(json!({
        "status": "success",
        "preview": retention_preview_json(preview),
    }))
}

fn retention_preview_json(preview: InvestigationRetentionPreview) -> Value {
    json!({
        "taskId": preview.task_id,
        "cutoffAt": preview.cutoff_at,
        "candidates": preview.candidates.into_iter().map(|item| json!({
            "id": item.id,
            "taskId": item.task_id,
            "kind": item.kind.as_str(),
            "createdAt": item.created_at,
            "bytes": item.bytes,
            "reason": item.reason,
        })).collect::<Vec<_>>(),
        "protectedCount": preview.protected_count,
        "candidateBytes": preview.candidate_bytes,
        "truncated": preview.truncated,
    })
}

const MAX_COMPARE_PATHS: usize = 64;
const MAX_COMPARE_PATH_COUNT: usize = 10_000;
const MAX_COMPARE_NODES: usize = 4_096;
const MAX_COMPARE_DEPTH: usize = 8;
const MAX_COMPARE_LINES: usize = 100_000;

#[derive(Default)]
struct JsonDiff {
    paths: Vec<String>,
    count: usize,
    nodes: usize,
    truncated: bool,
}

fn collect_json_diff(left: &Value, right: &Value, path: &str, depth: usize, diff: &mut JsonDiff) {
    if left == right {
        return;
    }
    if diff.nodes >= MAX_COMPARE_NODES {
        diff.truncated = true;
        return;
    }
    diff.nodes += 1;
    if depth >= MAX_COMPARE_DEPTH || diff.count >= MAX_COMPARE_PATH_COUNT {
        record_json_diff_path(path, diff);
        diff.truncated = true;
        return;
    }
    match (left, right) {
        (Value::Object(left), Value::Object(right)) => {
            let keys = left
                .keys()
                .chain(right.keys())
                .cloned()
                .collect::<BTreeSet<_>>();
            for key in keys {
                let child_path = if key.chars().all(|character| {
                    character.is_ascii_alphanumeric() || character == '_' || character == '-'
                }) {
                    format!("{path}.{key}")
                } else {
                    format!("{path}[{key:?}]")
                };
                match (left.get(&key), right.get(&key)) {
                    (Some(left), Some(right)) => {
                        collect_json_diff(left, right, &child_path, depth + 1, diff)
                    }
                    _ => record_json_diff_path(&child_path, diff),
                }
                if diff.truncated && diff.nodes >= MAX_COMPARE_NODES {
                    break;
                }
            }
        }
        (Value::Array(left), Value::Array(right)) => {
            for index in 0..left.len().max(right.len()) {
                let child_path = format!("{path}[{index}]");
                match (left.get(index), right.get(index)) {
                    (Some(left), Some(right)) => {
                        collect_json_diff(left, right, &child_path, depth + 1, diff)
                    }
                    _ => record_json_diff_path(&child_path, diff),
                }
                if diff.truncated && diff.nodes >= MAX_COMPARE_NODES {
                    break;
                }
            }
        }
        _ => record_json_diff_path(path, diff),
    }
}

fn record_json_diff_path(path: &str, diff: &mut JsonDiff) {
    diff.count = diff.count.saturating_add(1).min(MAX_COMPARE_PATH_COUNT);
    if diff.paths.len() < MAX_COMPARE_PATHS {
        diff.paths.push(path.chars().take(512).collect());
    } else {
        diff.truncated = true;
    }
}

fn compare_text_content(left: &Value, right: &Value) -> (Option<EvidenceTextComparison>, bool) {
    let (Some(left), Some(right)) = (left.as_str(), right.as_str()) else {
        return (None, false);
    };
    let left_lines = left
        .split('\n')
        .take(MAX_COMPARE_LINES + 1)
        .collect::<Vec<_>>();
    let right_lines = right
        .split('\n')
        .take(MAX_COMPARE_LINES + 1)
        .collect::<Vec<_>>();
    let truncated = left_lines.len() > MAX_COMPARE_LINES || right_lines.len() > MAX_COMPARE_LINES;
    let left_count = left_lines.len().min(MAX_COMPARE_LINES);
    let right_count = right_lines.len().min(MAX_COMPARE_LINES);
    let paired_changes = left_lines
        .iter()
        .zip(right_lines.iter())
        .take(MAX_COMPARE_LINES)
        .filter(|(left, right)| left != right)
        .count();
    let changed_line_count = paired_changes + left_count.abs_diff(right_count);
    (
        Some(EvidenceTextComparison {
            left_line_count: left_count,
            right_line_count: right_count,
            changed_line_count,
            added_line_count: right_count.saturating_sub(left_count),
            removed_line_count: left_count.saturating_sub(right_count),
        }),
        truncated,
    )
}

fn handle_finding_record(state: &AppState, params: Value) -> Result<Value, String> {
    let finding = match serde_json::from_value::<Finding>(
        params.get("finding").cloned().unwrap_or(Value::Null),
    ) {
        Ok(finding) => finding,
        Err(error) => {
            return Ok(record_failure(
                "invalid_input",
                format!("invalid finding: {error}"),
                false,
            ))
        }
    };
    let task = match state.database.investigations().get_task(&finding.task_id) {
        Ok(task) => task,
        Err(DatabaseError::NotFound) => {
            return Ok(record_failure(
                "not_found",
                "investigation task was not found",
                false,
            ))
        }
        Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
    };
    if finding.evidence_ids.is_empty() && finding.kind != FindingKind::Unknown {
        return Ok(record_failure(
            "invalid_input",
            "fact and inference findings require at least one evidence reference",
            false,
        ));
    }
    for evidence_id in &finding.evidence_ids {
        match state.database.investigations().get_evidence(evidence_id) {
            Ok(evidence) if evidence.task_id == task.id => {}
            Ok(_) => {
                return Ok(record_failure(
                    "denied_by_policy",
                    "finding references evidence from another task",
                    false,
                ))
            }
            Err(DatabaseError::NotFound) => {
                return Ok(record_failure(
                    "invalid_input",
                    format!("evidence `{evidence_id}` was not found"),
                    false,
                ))
            }
            Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
        }
    }
    match state.database.investigations().add_finding(&finding) {
        Ok(()) => Ok(json!({ "recorded": true, "finding": finding })),
        Err(DatabaseError::Validation(message)) => {
            Ok(record_failure("invalid_input", message, false))
        }
        Err(error) => Ok(record_failure("internal", error.to_string(), false)),
    }
}

fn handle_brief_record(state: &AppState, params: Value) -> Result<Value, String> {
    let brief = match serde_json::from_value::<DecisionBrief>(
        params.get("brief").cloned().unwrap_or(Value::Null),
    ) {
        Ok(brief) => brief,
        Err(error) => {
            return Ok(record_failure(
                "invalid_input",
                format!("invalid decision brief: {error}"),
                false,
            ))
        }
    };
    let task = match state.database.investigations().get_task(&brief.task_id) {
        Ok(task) => task,
        Err(DatabaseError::NotFound) => {
            return Ok(record_failure(
                "not_found",
                "investigation task was not found",
                false,
            ))
        }
        Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
    };
    if let Some(plan_id) = brief.plan_id.as_deref() {
        let current_plan = state
            .database
            .investigations()
            .latest_plan(&task.id)
            .map_err(|error| error.to_string())?;
        if current_plan.as_ref().map(|plan| plan.id.as_str()) != Some(plan_id) {
            return Ok(record_failure(
                "plan_deviation",
                "decision brief must describe the current plan revision",
                false,
            ));
        }
    }
    for finding_id in brief.finding_ids.iter().chain(
        brief
            .options
            .iter()
            .flat_map(|option| option.finding_ids.iter()),
    ) {
        match state.database.investigations().get_finding(finding_id) {
            Ok(finding) if finding.task_id == task.id => {}
            Ok(_) => {
                return Ok(record_failure(
                    "denied_by_policy",
                    "decision brief references a finding from another task",
                    false,
                ))
            }
            Err(DatabaseError::NotFound) => {
                return Ok(record_failure(
                    "invalid_input",
                    format!("finding `{finding_id}` was not found"),
                    false,
                ))
            }
            Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
        }
    }
    for evidence_id in brief
        .options
        .iter()
        .flat_map(|option| option.evidence_ids.iter())
    {
        match state.database.investigations().get_evidence(evidence_id) {
            Ok(evidence) if evidence.task_id == task.id => {}
            Ok(_) => {
                return Ok(record_failure(
                    "denied_by_policy",
                    "decision brief references evidence from another task",
                    false,
                ))
            }
            Err(DatabaseError::NotFound) => {
                return Ok(record_failure(
                    "invalid_input",
                    format!("evidence `{evidence_id}` was not found"),
                    false,
                ))
            }
            Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
        }
    }
    match state.database.investigations().save_decision_brief(&brief) {
        Ok(()) => Ok(json!({ "recorded": true, "brief": brief })),
        Err(DatabaseError::Validation(message)) => {
            Ok(record_failure("invalid_input", message, false))
        }
        Err(error) => Ok(record_failure("internal", error.to_string(), false)),
    }
}

fn handle_plan_record(state: &AppState, params: Value) -> Result<Value, String> {
    let mut plan = match serde_json::from_value::<InvestigationPlan>(
        params.get("plan").cloned().unwrap_or(Value::Null),
    ) {
        Ok(plan) => plan,
        Err(error) => {
            return Ok(record_failure(
                "invalid_input",
                format!("invalid investigation plan: {error}"),
                false,
            ))
        }
    };
    let task = match state.database.investigations().get_task(&plan.task_id) {
        Ok(task) => task,
        Err(DatabaseError::NotFound) => {
            return Ok(record_failure(
                "not_found",
                "investigation task was not found",
                false,
            ))
        }
        Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
    };
    if plan.steps.is_empty() {
        return Ok(record_failure(
            "invalid_input",
            "investigation plan needs at least one step",
            false,
        ));
    }
    if plan.steps.len() > 64 {
        return Ok(record_failure(
            "invalid_input",
            "investigation plan has too many steps",
            false,
        ));
    }
    let previous = state
        .database
        .investigations()
        .latest_plan(&task.id)
        .map_err(|error| error.to_string())?;
    // A plan id is an idempotency key, not a mutable approval handle. If an old
    // (possibly superseded) id is replayed, reject it instead of reopening that
    // row behind the current active plan and risking a unique-index collision.
    let existing_by_id = match state.database.investigations().get_plan(&plan.id) {
        Ok(existing) => Some(existing),
        Err(DatabaseError::NotFound) => None,
        Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
    };
    let same_plan = previous
        .as_ref()
        .is_some_and(|candidate| candidate.id == plan.id);
    if existing_by_id.is_some() && !same_plan {
        return Ok(record_failure(
            "plan_deviation",
            "plan id already belongs to an older revision; create a new plan id",
            false,
        ));
    }
    if let (Some(previous_plan), Some(existing)) = (previous.as_ref(), existing_by_id.as_ref()) {
        if previous_plan.id != existing.id
            || plan_definition(previous_plan)? != plan_definition(&plan)?
        {
            return Ok(record_failure(
                "plan_deviation",
                "an existing plan id cannot be reused with a different definition; create a new plan id",
                false,
            ));
        }
        merge_plan_runtime(&mut plan, previous_plan);
    } else {
        plan.revision = previous
            .as_ref()
            .map(|candidate| candidate.revision.saturating_add(1))
            .unwrap_or(1);
        plan.status = yukinal_database::models::PlanStatus::Active;
        plan.approval = Some(InvestigationPlanApproval {
            status: PlanApprovalStatus::Pending,
            source: None,
            option_id: None,
            approved_at: None,
            note: None,
        });
    }
    if let Some(window) = plan.observation_window.as_mut() {
        if let Some(error) = validate_observation_window(window, &plan.steps) {
            return Ok(record_failure("invalid_input", error, false));
        }
        let preserve_state = previous
            .as_ref()
            .and_then(|candidate| candidate.observation_window.as_ref())
            .filter(|candidate| observation_configuration_matches(candidate, window))
            .cloned();
        if let Some(previous_window) = preserve_state {
            *window = previous_window;
        } else {
            reset_observation_window(window);
        }
    }
    if plan.current_step_id.is_none() && !same_plan {
        plan.current_step_id = plan.steps.first().map(|step| step.id.clone());
    }
    let mut ids = std::collections::HashSet::new();
    for (index, step) in plan.steps.iter().enumerate() {
        if !ids.insert(step.id.as_str()) {
            return Ok(record_failure(
                "invalid_input",
                "investigation plan step ids must be unique",
                false,
            ));
        }
        if step.ordinal != index as u32 {
            return Ok(record_failure(
                "invalid_input",
                "investigation plan step ordinals must be consecutive",
                false,
            ));
        }
        if step.allowed_tools.is_empty() {
            return Ok(record_failure(
                "invalid_input",
                format!("plan step `{}` must allow at least one tool", step.id),
                false,
            ));
        }
        if let Some(error) = validate_playbook_step(step) {
            return Ok(record_failure("invalid_input", error, false));
        }
        if step.kind == PlanStepKind::Action
            && !step.requires_approval
            && !task_allows_auto_medium_action(&task, step)
        {
            return Ok(record_failure(
                "denied_by_policy",
                format!(
                    "action step `{}` may omit approval only for an auto executable goal on a remote development or staging target, and only at medium risk",
                    step.id
                ),
                false,
            ));
        }
        if let Some(error) = validate_input_bindings(step) {
            return Ok(record_failure("invalid_input", error, false));
        }
        if let Some(error) = validate_task_guardrails_for_plan_step(&task, step) {
            return Ok(record_failure("denied_by_policy", error, false));
        }
        if let Some(target) = &step.target {
            if !same_investigation_target(&task.scope, target) {
                return Ok(record_failure(
                    "denied_by_policy",
                    format!("plan step `{}` expands the task target scope", step.id),
                    false,
                ));
            }
        }
        for evidence_id in &step.evidence_ids {
            match state.database.investigations().get_evidence(evidence_id) {
                Ok(evidence) if evidence.task_id == task.id => {}
                Ok(_) => {
                    return Ok(record_failure(
                        "denied_by_policy",
                        format!(
                            "plan step `{}` references evidence from another task",
                            step.id
                        ),
                        false,
                    ))
                }
                Err(DatabaseError::NotFound) => {
                    return Ok(record_failure(
                        "evidence_missing",
                        format!("evidence `{evidence_id}` was not found",),
                        true,
                    ))
                }
                Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
            }
        }
    }
    if let Some(current_step_id) = plan.current_step_id.as_deref() {
        if !ids.contains(current_step_id) {
            return Ok(record_failure(
                "invalid_input",
                "currentStepId is not a plan step",
                false,
            ));
        }
    }
    if !same_plan {
        normalize_new_plan_runtime(&mut plan, &yukinal_core::sidecar::iso8601_now());
    }
    match state.database.investigations().save_plan(&plan) {
        Ok(()) => {
            let evidence_ids = plan
                .steps
                .iter()
                .flat_map(|step| step.evidence_ids.iter().cloned())
                .collect::<Vec<_>>();
            let change_plan = InvestigationArtifact {
                id: format!("artifact_change_plan_{}", plan.id),
                task_id: task.id.clone(),
                run_id: task.active_run_id.clone(),
                plan_id: Some(plan.id.clone()),
                plan_step_id: plan.current_step_id.clone(),
                phase: TaskPhase::Decision,
                kind: TaskArtifactKind::ChangePlan,
                status: TaskArtifactStatus::Ready,
                title: "待审批的变更计划".into(),
                summary: format!(
                    "计划修订 {} 已保存；行动步骤只会在范围、基线和批准条件满足后执行。",
                    plan.revision
                ),
                content: serde_json::to_value(&plan).map_err(|error| error.to_string())?,
                evidence_ids,
                created_at: plan.created_at.clone(),
                updated_at: plan.updated_at.clone(),
            };
            state
                .database
                .investigations()
                .upsert_artifact(&change_plan)
                .map_err(|error| error.to_string())?;
            Ok(json!({ "recorded": true, "plan": plan }))
        }
        Err(DatabaseError::Validation(message)) => {
            Ok(record_failure("invalid_input", message, false))
        }
        Err(error) => Ok(record_failure("internal", error.to_string(), false)),
    }
}

/// Return only the parts of a plan that require a fresh approval. Runtime
/// progress (attempts, step status, timestamps, deviations and observation
/// samples) is host-owned and deliberately excluded so an idempotent replay
/// cannot reset or widen an already-approved plan.
fn plan_definition(plan: &InvestigationPlan) -> Result<Value, String> {
    let mut value = serde_json::to_value(plan).map_err(|error| error.to_string())?;
    let Some(object) = value.as_object_mut() else {
        return Err("investigation plan must serialize as an object".into());
    };
    for key in [
        "revision",
        "status",
        "createdAt",
        "updatedAt",
        "currentStepId",
        "approval",
    ] {
        object.remove(key);
    }
    if let Some(steps) = object.get_mut("steps").and_then(Value::as_array_mut) {
        for step in steps {
            if let Some(step_object) = step.as_object_mut() {
                for key in [
                    "attempts",
                    "status",
                    "startedAt",
                    "endedAt",
                    "lastDeviation",
                ] {
                    step_object.remove(key);
                }
            }
        }
    }
    if let Some(window) = object
        .get_mut("observationWindow")
        .and_then(Value::as_object_mut)
    {
        for key in [
            "status",
            "sampleCount",
            "startedAt",
            "deadlineAt",
            "deadlineEpochSeconds",
            "lastSampleAt",
            "lastSampleEpochSeconds",
            "lastFailure",
        ] {
            window.remove(key);
        }
    }
    Ok(value)
}

/// Merge only host-owned progress back into an idempotent plan replay. The
/// caller has already proved that the declarative definition is byte-equivalent
/// (as structured JSON), so preserving the persisted state cannot hide a
/// changed target, command binding or approval-relevant field.
fn merge_plan_runtime(plan: &mut InvestigationPlan, previous: &InvestigationPlan) {
    plan.revision = previous.revision;
    plan.status = previous.status;
    plan.created_at = previous.created_at.clone();
    plan.current_step_id = previous.current_step_id.clone();
    plan.approval = previous.approval.clone();
    for step in &mut plan.steps {
        if let Some(previous_step) = previous
            .steps
            .iter()
            .find(|candidate| candidate.id == step.id)
        {
            step.attempts = previous_step.attempts;
            step.status = previous_step.status;
            step.started_at = previous_step.started_at.clone();
            step.ended_at = previous_step.ended_at.clone();
            step.last_deviation = previous_step.last_deviation.clone();
        }
    }
}

/// Runtime fields on a newly proposed plan are untrusted input. The host starts
/// the first declared step and keeps every later step pending; the model cannot
/// jump directly to an action by claiming that evidence already succeeded.
fn normalize_new_plan_runtime(plan: &mut InvestigationPlan, now: &str) {
    plan.status = yukinal_database::models::PlanStatus::Active;
    plan.current_step_id = plan.steps.first().map(|step| step.id.clone());
    plan.created_at = now.to_string();
    plan.updated_at = now.to_string();
    for (index, step) in plan.steps.iter_mut().enumerate() {
        step.attempts = 0;
        step.status = if index == 0 {
            PlanStepStatus::Running
        } else {
            PlanStepStatus::Pending
        };
        step.started_at = (index == 0).then(|| now.to_string());
        step.ended_at = None;
        step.last_deviation = None;
    }
}

fn handle_plan_check(state: &AppState, params: Value) -> Result<Value, String> {
    let request = serde_json::from_value::<HostPlanCheckRequest>(params)
        .map_err(|error| format!("invalid plan check request: {error}"))?;
    match check_plan_for_tool(
        state,
        &request.task_id,
        &request.tool_name,
        &request.target,
        PlanCheckBinding {
            input: &request.input,
            plan_id: None,
            step_id: None,
            evidence_ids: None,
        },
    )? {
        Ok(()) => {
            let plan = state
                .database
                .investigations()
                .latest_plan(&request.task_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "active plan disappeared during check".to_string())?;
            let step_id = plan
                .current_step_id
                .ok_or_else(|| "active plan has no current step".to_string())?;
            let current_step = plan
                .steps
                .iter()
                .find(|step| step.id == step_id)
                .ok_or_else(|| "active plan step disappeared during check".to_string())?;
            Ok(json!({
                "status": "allowed",
                "planId": plan.id,
                "stepId": current_step.id,
                "stepKind": current_step.kind,
                "evidenceIds": current_step.evidence_ids,
                "requiresApproval": current_step.requires_approval,
            }))
        }
        Err(deviation) => Ok(json!({ "status": "deviation", "deviation": deviation })),
    }
}

fn handle_plan_step_result(state: &AppState, params: Value) -> Result<Value, String> {
    let request = serde_json::from_value::<HostPlanStepResultRequest>(params)
        .map_err(|error| format!("invalid plan step result request: {error}"))?;
    let mut plan = match state.database.investigations().get_plan(&request.plan_id) {
        Ok(plan) => plan,
        Err(DatabaseError::NotFound) => {
            return Ok(record_failure(
                "not_found",
                "investigation plan was not found",
                false,
            ))
        }
        Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
    };
    if plan.task_id != request.task_id {
        return Ok(record_failure(
            "denied_by_policy",
            "plan does not belong to the current investigation task",
            false,
        ));
    }
    let task = match state.database.investigations().get_task(&request.task_id) {
        Ok(task) => task,
        Err(DatabaseError::NotFound) => {
            return Ok(record_failure(
                "not_found",
                "investigation task was not found",
                false,
            ))
        }
        Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
    };
    if plan.current_step_id.as_deref() != Some(request.step_id.as_str()) {
        return Ok(record_failure(
            "plan_deviation",
            "plan step result does not match the current active step",
            false,
        ));
    }
    if !matches!(request.status.as_str(), "success" | "failed" | "cancelled") {
        return Ok(record_failure(
            "invalid_input",
            "unknown plan step result status",
            false,
        ));
    }
    let now = yukinal_core::sidecar::iso8601_now();
    let Some(step_index) = plan
        .steps
        .iter()
        .position(|step| step.id == request.step_id)
    else {
        return Ok(record_failure(
            "not_found",
            "plan step was not found",
            false,
        ));
    };
    let step_kind = plan.steps[step_index].kind;
    let requires_baseline = plan.steps[step_index].requires_baseline == Some(true);
    if request.status == "success" && requires_baseline {
        let artifacts = state
            .database
            .investigations()
            .list_artifacts(&task.id, 128)
            .map_err(|error| error.to_string())?;
        if !baseline_artifact_matches_plan(&artifacts, &plan) {
            return Ok(record_failure(
                "evidence_missing",
                "action step completed without a ready baseline bound to this plan revision",
                true,
            ));
        }
    }
    if request.status == "success" && step_kind == PlanStepKind::Verification {
        let has_verification = state
            .database
            .investigations()
            .list_artifacts(&task.id, 128)
            .map_err(|error| error.to_string())?
            .iter()
            .any(|artifact| {
                artifact.kind == TaskArtifactKind::Verification
                    && artifact.status == TaskArtifactStatus::Succeeded
                    && artifact.plan_id.as_deref() == Some(plan.id.as_str())
                    && artifact.plan_step_id.as_deref() == Some(request.step_id.as_str())
            });
        if !has_verification {
            return Ok(record_failure(
                "evidence_missing",
                "verification step must record a succeeded verification artifact before it can advance",
                true,
            ));
        }
    }
    if step_kind == PlanStepKind::Verification {
        if request.status != "success"
            && plan
                .observation_window
                .as_ref()
                .is_some_and(|window| window.status == ObservationWindowStatus::Running)
        {
            let failure_message = super::safe_audit_summary(
                request
                    .output_summary
                    .as_deref()
                    .unwrap_or("观察窗口采样异常"),
                8_192,
            );
            if let Some(window) = plan.observation_window.as_mut() {
                window.status = ObservationWindowStatus::Failed;
                window.last_failure = Some(failure_message.clone());
            }
            plan.updated_at = now.clone();
            state
                .database
                .investigations()
                .save_plan(&plan)
                .map_err(|error| error.to_string())?;
            let failure = yukinal_database::models::InvestigationFailure {
                code: TaskFailureCode::CommandFailed,
                message: failure_message.clone(),
                retryable: false,
                attempt: plan.steps[step_index].attempts.saturating_add(1),
                at: now.clone(),
                detail: Some(json!({
                    "planId": plan.id,
                    "stepId": request.step_id,
                    "observation": "failed",
                    "status": request.status,
                })),
                options: Some(crate::commands::failure_options(
                    TaskFailureCode::CommandFailed,
                    false,
                )),
            };
            let failure_artifact = InvestigationArtifact {
                id: format!(
                    "artifact_observation_failure_{}_{}",
                    plan.id, request.step_id
                ),
                task_id: task.id.clone(),
                run_id: task.active_run_id.clone(),
                plan_id: Some(plan.id.clone()),
                plan_step_id: Some(request.step_id.clone()),
                phase: TaskPhase::Recovery,
                kind: TaskArtifactKind::Failure,
                status: TaskArtifactStatus::Failed,
                title: "观察窗口发现异常".into(),
                summary: failure_message,
                content: serde_json::to_value(&failure).map_err(|error| error.to_string())?,
                evidence_ids: plan.steps[step_index].evidence_ids.clone(),
                created_at: now.clone(),
                updated_at: now.clone(),
            };
            state
                .database
                .investigations()
                .upsert_artifact(&failure_artifact)
                .map_err(|error| error.to_string())?;
            if super::investigation::can_transition(task.status, TaskStatus::WaitingUser) {
                state
                    .database
                    .investigations()
                    .update_task_progress(&yukinal_database::repositories::TaskProgressUpdate {
                        id: &task.id,
                        status: TaskStatus::WaitingUser,
                        phase: TaskPhase::Recovery,
                        active_run_id: task.active_run_id.as_deref(),
                        last_failure: Some(&failure),
                        updated_at: &now,
                        completed_at: None,
                    })
                    .map_err(|error| error.to_string())?;
            }
            return Ok(json!({
                "recorded": true,
                "observation": "failed",
                "plan": plan,
            }));
        }
        if request.status == "success" {
            let epoch = yukinal_time::now_epoch_seconds();
            let mut observation_sampled = false;
            let mut next_sample_at = None;
            if let Some(window) = plan.observation_window.as_mut() {
                match window.status {
                    ObservationWindowStatus::Pending => {
                        let deadline = epoch.saturating_add(window.duration_seconds);
                        window.status = ObservationWindowStatus::Running;
                        window.sample_count = 1;
                        window.started_at = Some(now.clone());
                        window.deadline_epoch_seconds = Some(deadline);
                        window.deadline_at = Some(yukinal_time::iso8601_utc(deadline));
                        window.last_sample_epoch_seconds = Some(epoch);
                        window.last_sample_at = Some(now.clone());
                        observation_sampled = true;
                        next_sample_at = Some(yukinal_time::iso8601_utc(
                            epoch.saturating_add(window.interval_seconds),
                        ));
                    }
                    ObservationWindowStatus::Running => {
                        let deadline = window.deadline_epoch_seconds.unwrap_or(epoch);
                        if epoch >= deadline {
                            window.status = ObservationWindowStatus::Succeeded;
                            window.sample_count = window.sample_count.saturating_add(1);
                            window.last_sample_epoch_seconds = Some(epoch);
                            window.last_sample_at = Some(now.clone());
                        } else {
                            let last = window.last_sample_epoch_seconds.unwrap_or(0);
                            if epoch.saturating_sub(last) < window.interval_seconds {
                                next_sample_at = Some(yukinal_time::iso8601_utc(
                                    last.saturating_add(window.interval_seconds),
                                ));
                            } else {
                                window.sample_count = window.sample_count.saturating_add(1);
                                window.last_sample_epoch_seconds = Some(epoch);
                                window.last_sample_at = Some(now.clone());
                                observation_sampled = true;
                                next_sample_at = Some(yukinal_time::iso8601_utc(
                                    epoch.saturating_add(window.interval_seconds),
                                ));
                            }
                        }
                    }
                    ObservationWindowStatus::Succeeded => {}
                    ObservationWindowStatus::Failed | ObservationWindowStatus::Cancelled => {
                        return Ok(record_failure(
                            "invalid_input",
                            "observation window is no longer runnable; re-plan before sampling again",
                            false,
                        ));
                    }
                }
                if plan
                    .observation_window
                    .as_ref()
                    .is_some_and(|current| current.status == ObservationWindowStatus::Running)
                    && (observation_sampled || next_sample_at.is_some())
                {
                    plan.updated_at = now.clone();
                    state
                        .database
                        .investigations()
                        .save_plan(&plan)
                        .map_err(|error| error.to_string())?;
                    if let Some(next_sample_at) = next_sample_at {
                        return Ok(json!({
                            "recorded": true,
                            "observation": "running",
                            "sampleAccepted": observation_sampled,
                            "nextSampleAt": next_sample_at,
                            "plan": plan,
                        }));
                    }
                }
            }
        }
    }
    let step = &mut plan.steps[step_index];
    if !matches!(
        step.status,
        PlanStepStatus::Pending | PlanStepStatus::Running
    ) {
        return Ok(record_failure(
            "invalid_input",
            "plan step is no longer active",
            false,
        ));
    }
    step.attempts = step.attempts.saturating_add(1);
    step.started_at.get_or_insert_with(|| now.clone());
    step.ended_at = Some(now.clone());
    let output_summary = request.output_summary.clone();
    match request.status.as_str() {
        "success" => {
            step.status = PlanStepStatus::Succeeded;
            step.last_deviation = None;
            let next_index = step_index + 1;
            if let Some(next) = plan.steps.get_mut(next_index) {
                next.status = PlanStepStatus::Running;
                next.started_at.get_or_insert_with(|| now.clone());
                plan.current_step_id = Some(next.id.clone());
            } else {
                plan.current_step_id = None;
                plan.status = yukinal_database::models::PlanStatus::Completed;
            }
        }
        "failed" | "cancelled" => {
            if !request.retryable || step.attempts >= step.max_attempts {
                step.status = PlanStepStatus::Blocked;
            } else {
                step.status = PlanStepStatus::Pending;
            }
        }
        _ => unreachable!("status was validated above"),
    }
    let blocked = plan.steps[step_index].status == PlanStepStatus::Blocked;
    plan.updated_at = now.clone();
    match state.database.investigations().save_plan(&plan) {
        Ok(()) => {}
        Err(DatabaseError::Validation(message)) => {
            return Ok(record_failure("invalid_input", message, false))
        }
        Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
    }
    if blocked {
        let failure_code = if request.status == "cancelled" {
            TaskFailureCode::Cancelled
        } else {
            TaskFailureCode::CommandFailed
        };
        let failure_message = super::safe_audit_summary(
            output_summary
                .as_deref()
                .unwrap_or("计划步骤未完成，等待用户决定下一步"),
            8_192,
        );
        let mut failure_options = crate::commands::failure_options(failure_code, false);
        if step_kind == PlanStepKind::Action
            && plan.steps[step_index]
                .rollback
                .as_deref()
                .is_some_and(|rollback| !rollback.trim().is_empty())
        {
            failure_options.insert(
                0,
                InvestigationFailureOption {
                    id: "rollback".into(),
                    action: FailureOptionAction::Rollback,
                    title: "先规划回退".into(),
                    description:
                        "保留当前现场，生成一份单独的回退计划；回退动作仍需重新通过风险与用户审批。"
                            .into(),
                    requires_approval: true,
                },
            );
        }
        let failure = yukinal_database::models::InvestigationFailure {
            code: failure_code,
            message: failure_message.clone(),
            retryable: false,
            attempt: plan.steps[step_index].attempts,
            at: now.clone(),
            detail: Some(json!({
                "planId": plan.id,
                "stepId": request.step_id,
                "status": request.status,
                "retryable": request.retryable,
            })),
            options: Some(failure_options),
        };
        let failure_artifact = InvestigationArtifact {
            id: format!("artifact_failure_{}_{}", plan.id, request.step_id),
            task_id: task.id.clone(),
            run_id: task.active_run_id.clone(),
            plan_id: Some(plan.id.clone()),
            plan_step_id: Some(request.step_id.clone()),
            phase: TaskPhase::Recovery,
            kind: TaskArtifactKind::Failure,
            status: TaskArtifactStatus::Failed,
            title: "计划步骤失败".into(),
            summary: failure_message,
            content: serde_json::to_value(&failure).map_err(|error| error.to_string())?,
            evidence_ids: plan.steps[step_index].evidence_ids.clone(),
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        state
            .database
            .investigations()
            .upsert_artifact(&failure_artifact)
            .map_err(|error| error.to_string())?;
        if super::investigation::can_transition(task.status, TaskStatus::Failed) {
            state
                .database
                .investigations()
                .update_task_progress(&yukinal_database::repositories::TaskProgressUpdate {
                    id: &task.id,
                    status: TaskStatus::Failed,
                    phase: TaskPhase::Recovery,
                    active_run_id: task.active_run_id.as_deref(),
                    last_failure: Some(&failure),
                    updated_at: &now,
                    completed_at: None,
                })
                .map_err(|error| error.to_string())?;
        }
    }
    if blocked {
        return Ok(json!({ "recorded": true, "plan": plan }));
    }
    let desired_status = if plan.status == yukinal_database::models::PlanStatus::Completed
        && step_kind == PlanStepKind::Verification
    {
        TaskStatus::Completed
    } else {
        plan.current_step_id
            .as_deref()
            .and_then(|current_step_id| {
                plan.steps
                    .iter()
                    .find(|candidate| candidate.id == current_step_id)
                    .map(|candidate| match candidate.kind {
                        PlanStepKind::Action => TaskStatus::Executing,
                        PlanStepKind::Verification => TaskStatus::Verifying,
                        PlanStepKind::Decision => TaskStatus::WaitingUser,
                        PlanStepKind::Evidence => TaskStatus::Investigating,
                    })
            })
            .unwrap_or(TaskStatus::WaitingUser)
    };
    if task.status != desired_status
        && super::investigation::can_transition(task.status, desired_status)
    {
        let completed_at = (desired_status == TaskStatus::Completed).then_some(now.as_str());
        state
            .database
            .investigations()
            .update_task_status(&task.id, desired_status, &now, completed_at)
            .map_err(|error| error.to_string())?;
    }
    Ok(json!({ "recorded": true, "plan": plan }))
}

fn handle_artifact_record(state: &AppState, params: Value) -> Result<Value, String> {
    let request = match serde_json::from_value::<HostArtifactRecordRequest>(params) {
        Ok(request) => request,
        Err(error) => {
            return Ok(record_failure(
                "invalid_input",
                format!("invalid investigation artifact: {error}"),
                false,
            ))
        }
    };
    let mut artifact = request.artifact;
    // The envelope's binding is copied from the separately validated request. The model is
    // allowed to propose content, but it cannot smuggle a different plan revision into the
    // row that the host persists.
    artifact.plan_id = request.plan_id.clone();
    artifact.plan_step_id = request.plan_step_id.clone();
    let task = match state.database.investigations().get_task(&artifact.task_id) {
        Ok(task) => task,
        Err(DatabaseError::NotFound) => {
            return Ok(record_failure(
                "not_found",
                "investigation task was not found",
                false,
            ))
        }
        Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
    };
    match check_plan_for_tool(
        state,
        &task.id,
        "investigation.artifact",
        &HostToolTarget {
            host: match task.scope.host {
                yukinal_database::models::InvestigationTargetHost::Local => "local".into(),
                yukinal_database::models::InvestigationTargetHost::Remote => "remote".into(),
            },
            server_id: task.scope.server_id.clone(),
            workspace_id: task.scope.workspace_id.clone(),
            environment: task.scope.environment,
        },
        PlanCheckBinding {
            input: &Value::Object(serde_json::Map::new()),
            plan_id: request.plan_id.as_deref(),
            step_id: request.plan_step_id.as_deref(),
            evidence_ids: request.evidence_ids.as_deref(),
        },
    )? {
        Ok(()) => {}
        Err(deviation) => return Ok(record_failure("plan_deviation", deviation.message, false)),
    }
    if artifact.title.trim().is_empty() || artifact.summary.trim().is_empty() {
        return Ok(record_failure(
            "invalid_input",
            "artifact title and summary are required",
            false,
        ));
    }
    if artifact.kind == TaskArtifactKind::Baseline
        && !matches!(
            artifact.status,
            TaskArtifactStatus::Ready | TaskArtifactStatus::Succeeded
        )
    {
        return Ok(record_failure(
            "invalid_input",
            "baseline artifacts must be ready or succeeded before an action can consume them",
            false,
        ));
    }
    if artifact.kind == TaskArtifactKind::Baseline {
        let Some(plan_id) = artifact.plan_id.as_deref() else {
            return Ok(record_failure(
                "invalid_input",
                "baseline artifacts must be bound to the current plan",
                false,
            ));
        };
        let Some(plan_step_id) = artifact.plan_step_id.as_deref() else {
            return Ok(record_failure(
                "invalid_input",
                "baseline artifacts must identify their evidence step",
                false,
            ));
        };
        if artifact.evidence_ids.is_empty() {
            return Ok(record_failure(
                "evidence_missing",
                "baseline artifacts must cite at least one persisted evidence envelope",
                true,
            ));
        }
        let plan = match state.database.investigations().get_plan(plan_id) {
            Ok(plan) if plan.task_id == task.id => plan,
            Ok(_) => {
                return Ok(record_failure(
                    "denied_by_policy",
                    "baseline plan does not belong to the current investigation task",
                    false,
                ))
            }
            Err(DatabaseError::NotFound) => {
                return Ok(record_failure(
                    "not_found",
                    "baseline plan was not found",
                    false,
                ))
            }
            Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
        };
        if !plan
            .steps
            .iter()
            .any(|step| step.id == plan_step_id && step.kind == PlanStepKind::Evidence)
        {
            return Ok(record_failure(
                "invalid_input",
                "baseline artifacts must be produced by an evidence step",
                false,
            ));
        }
    }
    if artifact.title.chars().count() > 512 || artifact.summary.chars().count() > 8_192 {
        return Ok(record_failure(
            "invalid_input",
            "artifact title or summary exceeds the configured limit",
            false,
        ));
    }
    let content_bytes = serde_json::to_vec(&artifact.content).map_err(|error| error.to_string())?;
    if content_bytes.len() > yukinal_database::models::MAX_ARTIFACT_SERIALIZED_BYTES {
        return Ok(record_failure(
            "invalid_input",
            "artifact content exceeds the 1 MiB limit",
            false,
        ));
    }
    if let Some(run_id) = artifact.run_id.as_deref() {
        match state.database.investigations().get_run(run_id) {
            Ok(run) if run.task_id == task.id => {}
            Ok(_) => {
                return Ok(record_failure(
                    "denied_by_policy",
                    "artifact run does not belong to the current investigation task",
                    false,
                ))
            }
            Err(DatabaseError::NotFound) => {
                return Ok(record_failure(
                    "not_found",
                    "artifact run was not found",
                    false,
                ))
            }
            Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
        }
    }
    for evidence_id in &artifact.evidence_ids {
        match state.database.investigations().get_evidence(evidence_id) {
            Ok(evidence) if evidence.task_id == task.id => {}
            Ok(_) => {
                return Ok(record_failure(
                    "denied_by_policy",
                    "artifact references evidence from another task",
                    false,
                ))
            }
            Err(DatabaseError::NotFound) => {
                return Ok(record_failure(
                    "evidence_missing",
                    format!("evidence `{evidence_id}` was not found"),
                    true,
                ))
            }
            Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
        }
    }
    match state.database.investigations().upsert_artifact(&artifact) {
        Ok(()) => Ok(json!({ "recorded": true, "artifact": artifact })),
        Err(DatabaseError::Validation(message)) => {
            Ok(record_failure("invalid_input", message, false))
        }
        Err(error) => Ok(record_failure("internal", error.to_string(), false)),
    }
}

fn validate_playbook_step(
    step: &yukinal_database::models::InvestigationPlanStep,
) -> Option<String> {
    if step.requires_baseline == Some(true) && step.kind != PlanStepKind::Action {
        return Some(format!(
            "only action step `{}` may require a baseline",
            step.id
        ));
    }
    if step.kind != PlanStepKind::Action {
        return None;
    }
    if step
        .preview
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
    {
        return Some(format!(
            "action step `{}` must include a bounded preview",
            step.id
        ));
    }
    let has_verification = step
        .verification_criteria
        .as_ref()
        .is_some_and(|criteria| !criteria.is_empty())
        || !step.success_criteria.is_empty();
    if !has_verification {
        return Some(format!(
            "action step `{}` must declare verification criteria",
            step.id
        ));
    }
    if matches!(step.risk_level, Some(RiskLevel::High | RiskLevel::Critical))
        && !step.requires_approval
    {
        return Some(format!(
            "high-risk action step `{}` must require approval",
            step.id
        ));
    }
    if step.idempotency == Some(PlanIdempotency::Unsafe)
        && step
            .rollback
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
    {
        return Some(format!(
            "unsafe action step `{}` must declare rollback details",
            step.id
        ));
    }
    None
}

fn task_allows_auto_medium_action(
    task: &InvestigationTask,
    step: &yukinal_database::models::InvestigationPlanStep,
) -> bool {
    let target = step.target.as_ref().unwrap_or(&task.scope);
    let protected_config_action = step.allowed_tools.len() == 1
        && matches!(
            step.allowed_tools[0].as_str(),
            FILESYSTEM_BACKUP | FILESYSTEM_EDIT
        );
    task.permission_mode == InvestigationPermissionMode::Auto
        && task.mode == InvestigationRunMode::Goal
        && task.automation_level == TaskAutomationLevel::Execute
        && target.host == InvestigationTargetHost::Remote
        && matches!(
            target.environment,
            Environment::Development | Environment::Staging
        )
        && step.risk_level == Some(RiskLevel::Medium)
        && protected_config_action
}

fn validate_input_bindings(
    step: &yukinal_database::models::InvestigationPlanStep,
) -> Option<String> {
    let bindings = step.input_bindings.as_ref()?;
    if bindings.len() > 8 {
        return Some(format!(
            "plan step `{}` has too many input bindings",
            step.id
        ));
    }
    if bindings.iter().any(|(key, value)| {
        key.trim().is_empty()
            || key.chars().count() > 64
            || value.is_empty()
            || value.chars().count() > 4_096
    }) {
        return Some(format!(
            "plan step `{}` has an invalid input binding",
            step.id
        ));
    }
    None
}

fn validate_observation_window(
    window: &InvestigationObservationWindow,
    steps: &[yukinal_database::models::InvestigationPlanStep],
) -> Option<String> {
    if window.duration_seconds == 0 || window.duration_seconds > 86_400 {
        return Some("observation duration must be between 1 and 86400 seconds".into());
    }
    if window.interval_seconds == 0 || window.interval_seconds > window.duration_seconds {
        return Some("observation interval must be between 1 and the duration".into());
    }
    if window.allowed_tools.is_empty() || window.allowed_tools.len() > 32 {
        return Some("observation window needs 1 to 32 allowed read tools".into());
    }
    if window.success_criteria.is_empty() || window.success_criteria.len() > 16 {
        return Some("observation window needs 1 to 16 success criteria".into());
    }
    if window
        .allowed_tools
        .iter()
        .any(|tool| tool.trim().is_empty() || tool.len() > 256)
    {
        return Some("observation tools must be bounded names".into());
    }
    if window
        .success_criteria
        .iter()
        .any(|criterion| criterion.trim().is_empty() || criterion.chars().count() > 1_024)
    {
        return Some("observation success criteria must be bounded text".into());
    }
    let Some(verification) = steps
        .last()
        .filter(|step| step.kind == PlanStepKind::Verification)
    else {
        return Some("observation window requires the final plan step to be verification".into());
    };
    if window.allowed_tools.iter().any(|tool| {
        !verification
            .allowed_tools
            .iter()
            .any(|allowed| allowed == tool)
    }) {
        return Some("observation tools must be allowed by the final verification step".into());
    }
    None
}

fn observation_configuration_matches(
    left: &InvestigationObservationWindow,
    right: &InvestigationObservationWindow,
) -> bool {
    left.duration_seconds == right.duration_seconds
        && left.interval_seconds == right.interval_seconds
        && left.allowed_tools == right.allowed_tools
        && left.success_criteria == right.success_criteria
}

fn reset_observation_window(window: &mut InvestigationObservationWindow) {
    window.status = ObservationWindowStatus::Pending;
    window.sample_count = 0;
    window.started_at = None;
    window.deadline_at = None;
    window.deadline_epoch_seconds = None;
    window.last_sample_at = None;
    window.last_sample_epoch_seconds = None;
    window.last_failure = None;
}

struct PlanCheckBinding<'a> {
    input: &'a Value,
    plan_id: Option<&'a str>,
    step_id: Option<&'a str>,
    evidence_ids: Option<&'a [String]>,
}

struct TaskGuardrailViolation {
    code: PlanDeviationCode,
    message: String,
}

fn task_guardrail_violation(
    task: &InvestigationTask,
    tool_name: &str,
    input: &Value,
) -> Option<TaskGuardrailViolation> {
    let now = yukinal_time::now_epoch_seconds();
    if let Some(not_before) = task.guardrails.not_before_at.as_deref() {
        let Some(not_before) = yukinal_time::parse_iso8601_utc(not_before) else {
            return Some(TaskGuardrailViolation {
                code: PlanDeviationCode::OutsideTimeWindow,
                message: "task guardrail notBeforeAt is invalid; execution is blocked".into(),
            });
        };
        if now < not_before {
            return Some(TaskGuardrailViolation {
                code: PlanDeviationCode::OutsideTimeWindow,
                message: format!(
                    "task execution is not allowed before {}",
                    task.guardrails.not_before_at.as_deref().unwrap_or_default()
                ),
            });
        }
    }
    if let Some(expires) = task.guardrails.expires_at.as_deref() {
        let Some(expires) = yukinal_time::parse_iso8601_utc(expires) else {
            return Some(TaskGuardrailViolation {
                code: PlanDeviationCode::OutsideTimeWindow,
                message: "task guardrail expiresAt is invalid; execution is blocked".into(),
            });
        };
        if now >= expires {
            return Some(TaskGuardrailViolation {
                code: PlanDeviationCode::OutsideTimeWindow,
                message: format!(
                    "task execution time window expired at {}",
                    task.guardrails.expires_at.as_deref().unwrap_or_default()
                ),
            });
        }
    }
    if task
        .guardrails
        .forbidden_tools
        .iter()
        .any(|forbidden| forbidden == tool_name)
    {
        return Some(TaskGuardrailViolation {
            code: PlanDeviationCode::ScopeForbidden,
            message: format!("tool `{tool_name}` is forbidden by the task guardrails"),
        });
    }
    for path in input_path_values(input) {
        if let Some(prefix) = task
            .guardrails
            .forbidden_path_prefixes
            .iter()
            .find(|prefix| path_is_under_prefix(path, prefix))
        {
            return Some(TaskGuardrailViolation {
                code: PlanDeviationCode::ScopeForbidden,
                message: format!(
                    "path `{path}` is forbidden by the task guardrail prefix `{prefix}`"
                ),
            });
        }
    }
    None
}

fn validate_task_guardrails_for_plan_step(
    task: &InvestigationTask,
    step: &yukinal_database::models::InvestigationPlanStep,
) -> Option<String> {
    if !task.guardrails.forbidden_tools.is_empty()
        && step.allowed_tools.iter().any(|tool| tool == "*")
    {
        return Some(format!(
            "plan step `{}` uses wildcard tools while task guardrails forbid specific tools; enumerate only allowed tools",
            step.id
        ));
    }
    if let Some(tool) = step.allowed_tools.iter().find(|tool| {
        task.guardrails
            .forbidden_tools
            .iter()
            .any(|forbidden| forbidden == *tool)
    }) {
        return Some(format!(
            "plan step `{}` allows tool `{tool}`, which is forbidden by the task guardrails",
            step.id
        ));
    }
    if let Some(bindings) = step.input_bindings.as_ref() {
        let input = Value::Object(
            bindings
                .iter()
                .map(|(key, value)| (key.clone(), Value::String(value.clone())))
                .collect(),
        );
        if let Some(violation) = task_guardrail_violation(task, "plan.binding", &input) {
            if violation.code == PlanDeviationCode::ScopeForbidden {
                return Some(format!(
                    "plan step `{}` violates task guardrails: {}",
                    step.id, violation.message
                ));
            }
        }
    }
    None
}

fn input_path_values(input: &Value) -> Vec<&str> {
    let Some(object) = input.as_object() else {
        return Vec::new();
    };
    ["path", "backupPath"]
        .into_iter()
        .filter_map(|key| object.get(key).and_then(Value::as_str))
        .collect()
}

fn path_is_under_prefix(path: &str, prefix: &str) -> bool {
    let path = normalize_guardrail_path(path);
    let prefix = normalize_guardrail_path(prefix);
    prefix == "/" || path == prefix || path.starts_with(&format!("{prefix}/"))
}

fn normalize_guardrail_path(value: &str) -> String {
    let mut parts = Vec::new();
    for part in value.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            value => parts.push(value),
        }
    }
    if parts.is_empty() {
        "/".into()
    } else {
        format!("/{}", parts.join("/"))
    }
}

fn check_plan_for_tool(
    state: &AppState,
    task_id: &str,
    tool_name: &str,
    target: &HostToolTarget,
    binding: PlanCheckBinding<'_>,
) -> Result<std::result::Result<(), InvestigationPlanDeviation>, String> {
    let task = state
        .database
        .investigations()
        .get_task(task_id)
        .map_err(|error| error.to_string())?;
    if let Some(violation) = task_guardrail_violation(&task, tool_name, binding.input) {
        return Ok(Err(plan_deviation(
            violation.code,
            PlanDeviationAction::WaitUser,
            violation.message,
            tool_name,
            yukinal_core::sidecar::iso8601_now(),
            None,
            None,
        )));
    }
    let at = yukinal_core::sidecar::iso8601_now();
    let plan = match state
        .database
        .investigations()
        .latest_plan(task_id)
        .map_err(|error| error.to_string())?
    {
        Some(plan) => plan,
        None => return Ok(Err(plan_deviation(
            PlanDeviationCode::MissingPlan,
            PlanDeviationAction::Replan,
            "durable task has no active plan; create investigation.plan before using a task tool",
            tool_name,
            at,
            None,
            None,
        ))),
    };
    let Some(current_step_id) = plan.current_step_id.as_deref() else {
        return Ok(Err(plan_deviation(
            PlanDeviationCode::NoActiveStep,
            PlanDeviationAction::Replan,
            "active plan has no current step",
            tool_name,
            at,
            Some(plan.id),
            None,
        )));
    };
    let Some(step) = plan.steps.iter().find(|step| step.id == current_step_id) else {
        return Ok(Err(plan_deviation(
            PlanDeviationCode::NoActiveStep,
            PlanDeviationAction::Replan,
            "active plan points to a missing current step",
            tool_name,
            at,
            Some(plan.id),
            Some(current_step_id.to_string()),
        )));
    };
    if !plan_binding_matches(
        &plan,
        step,
        binding.plan_id,
        binding.step_id,
        binding.evidence_ids,
    ) {
        return Ok(Err(plan_deviation(
            PlanDeviationCode::BindingMismatch,
            PlanDeviationAction::Deny,
            "tool request is not bound to the plan revision, current step and evidence set returned by the host",
            tool_name,
            at,
            Some(plan.id.clone()),
            Some(step.id.clone()),
        )));
    }
    if !matches!(
        step.status,
        PlanStepStatus::Pending | PlanStepStatus::Running
    ) {
        return Ok(Err(plan_deviation(
            PlanDeviationCode::NoActiveStep,
            PlanDeviationAction::Replan,
            "current plan step is no longer runnable",
            tool_name,
            at,
            Some(plan.id),
            Some(step.id.clone()),
        )));
    }
    if step.attempts >= step.max_attempts {
        return Ok(Err(plan_deviation(
            PlanDeviationCode::StepBudgetExhausted,
            PlanDeviationAction::WaitUser,
            "current plan step has exhausted its retry budget",
            tool_name,
            at,
            Some(plan.id),
            Some(step.id.clone()),
        )));
    }
    if let Some(window) = plan
        .observation_window
        .as_ref()
        .filter(|window| window.status == ObservationWindowStatus::Running)
    {
        if step.kind != PlanStepKind::Verification
            || !window
                .allowed_tools
                .iter()
                .any(|allowed| allowed == tool_name)
        {
            return Ok(Err(plan_deviation(
                PlanDeviationCode::ToolNotAllowed,
                PlanDeviationAction::WaitUser,
                "observation window only permits its declared verification read tools",
                tool_name,
                at,
                Some(plan.id.clone()),
                Some(step.id.clone()),
            )));
        }
    }
    if step.kind == PlanStepKind::Action {
        match task.automation_level {
            TaskAutomationLevel::Readonly => {
                return Ok(Err(plan_deviation(
                    PlanDeviationCode::ToolNotAllowed,
                    PlanDeviationAction::Deny,
                    "this task is read-only; action steps are available for planning but cannot execute",
                    tool_name,
                    at,
                    Some(plan.id),
                    Some(step.id.clone()),
                )))
            }
            TaskAutomationLevel::Propose
                if plan.approval.as_ref().map(|approval| approval.status)
                    != Some(yukinal_database::models::PlanApprovalStatus::Approved) =>
            {
                return Ok(Err(plan_deviation(
                    PlanDeviationCode::NoActiveStep,
                    PlanDeviationAction::WaitUser,
                    "proposal-mode action is waiting for the user to select and approve a decision option",
                    tool_name,
                    at,
                    Some(plan.id),
                    Some(step.id.clone()),
                )))
            }
            TaskAutomationLevel::Propose | TaskAutomationLevel::Execute => {}
        }
    }
    if step.kind == PlanStepKind::Action && step.requires_baseline == Some(true) {
        let artifacts = state
            .database
            .investigations()
            .list_artifacts(&task.id, 128)
            .map_err(|error| error.to_string())?;
        let has_baseline = baseline_artifact_matches_plan(&artifacts, &plan);
        if !has_baseline {
            return Ok(Err(plan_deviation(
                PlanDeviationCode::EvidenceMissing,
                PlanDeviationAction::Replan,
                "current action step requires a ready baseline artifact bound to this plan revision",
                tool_name,
                at,
                Some(plan.id),
                Some(step.id.clone()),
            )));
        }
    }
    if tool_name != "investigation.artifact"
        && !step
            .allowed_tools
            .iter()
            .any(|allowed| allowed == "*" || allowed == tool_name)
    {
        return Ok(Err(plan_deviation(
            PlanDeviationCode::ToolNotAllowed,
            PlanDeviationAction::Replan,
            format!("tool `{tool_name}` is not allowed by the current plan step"),
            tool_name,
            at,
            Some(plan.id),
            Some(step.id.clone()),
        )));
    }
    if !input_bindings_match(step, binding.input) {
        return Ok(Err(plan_deviation(
            PlanDeviationCode::BindingMismatch,
            PlanDeviationAction::Deny,
            "tool input does not match the exact arguments bound by the active plan step",
            tool_name,
            at,
            Some(plan.id),
            Some(step.id.clone()),
        )));
    }
    let target = host_target_to_investigation(target)?;
    let expected = step.target.as_ref().unwrap_or(&task.scope);
    if !same_investigation_target(expected, &target) {
        return Ok(Err(plan_deviation(
            PlanDeviationCode::TargetMismatch,
            PlanDeviationAction::WaitUser,
            "tool target does not match the active plan step scope",
            tool_name,
            at,
            Some(plan.id),
            Some(step.id.clone()),
        )));
    }
    for evidence_id in &step.evidence_ids {
        match state.database.investigations().get_evidence(evidence_id) {
            Ok(evidence) if evidence.task_id == task.id => {}
            Ok(_) | Err(DatabaseError::NotFound) => {
                return Ok(Err(plan_deviation(
                    PlanDeviationCode::EvidenceMissing,
                    PlanDeviationAction::Replan,
                    format!("plan step requires missing evidence `{evidence_id}`"),
                    tool_name,
                    at,
                    Some(plan.id),
                    Some(step.id.clone()),
                )))
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(Ok(()))
}

fn baseline_artifact_matches_plan(
    artifacts: &[InvestigationArtifact],
    plan: &InvestigationPlan,
) -> bool {
    artifacts.iter().any(|artifact| {
        artifact.kind == TaskArtifactKind::Baseline
            && matches!(
                artifact.status,
                TaskArtifactStatus::Ready | TaskArtifactStatus::Succeeded
            )
            && artifact.plan_id.as_deref() == Some(plan.id.as_str())
            && !artifact.evidence_ids.is_empty()
            && artifact.plan_step_id.as_deref().is_some_and(|step_id| {
                plan.steps.iter().any(|step| {
                    step.id == step_id
                        && step.kind == PlanStepKind::Evidence
                        && step.status == PlanStepStatus::Succeeded
                })
            })
    })
}

fn plan_binding_matches(
    plan: &InvestigationPlan,
    step: &yukinal_database::models::InvestigationPlanStep,
    expected_plan_id: Option<&str>,
    expected_step_id: Option<&str>,
    expected_evidence_ids: Option<&[String]>,
) -> bool {
    // The first `host.investigation.plan.check` call has no binding yet: it asks
    // the host to issue the current binding.  The actual `host.tool.execute`
    // request must carry all three values and is checked fail-closed.  Treating
    // the unbound probe as a mismatch would make every durable task unusable.
    if expected_plan_id.is_none() && expected_step_id.is_none() && expected_evidence_ids.is_none() {
        return true;
    }
    expected_plan_id == Some(plan.id.as_str())
        && expected_step_id == Some(step.id.as_str())
        && expected_evidence_ids == Some(step.evidence_ids.as_slice())
}

fn input_bindings_match(
    step: &yukinal_database::models::InvestigationPlanStep,
    input: &Value,
) -> bool {
    let Some(bindings) = step.input_bindings.as_ref() else {
        return true;
    };
    let Some(object) = input.as_object() else {
        return false;
    };
    bindings.iter().all(|(key, expected)| {
        object
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|actual| actual == expected)
    })
}

fn plan_deviation(
    code: PlanDeviationCode,
    action: PlanDeviationAction,
    message: impl Into<String>,
    tool_name: &str,
    at: String,
    plan_id: Option<String>,
    step_id: Option<String>,
) -> InvestigationPlanDeviation {
    InvestigationPlanDeviation {
        code,
        action,
        message: message.into(),
        tool_name: tool_name.to_string(),
        at,
        plan_id,
        step_id,
    }
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

fn handle_context_request(state: &AppState, params: Value) -> Result<Value, String> {
    let request = serde_json::from_value::<HostContextRequest>(params)
        .map_err(|error| format!("invalid host context request: {error}"))?;
    if request.id.trim().is_empty() || request.id.len() > 160 {
        return Ok(failed(
            "invalid_input",
            "context id must be between 1 and 160 characters",
            true,
            None,
        ));
    }
    if matches!(
        request.kind,
        HostContextKind::Server | HostContextKind::Snapshot
    ) && !request.id.starts_with("srv_")
    {
        return Ok(failed(
            "invalid_input",
            "server context requires an opaque srv_ id",
            false,
            None,
        ));
    }

    match request.kind {
        HostContextKind::Server => context_row(state.database.servers().get(&request.id)),
        HostContextKind::Snapshot => match state.database.snapshots().latest(&request.id) {
            Ok(Some(snapshot)) => context_success(snapshot),
            Ok(None) => Ok(json!({ "status": "not_found" })),
            Err(error) => context_error(error),
        },
        HostContextKind::Workspace => context_row(state.database.workspaces().get(&request.id)),
        HostContextKind::Investigation => investigation_context(state, &request.id),
    }
}

fn investigation_context(state: &AppState, task_id: &str) -> Result<Value, String> {
    let task = match state.database.investigations().get_task(task_id) {
        Ok(task) => task,
        Err(error) => return context_error(error),
    };
    let evidence = state
        .database
        .investigations()
        .list_evidence(task_id, MAX_EVIDENCE_SEARCH_LIMIT)
        .map_err(|error| error.to_string())?;
    let findings = state
        .database
        .investigations()
        .list_findings(task_id, 100)
        .map_err(|error| error.to_string())?;
    let decision_brief = state
        .database
        .investigations()
        .latest_decision_brief(task_id)
        .map_err(|error| error.to_string())?;
    let runs = state
        .database
        .investigations()
        .list_runs(task_id, 64)
        .map_err(|error| error.to_string())?;
    let steps = state
        .database
        .investigations()
        .list_steps(task_id, 512)
        .map_err(|error| error.to_string())?;
    let artifacts = state
        .database
        .investigations()
        .list_artifacts(task_id, 128)
        .map_err(|error| error.to_string())?;
    let plan = state
        .database
        .investigations()
        .latest_plan(task_id)
        .map_err(|error| error.to_string())?;
    let evaluated_at_epoch = yukinal_time::now_epoch_seconds();
    context_success(InvestigationContextResponse {
        task,
        evidence: evidence
            .into_iter()
            .map(|item| InvestigationEvidenceSummary::from_at(item, evaluated_at_epoch))
            .collect(),
        findings,
        runs,
        steps,
        artifacts: artifacts
            .into_iter()
            .map(InvestigationArtifactSummary::from)
            .collect(),
        plan,
        decision_brief,
    })
}

fn context_row<T: Serialize>(result: yukinal_database::Result<T>) -> Result<Value, String> {
    match result {
        Ok(value) => context_success(value),
        Err(error) => context_error(error),
    }
}

fn context_success<T: Serialize>(value: T) -> Result<Value, String> {
    Ok(json!({
        "status": "success",
        "data": serde_json::to_value(value).map_err(|error| error.to_string())?,
    }))
}

fn context_error(error: DatabaseError) -> Result<Value, String> {
    if matches!(error, DatabaseError::NotFound) {
        return Ok(json!({ "status": "not_found" }));
    }
    Ok(failed("internal", error.to_string(), false, None))
}

async fn ensure_session_with_cancel(
    state: &AppState,
    server_id: &str,
    cancel: &CancellationToken,
) -> Result<(), String> {
    if cancel.is_cancelled() {
        return Err("host operation cancelled".to_string());
    }
    tokio::select! {
        result = ensure_session(state, server_id) => result,
        _ = cancel.cancelled() => Err("host operation cancelled".to_string()),
    }
}

fn transport_or_cancel(error: impl std::fmt::Display, cancel: &CancellationToken) -> Value {
    if cancel.is_cancelled() {
        cancelled_failure()
    } else {
        failed("transport", error.to_string(), true, None)
    }
}

/// File-capability failure → the host tool result's failure code.
///
/// The codes themselves (`invalid_input` / `denied_by_policy`) belong to the sidecar's host
/// protocol, not to the file capability, so the mapping stays here while the rules, the messages
/// and the limits live in `yukinal-filesystem`. `retryable` follows the existing table: a bad
/// argument can be fixed by the Agent, a policy denial cannot.
///
/// The two edit refusals ride on the existing `invalid_input` code — the vocabulary has no
/// edit-specific code, and both messages say what to do next:
/// - a **revision mismatch** is exactly "your input is stale, re-read and retry", so it is
///   retryable, and `detail` carries both revision strings so the Agent can see the drift;
/// - a file **over the edit cap** cannot be fixed by retrying anything (the file has to shrink),
///   so it is `retryable: false`: the fix is a different tool, not another attempt.
fn filesystem_failure(error: FilesystemError, cancel: &CancellationToken) -> Value {
    // The wording belongs to the capability, so it is taken from the typed error itself instead of
    // being re-written here, where it could drift away from the rule it explains.
    let message = error.to_string();
    match error {
        FilesystemError::InvalidInput(_) => failed("invalid_input", message, true, None),
        FilesystemError::DeniedByPolicy(_) => failed("denied_by_policy", message, false, None),
        FilesystemError::RevisionMismatch { expected, actual } => failed(
            "invalid_input",
            message,
            true,
            Some(json!({ "expectedRevision": expected, "actualRevision": actual })),
        ),
        FilesystemError::FileTooLargeToEdit { limit } => failed(
            "invalid_input",
            message,
            false,
            Some(json!({ "maxEditableBytes": limit })),
        ),
        FilesystemError::FileTooLargeToBackup { limit } => failed(
            "invalid_input",
            message,
            false,
            Some(json!({ "maxBackupBytes": limit })),
        ),
        // 「远端做不到安全替换」：重试多少次都一样（要变的是服务器或这个文件），所以它有一个
        // 自己的码 —— 把它报成 invalid_input 会让模型把「换条路重试」当成正确反应。
        FilesystemError::UnsafeRemoteWrite(_) => failed("unsupported", message, false, None),
        // 并发修改：与过期的 revision 同一类，下一步都是「重新读，再带着新 revision 重试」。
        FilesystemError::ConcurrentChange(_) => failed("invalid_input", message, true, None),
        FilesystemError::MetadataNotPreserved { missing, .. } => failed(
            "unsupported",
            message,
            false,
            Some(json!({ "missingMetadata": missing })),
        ),
        FilesystemError::Transport(error) => transport_or_cancel(error, cancel),
    }
}

pub(crate) fn cancelled_failure() -> Value {
    failed("cancelled", "Host operation cancelled", false, None)
}

async fn server_info(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    if !is_empty_object(input) {
        return Ok(failed(
            "invalid_input",
            "server.info accepts an empty object",
            true,
            None,
        ));
    }

    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let session = match state.terminals.cached_session(server_id) {
        Ok(session) => session,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    let collected_at = yukinal_core::sidecar::iso8601_now();
    let (snapshot, _) = tokio::select! {
        result = yukinal_core::collector::collect_snapshot(
            &state.ssh,
            &session,
            server_id,
            &collected_at,
        ) => match result {
            Ok(result) => result,
            Err(error) => return Ok(transport_or_cancel(error, cancel)),
        },
        _ = cancel.cancelled() => return Ok(cancelled_failure()),
    };

    if let Err(error) = state.database.snapshots().insert(&snapshot) {
        return Ok(failed("internal", error.to_string(), false, None));
    }
    let output = serde_json::to_value(snapshot).map_err(|error| error.to_string())?;
    Ok(success(output))
}

async fn server_logs(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<ServerLogsInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("server.logs input is invalid: {error}"),
                true,
                None,
            ))
        }
    };
    let command = match log_discovery_command_for(&input) {
        Ok(command) => command,
        Err(error) => return Ok(failed("invalid_input", error, true, None)),
    };
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let session = match state.terminals.cached_session(server_id) {
        Ok(session) => session,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    let result = match state
        .ssh
        .execute(
            &session,
            &command,
            Some(std::time::Duration::from_secs(10)),
            cancel,
        )
        .await
    {
        Ok(result) => result,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    let parsed = match parse_logs_output(&result.stdout_lossy()) {
        Ok(parsed) => parsed,
        Err(error) => {
            return Ok(failed(
                "execution_failed",
                format!("server.logs returned an invalid response: {error}"),
                false,
                Some(json!({ "exitCode": result.exit_code })),
            ))
        }
    };
    if (input.since_seconds.is_some() || input.unit.is_some())
        && parsed.source != crate::commands::logs::LogSource::Journalctl
    {
        return Ok(failed(
            "unsupported",
            "server.logs filters require journalctl; the fallback log source was not returned unfiltered",
            false,
            Some(json!({ "source": parsed.source })),
        ));
    }
    Ok(success(
        serde_json::to_value(parsed).map_err(|error| error.to_string())?,
    ))
}

async fn server_services(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<ServerServicesInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("server.services input is invalid: {error}"),
                true,
                None,
            ))
        }
    };
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let session = match state.terminals.cached_session(server_id) {
        Ok(session) => session,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    let result = match state
        .ssh
        .execute(
            &session,
            service_discovery_command(),
            Some(std::time::Duration::from_secs(10)),
            cancel,
        )
        .await
    {
        Ok(result) => result,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    let mut parsed = match parse_services_output(&result.stdout_lossy()) {
        Ok(parsed) => parsed,
        Err(error) => {
            return Ok(failed(
                "execution_failed",
                format!("server.services returned an invalid response: {error}"),
                false,
                Some(json!({ "exitCode": result.exit_code })),
            ))
        }
    };
    if let Err(error) = filter_services(&mut parsed, &input) {
        return Ok(failed("invalid_input", error, true, None));
    }
    Ok(success(
        serde_json::to_value(parsed).map_err(|error| error.to_string())?,
    ))
}

async fn filesystem_read(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<FilesystemReadInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("filesystem.read input is invalid: {error}"),
                true,
                None,
            ))
        }
    };
    // Path policy and the byte cap are checked before anything else, cancellation included:
    // an invalid argument reports `invalid_input` even after the user pressed Stop, and a
    // blocked path never opens a session. The crate's request type carries that guarantee, so
    // the rules are not repeated here.
    let request = match AgentReadRequest::check(&input.path, input.max_bytes) {
        Ok(request) => request,
        Err(error) => return Ok(filesystem_failure(error, cancel)),
    };
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let service = remote_file_service(state);
    let read = tokio::select! {
        result = service.agent_read(server_id, &request) => match result {
            Ok(read) => read,
            Err(error) => return Ok(filesystem_failure(error, cancel)),
        },
        _ = cancel.cancelled() => return Ok(cancelled_failure()),
    };
    Ok(success(
        serde_json::to_value(FilesystemReadResult {
            path: read.path,
            content: read.content,
            truncated: read.truncated,
            revision: read.revision,
        })
        .map_err(|error| error.to_string())?,
    ))
}

async fn filesystem_write(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<FilesystemWriteInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("filesystem.write input is invalid: {error}"),
                true,
                None,
            ))
        }
    };
    let request = match AgentWriteRequest::check(&input.path, input.content) {
        Ok(request) => request,
        Err(error) => return Ok(filesystem_failure(error, cancel)),
    };
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let service = remote_file_service(state);
    let write = tokio::select! {
        result = service.agent_write(server_id, &request) => match result {
            Ok(write) => write,
            Err(error) => return Ok(filesystem_failure(error, cancel)),
        },
        _ = cancel.cancelled() => return Ok(cancelled_failure()),
    };
    Ok(success(
        serde_json::to_value(FilesystemWriteResult {
            path: write.path,
            bytes_written: write.bytes_written,
        })
        .map_err(|error| error.to_string())?,
    ))
}

/// `filesystem.edit`: read the file, verify its revision, replace one exact match, write it back.
///
/// All of that happens inside one capability call (`RemoteFileService::agent_edit`) and is
/// cancellable as a whole, because a half-applied edit is exactly what the guard exists to
/// prevent. The checks themselves (cap, revision, single match) live in `yukinal-filesystem`.
async fn filesystem_edit(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<FilesystemEditInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("filesystem.edit input is invalid: {error}"),
                true,
                None,
            ))
        }
    };
    let request = match AgentEditRequest::check(
        &input.path,
        &input.expected_revision,
        input.old_string,
        input.new_string,
    ) {
        Ok(request) => request,
        Err(error) => return Ok(filesystem_failure(error, cancel)),
    };
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let service = remote_file_service(state);
    let edit = tokio::select! {
        result = service.agent_edit(server_id, &request) => match result {
            Ok(edit) => edit,
            Err(error) => return Ok(filesystem_failure(error, cancel)),
        },
        _ = cancel.cancelled() => return Ok(cancelled_failure()),
    };
    Ok(success(
        serde_json::to_value(FilesystemEditResult {
            path: edit.path,
            revision: edit.revision,
            bytes_before: edit.bytes_before,
            bytes_after: edit.bytes_after,
            line_delta: edit.line_delta,
        })
        .map_err(|error| error.to_string())?,
    ))
}

/// Generate a single-use token for a host-owned sibling backup path. The token never comes from the
/// model, and `AgentBackupRequest::check` still validates its exact shape before SFTP is touched.
fn backup_token() -> String {
    let mut bytes = [0_u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn backup_record_id(server_id: &str, backup_path: &str) -> String {
    format!(
        "backup_{:x}",
        Sha256::digest(format!("{server_id}\0{backup_path}").as_bytes())
    )
}

fn backup_owner_matches_request(
    record: &FilesystemBackupRecord,
    request: &HostToolExecuteRequest,
) -> bool {
    match (record.task_id.as_deref(), request.task_id.as_deref()) {
        (Some(owner), Some(request_task)) => owner == request_task,
        (None, None) => true,
        _ => false,
    }
}

/// Read the host-owned backup ledger for the current investigation task.
///
/// This never opens an SSH session and never proves that the remote sibling still exists.  The
/// ledger is intentionally presented as metadata so an Agent can explain what may be cleaned or
/// restored before a separately approved action performs a remote check.
async fn filesystem_backup_list(
    state: &AppState,
    server_id: &str,
    input: &Value,
    request_context: &HostToolExecuteRequest,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<FilesystemBackupListInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("filesystem.backup.list input is invalid: {error}"),
                false,
                None,
            ))
        }
    };
    let Some(task_id) = request_context.task_id.as_deref() else {
        return Ok(failed(
            "invalid_input",
            "filesystem.backup.list requires a durable investigation task",
            false,
            None,
        ));
    };
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
    if task.server_id.as_deref() != Some(server_id) {
        return Ok(failed(
            "denied_by_policy",
            "filesystem.backup.list target does not match the task's registered server",
            false,
            None,
        ));
    }
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
                ))
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
    let (backups, truncated) = match state.database.filesystem_backups().list_for_task(
        server_id,
        task_id,
        status,
        input.path.as_deref(),
        limit,
    ) {
        Ok(result) => result,
        Err(DatabaseError::Validation(error)) => {
            return Ok(failed("invalid_input", error, false, None))
        }
        Err(error) => return Ok(failed("internal", error.to_string(), false, None)),
    };
    let backups = backups
        .into_iter()
        .map(|backup| FilesystemBackupLedgerItemResult {
            id: backup.id,
            server_id: backup.server_id,
            task_id: backup.task_id.unwrap_or_else(|| task_id.to_string()),
            path: backup.path,
            backup_path: backup.backup_path,
            revision: backup.revision,
            bytes_backed_up: backup.bytes_backed_up,
            status: backup.status.as_str().to_string(),
            created_at: backup.created_at,
            updated_at: backup.updated_at,
            restored_at: backup.restored_at,
            deleted_at: backup.deleted_at,
        })
        .collect();
    Ok(success(
        serde_json::to_value(FilesystemBackupListResult { backups, truncated })
            .map_err(|error| error.to_string())?,
    ))
}

async fn filesystem_backup(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
    request_context: &HostToolExecuteRequest,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<FilesystemBackupInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("filesystem.backup input is invalid: {error}"),
                true,
                None,
            ))
        }
    };
    let request = match AgentBackupRequest::check(&input.path, &backup_token()) {
        Ok(request) => request,
        Err(error) => return Ok(filesystem_failure(error, cancel)),
    };
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let service = remote_file_service(state);
    let backup = tokio::select! {
        result = service.agent_backup(server_id, &request) => match result {
            Ok(backup) => backup,
            Err(error) => return Ok(filesystem_failure(error, cancel)),
        },
        _ = cancel.cancelled() => return Ok(cancelled_failure()),
    };
    let now = yukinal_core::sidecar::iso8601_now();
    let bytes_backed_up = match i64::try_from(backup.bytes_backed_up) {
        Ok(bytes) => bytes,
        Err(_) => {
            return Ok(failed(
                "internal",
                "filesystem backup byte count exceeded the local ledger range",
                false,
                None,
            ))
        }
    };
    let record = FilesystemBackupRecord {
        id: backup_record_id(server_id, &backup.backup_path),
        server_id: server_id.to_string(),
        task_id: request_context.task_id.clone(),
        plan_id: request_context.plan_id.clone(),
        plan_step_id: request_context.plan_step_id.clone(),
        trace_id: Some(request_context.trace_id.clone()),
        call_id: Some(request_context.call_id.clone()),
        path: backup.path.clone(),
        backup_path: backup.backup_path.clone(),
        revision: backup.revision.clone(),
        bytes_backed_up,
        status: FilesystemBackupStatus::Available,
        created_at: now.clone(),
        updated_at: now,
        restored_at: None,
        deleted_at: None,
    };
    if let Err(error) = state.database.filesystem_backups().insert(&record) {
        return Ok(failed(
            "internal",
            "backup was created remotely but could not be registered in the host ledger; preserve it and reconcile before restoring",
            false,
            Some(json!({ "database": error.to_string(), "backupPath": backup.backup_path })),
        ));
    }
    Ok(success(
        serde_json::to_value(FilesystemBackupResult {
            path: backup.path,
            backup_path: backup.backup_path,
            revision: backup.revision,
            bytes_backed_up: backup.bytes_backed_up,
        })
        .map_err(|error| error.to_string())?,
    ))
}

async fn filesystem_backup_cleanup(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
    request_context: &HostToolExecuteRequest,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<FilesystemBackupCleanupInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("filesystem.backup.cleanup input is invalid: {error}"),
                true,
                None,
            ))
        }
    };
    let request = match AgentCleanupBackupRequest::check(
        &input.path,
        &input.backup_path,
        &input.expected_revision,
    ) {
        Ok(request) => request,
        Err(error) => return Ok(filesystem_failure(error, cancel)),
    };
    let backup = match state
        .database
        .filesystem_backups()
        .find_available(server_id, &input.path, &input.backup_path)
    {
        Ok(Some(record)) => record,
        Ok(None) => {
            return Ok(failed(
                "denied_by_policy",
                "filesystem.backup.cleanup requires an available backup previously created by this host for the same server and target path",
                false,
                None,
            ))
        }
        Err(error) => {
            return Ok(failed(
                "internal",
                format!("could not read the filesystem backup ledger: {error}"),
                false,
                None,
            ))
        }
    };
    if !backup_owner_matches_request(&backup, request_context) {
        return Ok(failed(
            "denied_by_policy",
            "filesystem.backup.cleanup cannot use a backup owned by another investigation task",
            false,
            Some(json!({
                "backupTaskId": backup.task_id,
                "requestTaskId": request_context.task_id,
            })),
        ));
    }
    if !backup
        .revision
        .eq_ignore_ascii_case(&input.expected_revision)
    {
        return Ok(failed(
            "invalid_input",
            "expectedRevision does not match the revision recorded for this backup; preserve it and re-read the ledger",
            false,
            Some(json!({
                "expectedRevision": backup.revision,
                "actualRevision": input.expected_revision,
            })),
        ));
    }
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let service = remote_file_service(state);
    let cleanup = tokio::select! {
        result = service.agent_cleanup_backup(server_id, &request) => match result {
            Ok(cleanup) => cleanup,
            Err(error) => return Ok(filesystem_failure(error, cancel)),
        },
        _ = cancel.cancelled() => return Ok(cancelled_failure()),
    };
    if let Err(error) = state.database.filesystem_backups().mark_deleted(
        server_id,
        &cleanup.path,
        &cleanup.backup_path,
        &yukinal_core::sidecar::iso8601_now(),
    ) {
        return Ok(failed(
            "internal",
            "filesystem.backup.cleanup removed the remote backup but the host could not consume its ledger entry; reconcile before any further cleanup",
            false,
            Some(json!({ "database": error.to_string(), "backupPath": cleanup.backup_path })),
        ));
    }
    Ok(success(
        serde_json::to_value(FilesystemBackupCleanupResult {
            path: cleanup.path,
            backup_path: cleanup.backup_path,
            revision: cleanup.revision,
            bytes_deleted: cleanup.bytes_deleted,
        })
        .map_err(|error| error.to_string())?,
    ))
}

async fn filesystem_restore(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
    request_context: &HostToolExecuteRequest,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<FilesystemRestoreInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("filesystem.restore input is invalid: {error}"),
                true,
                None,
            ))
        }
    };
    let request =
        match AgentRestoreRequest::check(&input.path, &input.backup_path, &input.expected_revision)
        {
            Ok(request) => request,
            Err(error) => return Ok(filesystem_failure(error, cancel)),
        };
    let backup = match state
        .database
        .filesystem_backups()
        .find_available(server_id, &input.path, &input.backup_path)
    {
        Ok(Some(record)) => record,
        Ok(None) => {
            return Ok(failed(
                "denied_by_policy",
                "filesystem.restore requires an available backup previously created by this host for the same server and target path",
                false,
                None,
            ))
        }
        Err(error) => {
            return Ok(failed(
                "internal",
                format!("could not read the filesystem backup ledger: {error}"),
                false,
                None,
            ))
        }
    };
    if !backup_owner_matches_request(&backup, request_context) {
        return Ok(failed(
            "denied_by_policy",
            "filesystem.restore cannot use a backup owned by another investigation task",
            false,
            Some(json!({
                "backupTaskId": backup.task_id,
                "requestTaskId": request_context.task_id,
            })),
        ));
    }
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let service = remote_file_service(state);
    let restore = tokio::select! {
        result = service.agent_restore(server_id, &request) => match result {
            Ok(restore) => restore,
            Err(error) => return Ok(filesystem_failure(error, cancel)),
        },
        _ = cancel.cancelled() => return Ok(cancelled_failure()),
    };
    if let Err(error) = state.database.filesystem_backups().mark_restored(
        server_id,
        &restore.path,
        &restore.backup_path,
        &yukinal_core::sidecar::iso8601_now(),
    ) {
        return Ok(failed(
            "internal",
            "filesystem.restore changed the remote file but the host could not consume its backup ledger entry; reconcile before any further restore",
            false,
            Some(json!({ "database": error.to_string(), "backupPath": restore.backup_path })),
        ));
    }
    Ok(success(
        serde_json::to_value(FilesystemRestoreResult {
            path: restore.path,
            backup_path: restore.backup_path,
            revision: restore.revision,
            bytes_before: restore.bytes_before,
            bytes_after: restore.bytes_after,
        })
        .map_err(|error| error.to_string())?,
    ))
}

async fn docker_ps(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<DockerPsInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("docker.ps input is invalid: {error}"),
                true,
                None,
            ))
        }
    };
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let session = match state.terminals.cached_session(server_id) {
        Ok(session) => session,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    let command = if input.all.unwrap_or(false) {
        DOCKER_PS_ALL_COMMAND
    } else {
        DOCKER_PS_COMMAND
    };
    let result = match state
        .ssh
        .execute(
            &session,
            command,
            Some(std::time::Duration::from_secs(10)),
            cancel,
        )
        .await
    {
        Ok(result) => result,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };

    // A missing Docker binary or a non-Docker host is a valid, structured answer.
    if result.exit_code != 0 {
        return Ok(success(json!({ "available": false, "containers": [] })));
    }
    Ok(success(json!({
        "available": true,
        "containers": parse_docker_ps(&result.stdout_lossy()),
    })))
}

async fn docker_logs(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<DockerLogsInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("docker.logs input is invalid: {error}"),
                true,
                None,
            ))
        }
    };
    if !is_safe_container_ref(&input.container) {
        return Ok(failed(
            "invalid_input",
            "container must be a Docker name or id without shell metacharacters",
            true,
            None,
        ));
    }
    let tail = input.tail.unwrap_or(DEFAULT_LOG_TAIL);
    if !(1..=MAX_LOG_TAIL).contains(&tail) {
        return Ok(failed(
            "invalid_input",
            format!("tail must be between 1 and {MAX_LOG_TAIL}"),
            true,
            None,
        ));
    }
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let session = match state.terminals.cached_session(server_id) {
        Ok(session) => session,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    let command = format!(
        "docker logs --tail {tail} --timestamps -- {} 2>&1",
        shell_quote(&input.container)
    );
    let result = match state
        .ssh
        .execute(
            &session,
            &command,
            Some(std::time::Duration::from_secs(10)),
            cancel,
        )
        .await
    {
        Ok(result) => result,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    if result.exit_code != 0 {
        return Ok(failed(
            "execution_failed",
            format!("could not read logs for container `{}`", input.container),
            false,
            Some(json!({
                "exitCode": result.exit_code,
                "stderr": truncate_text(&result.stderr_lossy(), 1_000),
            })),
        ));
    }

    let (lines, truncated) = bounded_log_lines(&result.stdout_lossy(), tail);
    Ok(success(
        serde_json::to_value(DockerLogsResult {
            container: input.container,
            lines,
            truncated,
        })
        .map_err(|error| error.to_string())?,
    ))
}

async fn docker_inspect(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<DockerInspectInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("docker.inspect input is invalid: {error}"),
                true,
                None,
            ))
        }
    };
    if !is_safe_container_ref(&input.container) {
        return Ok(failed(
            "invalid_input",
            "container must be a Docker name or id without shell metacharacters",
            true,
            None,
        ));
    }
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let session = match state.terminals.cached_session(server_id) {
        Ok(session) => session,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    let command = format!(
        "docker inspect --format '{{{{json .}}}}' -- {} 2>/dev/null",
        shell_quote(&input.container)
    );
    let result = match state
        .ssh
        .execute(
            &session,
            &command,
            Some(std::time::Duration::from_secs(10)),
            cancel,
        )
        .await
    {
        Ok(result) => result,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    if result.exit_code != 0 {
        return Ok(failed(
            "not_found",
            format!("container `{}` was not found", input.container),
            false,
            Some(json!({
                "exitCode": result.exit_code,
                "stderr": truncate_text(&result.stderr_lossy(), 1_000),
            })),
        ));
    }
    let inspected = match parse_docker_inspect(&result.stdout_lossy()) {
        Ok(inspected) => inspected,
        Err(error) => return Ok(failed("execution_failed", error, false, None)),
    };
    Ok(success(
        serde_json::to_value(inspected).map_err(|error| error.to_string())?,
    ))
}

async fn docker_restart(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<DockerRestartInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("docker.restart input is invalid: {error}"),
                true,
                None,
            ))
        }
    };
    if !is_safe_container_ref(&input.container) {
        return Ok(failed(
            "invalid_input",
            "container must be a Docker name or id without shell metacharacters",
            true,
            None,
        ));
    }
    let timeout = input.timeout_seconds.unwrap_or(DEFAULT_RESTART_TIMEOUT);
    if !(1..=MAX_RESTART_TIMEOUT).contains(&timeout) {
        return Ok(failed(
            "invalid_input",
            format!("timeoutSeconds must be between 1 and {MAX_RESTART_TIMEOUT}"),
            true,
            None,
        ));
    }
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let session = match state.terminals.cached_session(server_id) {
        Ok(session) => session,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    let command = docker_restart_command(&input.container, timeout);
    let result = match state
        .ssh
        .execute_once(
            &session,
            &command,
            Some(std::time::Duration::from_secs(30)),
            cancel,
        )
        .await
    {
        Ok(result) => result,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    if result.exit_code != 0 {
        return Ok(failed(
            "execution_failed",
            format!("could not restart container `{}`", input.container),
            false,
            Some(json!({
                "exitCode": result.exit_code,
                "stderr": truncate_text(&result.stderr_lossy(), 1_000),
            })),
        ));
    }
    Ok(success(
        serde_json::to_value(DockerRestartResult {
            container: input.container,
            restarted: true,
        })
        .map_err(|error| error.to_string())?,
    ))
}

async fn systemd_inspect(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<SystemdInspectInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("systemd.inspect input is invalid: {error}"),
                true,
                None,
            ))
        }
    };
    if !is_safe_systemd_service_ref(&input.service) {
        return Ok(failed(
            "invalid_input",
            "service must be a .service unit without shell metacharacters",
            true,
            None,
        ));
    }
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let session = match state.terminals.cached_session(server_id) {
        Ok(session) => session,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    let command = systemd_inspect_command(&input.service);
    let result = match state
        .ssh
        .execute(
            &session,
            &command,
            Some(std::time::Duration::from_secs(10)),
            cancel,
        )
        .await
    {
        Ok(result) => result,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    if result.exit_code != 0 {
        return Ok(failed(
            "not_found",
            format!(
                "systemd service `{}` was not found or could not be inspected",
                input.service
            ),
            false,
            Some(json!({
                "exitCode": result.exit_code,
                "stderr": truncate_text(&result.stderr_lossy(), 1_000),
            })),
        ));
    }
    let inspected = match parse_systemd_inspect(&result.stdout_lossy(), &input.service) {
        Ok(inspected) => inspected,
        Err(error) => return Ok(failed("execution_failed", error, false, None)),
    };
    Ok(success(
        serde_json::to_value(inspected).map_err(|error| error.to_string())?,
    ))
}

async fn systemd_restart(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<SystemdRestartInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("systemd.restart input is invalid: {error}"),
                true,
                None,
            ))
        }
    };
    if !is_safe_systemd_service_ref(&input.service) {
        return Ok(failed(
            "invalid_input",
            "service must be a .service unit without shell metacharacters",
            true,
            None,
        ));
    }
    let timeout = input
        .timeout_seconds
        .unwrap_or(DEFAULT_SYSTEMD_RESTART_TIMEOUT);
    if !(1..=MAX_SYSTEMD_RESTART_TIMEOUT).contains(&timeout) {
        return Ok(failed(
            "invalid_input",
            format!("timeoutSeconds must be between 1 and {MAX_SYSTEMD_RESTART_TIMEOUT}"),
            true,
            None,
        ));
    }
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let session = match state.terminals.cached_session(server_id) {
        Ok(session) => session,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    let command = systemd_restart_command(&input.service);
    let result = match state
        .ssh
        .execute_once(
            &session,
            &command,
            Some(std::time::Duration::from_secs(timeout as u64)),
            cancel,
        )
        .await
    {
        Ok(result) => result,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    if result.exit_code != 0 {
        return Ok(failed(
            "execution_failed",
            format!("could not restart systemd service `{}`", input.service),
            false,
            Some(json!({
                "exitCode": result.exit_code,
                "stderr": truncate_text(&result.stderr_lossy(), 1_000),
            })),
        ));
    }
    Ok(success(
        serde_json::to_value(SystemdRestartResult {
            service: input.service,
            restarted: true,
        })
        .map_err(|error| error.to_string())?,
    ))
}

async fn package_inspect(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<PackageInspectInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("package.inspect input is invalid: {error}"),
                true,
                None,
            ))
        }
    };
    if !is_supported_package_manager(&input.manager) {
        return Ok(failed(
            "invalid_input",
            "package manager must be apt or dnf",
            true,
            None,
        ));
    }
    if !is_safe_package_ref(&input.package) {
        return Ok(failed(
            "invalid_input",
            "package must be a safe reference without shell metacharacters",
            true,
            None,
        ));
    }
    let Some(command) = package_inspect_command(&input.manager, &input.package) else {
        return Ok(failed(
            "invalid_input",
            "package.inspect could not construct a safe query",
            true,
            None,
        ));
    };
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let session = match state.terminals.cached_session(server_id) {
        Ok(session) => session,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    let result = match state
        .ssh
        .execute(
            &session,
            &command,
            Some(std::time::Duration::from_secs(10)),
            cancel,
        )
        .await
    {
        Ok(result) => result,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    if result.exit_code != 0 {
        return Ok(failed(
            "execution_failed",
            format!("could not inspect package `{}`", input.package),
            false,
            Some(json!({
                "exitCode": result.exit_code,
                "stderr": truncate_text(&result.stderr_lossy(), 1_000),
            })),
        ));
    }
    match parse_package_inspect(&result.stdout_lossy(), &input.manager, &input.package) {
        Ok(inspected) => Ok(success(
            serde_json::to_value(inspected).map_err(|error| error.to_string())?,
        )),
        Err(error) if error.starts_with("package manager ") => {
            Ok(failed("unsupported", error, false, None))
        }
        Err(error) => Ok(failed("execution_failed", error, false, None)),
    }
}

async fn package_install(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<PackageInstallInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("package.install input is invalid: {error}"),
                true,
                None,
            ))
        }
    };
    if !is_supported_package_manager(&input.manager) {
        return Ok(failed(
            "invalid_input",
            "package manager must be apt or dnf",
            true,
            None,
        ));
    }
    if !is_safe_package_ref(&input.package) {
        return Ok(failed(
            "invalid_input",
            "package must be a safe reference without shell metacharacters",
            true,
            None,
        ));
    }
    if let Some(version) = input.version.as_deref() {
        if !is_safe_package_version(version) {
            return Ok(failed(
                "invalid_input",
                "package version contains unsupported characters",
                true,
                None,
            ));
        }
    }
    let timeout = input
        .timeout_seconds
        .unwrap_or(DEFAULT_PACKAGE_INSTALL_TIMEOUT);
    if !(1..=MAX_PACKAGE_INSTALL_TIMEOUT).contains(&timeout) {
        return Ok(failed(
            "invalid_input",
            format!("timeoutSeconds must be between 1 and {MAX_PACKAGE_INSTALL_TIMEOUT}"),
            true,
            None,
        ));
    }
    let command =
        match package_install_command(&input.manager, &input.package, input.version.as_deref()) {
            Ok(command) => command,
            Err(error) => return Ok(failed("invalid_input", error, true, None)),
        };
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let session = match state.terminals.cached_session(server_id) {
        Ok(session) => session,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    let result = match state
        .ssh
        .execute_once(
            &session,
            &command,
            Some(std::time::Duration::from_secs(timeout as u64)),
            cancel,
        )
        .await
    {
        Ok(result) => result,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    if result.exit_code != 0 {
        return Ok(failed(
            "execution_failed",
            format!("could not install package `{}`", input.package),
            false,
            Some(json!({
                "exitCode": result.exit_code,
                "stderr": truncate_text(&result.stderr_lossy(), 1_000),
            })),
        ));
    }
    Ok(success(
        serde_json::to_value(PackageInstallResult {
            manager: input.manager,
            package: input.package,
            version: input.version,
            installed: true,
        })
        .map_err(|error| error.to_string())?,
    ))
}

fn is_empty_object(value: &Value) -> bool {
    value.as_object().is_some_and(serde_json::Map::is_empty)
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use serde_json::json;
    use sha2::{Digest, Sha256};
    use tokio_util::sync::CancellationToken;
    use yukinal_filesystem::Error as FilesystemError;

    use super::{
        backup_owner_matches_request, backup_record_id, baseline_artifact_matches_plan,
        cancel_sidecar_request, collect_json_diff, compare_text_content,
        effectful_tool_requires_durable_plan, evidence_freshness_at, filesystem_failure,
        finish_host_tool_call, handle_evidence_correlation, handle_sidecar_request_with_cancel,
        host_tool_call_status, input_bindings_match, merge_plan_runtime,
        normalize_new_plan_runtime, observation_configuration_matches,
        plan_allows_repeated_observation, plan_binding_matches, plan_definition,
        prepare_host_tool_call, reset_observation_window, success, task_allows_auto_medium_action,
        task_guardrail_violation, validate_observation_window, validate_playbook_step,
        HostCancellationRegistry, HostToolCallDecision, HostToolExecuteRequest, HostToolTarget,
        DOCKER_LOGS, FILESYSTEM_BACKUP, FILESYSTEM_BACKUP_CLEANUP, FILESYSTEM_EDIT,
        FILESYSTEM_WRITE, HOST_TOOL_EXECUTE, SERVER_INFO,
    };
    use crate::state::AppState;
    use yukinal_database::models::{
        Environment, Evidence, EvidenceContentType, EvidenceKind, EvidenceRedactionStatus,
        InvestigationArtifact, InvestigationObservationWindow, InvestigationPermissionMode,
        InvestigationPlan, InvestigationPlanApproval, InvestigationPlanStep, InvestigationRun,
        InvestigationRunMode, InvestigationRunStatus, InvestigationTarget, InvestigationTargetHost,
        InvestigationTask, InvestigationTaskGuardrails, ObservationWindowStatus,
        PlanApprovalSource, PlanApprovalStatus, PlanIdempotency, PlanStatus, PlanStepKind,
        PlanStepStatus, RiskLevel, TaskArtifactKind, TaskArtifactStatus, TaskAutomationLevel,
        TaskBudget, TaskPhase, TaskStatus,
    };
    use yukinal_database::repositories::{FilesystemBackupRecord, FilesystemBackupStatus};

    fn guarded_write_request(content: &str) -> HostToolExecuteRequest {
        HostToolExecuteRequest {
            call_id: "call_guarded_write".into(),
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
        }
    }

    #[test]
    fn effectful_agent_calls_fail_closed_without_a_complete_durable_plan_binding() {
        let mut ordinary = guarded_write_request("one");
        ordinary.task_id = None;
        ordinary.plan_id = None;
        ordinary.plan_step_id = None;
        assert!(effectful_tool_requires_durable_plan(&ordinary));

        let mut incomplete = guarded_write_request("one");
        incomplete.plan_step_id = None;
        assert!(effectful_tool_requires_durable_plan(&incomplete));

        let planned = guarded_write_request("one");
        assert!(!effectful_tool_requires_durable_plan(&planned));
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
        let tool =
            task_guardrail_violation(&task, FILESYSTEM_WRITE, &json!({"path": "/etc/app.env"}));
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

        let mut timed = task;
        timed.guardrails.not_before_at = Some("2999-01-01T00:00:00Z".into());
        let window = task_guardrail_violation(&timed, SERVER_INFO, &json!({}))
            .expect("future window must deny");
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

    #[test]
    fn host_action_replay_is_cached_but_a_different_payload_is_refused() {
        let directory =
            std::env::temp_dir().join(format!("yukinal-host-idempotency-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let state = AppState::bootstrap(&directory).expect("bootstrap host state");
        let request = guarded_write_request("one");

        let token = match prepare_host_tool_call(&state, &request).expect("first claim") {
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
        match prepare_host_tool_call(&state, &request).expect("same call") {
            HostToolCallDecision::Respond(replayed) => assert_eq!(replayed, response_value),
            other => panic!("same action was not replayed: {other:?}"),
        }

        // A resumed task may receive a fresh provider call ID. The logical
        // plan binding still identifies it as the same remote action.
        let mut resumed = request.clone();
        resumed.call_id = "call_guarded_write_after_restart".into();
        match prepare_host_tool_call(&state, &resumed).expect("new call id") {
            HostToolCallDecision::Respond(ref refusal) => {
                assert_eq!(refusal["error"]["code"], json!("plan_deviation"));
                assert_eq!(refusal["error"]["detail"]["code"], json!("duplicate_call"));
            }
            other => panic!("new call id was not refused: {other:?}"),
        }

        let changed = guarded_write_request("two");
        match prepare_host_tool_call(&state, &changed).expect("changed call") {
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
        match prepare_host_tool_call(&reopened, &request).expect("replayed after restart") {
            HostToolCallDecision::Respond(ref refusal) => {
                assert_eq!(refusal["error"]["code"], json!("plan_deviation"));
                assert_eq!(refusal["error"]["detail"]["status"], json!("success"));
            }
            other => panic!("restart replay was not refused: {other:?}"),
        }
        drop(reopened);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn planned_read_replay_is_refused_before_a_second_remote_execution() {
        let directory =
            std::env::temp_dir().join(format!("yukinal-host-read-fence-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let state = AppState::bootstrap(&directory).expect("bootstrap host state");
        let request = HostToolExecuteRequest {
            call_id: "call_server_info_1".into(),
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
        };

        let token = match prepare_host_tool_call(&state, &request).expect("first read claim") {
            HostToolCallDecision::Execute(Some(token)) => token,
            other => panic!("first planned read was not claimed: {other:?}"),
        };
        let response = Ok(success(json!({ "hostname": "fixture" })));
        finish_host_tool_call(&state, &request, Some(token), &response)
            .expect("finish planned read");

        // A provider retry commonly has a new call ID. The durable plan step,
        // not that transient ID, is the identity of the logical observation.
        let mut retry = request.clone();
        retry.call_id = "call_server_info_2".into();
        match prepare_host_tool_call(&state, &retry).expect("replayed planned read") {
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
            prepare_host_tool_call(&state, &ordinary).expect("ordinary read claim"),
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
        observation_plan.observation_window.as_mut().unwrap().status =
            ObservationWindowStatus::Pending;
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

        let mut ask_task = task.clone();
        ask_task.permission_mode = InvestigationPermissionMode::Ask;
        assert!(!task_allows_auto_medium_action(&ask_task, &step));
        let mut production_task = task.clone();
        production_task.scope.environment = Environment::Production;
        assert!(!task_allows_auto_medium_action(&production_task, &step));
        let mut high_step = step.clone();
        high_step.risk_level = Some(RiskLevel::High);
        assert!(!task_allows_auto_medium_action(&task, &high_step));
        let mut cleanup_step = step.clone();
        cleanup_step.allowed_tools = vec![FILESYSTEM_BACKUP_CLEANUP.into()];
        assert!(!task_allows_auto_medium_action(&task, &cleanup_step));
        let mut ordinary_write_step = step;
        ordinary_write_step.allowed_tools = vec![FILESYSTEM_WRITE.into()];
        assert!(!task_allows_auto_medium_action(&task, &ordinary_write_step));
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
        assert!(
            validate_observation_window(&window, std::slice::from_ref(&verification)).is_none()
        );
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
}
