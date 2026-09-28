//! The native surface available to React through the explicit IPC allow-list.
//!
//! Every command mirrors a key of `IpcCommandMap` in `@yukinal/shared`; field naming is
//! camelCase on both sides. If a command is not in that map, it does not exist for the
//! UI and must not be added here.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio_util::sync::CancellationToken;

use crate::state::AppState;
use yukinal_core::ids::is_stable_server_id;
use yukinal_core::ipc::{AgentKillResponse, AgentLogsResponse, AgentSpawnResponse, PingResponse};
use yukinal_core::sidecar::{SidecarConfig, SidecarEvent};
use yukinal_core::supervisor::{SupervisorStatus, LOG_HISTORY};
use yukinal_database::models::{
    Activity, ActivityOutcome, ActivitySource, ActivityType, DecisionBrief, DecisionBriefStatus,
    DecisionOption, DecisionOptionContinuation, DecisionOptionStatus, Environment,
    FailureOptionAction, Finding, FindingConfidence, FindingKind, InvestigationFailure,
    InvestigationFailureOption, InvestigationRunStatus, InvestigationScheduleComparisonStatus,
    InvestigationStep, InvestigationStepKind, InvestigationStepStatus, PermissionMode, RiskLevel,
    TaskArtifactKind, TaskArtifactStatus, TaskFailureCode, TaskPhase, TaskStatus,
    ToolExecutionRecord, ToolExecutionStatus,
};
use yukinal_database::repositories::TaskProgressUpdate;

pub mod activity;
pub mod agent_run;
pub mod chat;
pub mod execution;
pub mod files;
pub mod host;
pub mod host_key;
pub mod investigation;
pub mod logs;
pub mod mcp;
pub mod network;
pub mod provider;
pub mod recovery;
pub mod scheduler;
pub mod server;
pub mod services;
pub mod terminal;
pub mod workspace;

mod audit;
mod event_projection;
pub(crate) mod sidecar;

pub(crate) use audit::safe_audit_summary;
#[cfg(test)]
pub(crate) use audit::{is_terminal_result_status, sanitize_audit_input};
pub(crate) use event_projection::{
    failure_options, forward_agent_frame, persist_investigation_event,
    sync_investigation_task_status,
};
#[cfg(test)]
pub(crate) use event_projection::{
    is_current_investigation_run, is_terminal_investigation_run, next_investigation_status,
    persist_investigation_event_state, sync_investigation_task_status_state,
    task_failure_code_from_tool_error,
};
pub(crate) use sidecar::{forward_sidecar_events, start_sidecar};
/// Map the logical event names shared with the UI to Tauri's event-channel
/// grammar. Tauri channels do not allow `.` even though the payload type names
/// intentionally use dots (for example, `agent.started`).
pub(crate) fn tauri_event_name(name: &str) -> String {
    name.replace('.', ":")
}

/// Explicit empty JSON object returned by commands whose shared IPC contract
/// is `{}`. Returning Rust's unit type would serialize as `null` and make the
/// frontend contract depend on Tauri's unit representation.
#[derive(Debug, Deserialize, Serialize)]
pub struct EmptyResponse {}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod fixture_contracts;
