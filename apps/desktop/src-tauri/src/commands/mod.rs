//! The complete native surface available to React (-R9-R10).
//!
//! Every command mirrors a key of `IpcCommandMap` in `@yukinal/shared`; field naming is
//! camelCase on both sides. If a command is not in that map, it does not exist for the
//! UI and must not be added here.

use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::state::AppState;
use yukinal_core::ipc::{AgentKillResponse, AgentLogsResponse, AgentSpawnResponse, PingResponse};
use yukinal_core::sidecar::{SidecarConfig, SidecarEvent};
use yukinal_core::supervisor::{SupervisorStatus, LOG_HISTORY};
use yukinal_database::models::{
    Activity, ActivityOutcome, ActivitySource, ActivityType, Environment, PermissionMode,
    RiskLevel, ToolExecutionRecord, ToolExecutionStatus,
};

pub mod activity;
pub mod agent_run;
pub mod execution;
pub mod files;
pub mod host;
pub mod logs;
pub mod provider;
pub mod server;
pub mod services;
pub mod terminal;
pub mod workspace;

/// Smoke test: proves the IPC round trip without pretending to do real work.
#[tauri::command]
pub fn core_ping() -> PingResponse {
    PingResponse {
        version: env!("CARGO_PKG_VERSION"),
        os: std::env::consts::OS,
    }
}

/// Launch the agent sidecar and handshake with it. React never spawns processes:
/// ownership of the child stays on this side of the boundary (ADR 0001).
#[tauri::command]
pub async fn agent_spawn(app: AppHandle) -> Result<AgentSpawnResponse, String> {
    start_sidecar(&app).await
}

/// The only code path that starts a sidecar. The dev autostart hook calls this same
/// function, so an automated run exercises exactly what a user click does (config
/// resolution, app-data dir, handshake, event forwarding).
pub(crate) async fn start_sidecar(app: &AppHandle) -> Result<AgentSpawnResponse, String> {
    let config = resolve_config(app)?;
    let outcome = app
        .state::<AppState>()
        .supervisor
        .start(&config)
        .await
        .map_err(|error| error.to_string())?;

    if !outcome.already_running {
        forward_sidecar_events(app.clone());
    }

    Ok(AgentSpawnResponse {
        pid: outcome.runtime.pid,
        protocol_version: outcome.runtime.protocol_version,
        agent_version: outcome.runtime.agent_version,
        entry: outcome.runtime.entry,
        tool_count: outcome.runtime.tool_count,
        already_running: outcome.already_running,
    })
}

#[tauri::command]
pub async fn agent_status(state: State<'_, AppState>) -> Result<SupervisorStatus, String> {
    Ok(state.supervisor.status().await)
}

#[tauri::command]
pub async fn agent_kill(state: State<'_, AppState>) -> Result<AgentKillResponse, String> {
    Ok(AgentKillResponse {
        killed: state.supervisor.stop().await,
    })
}

/// Recent sidecar stderr, for the "why did it die" affordance.
#[tauri::command]
pub async fn agent_logs(state: State<'_, AppState>) -> Result<AgentLogsResponse, String> {
    Ok(AgentLogsResponse {
        lines: state.supervisor.logs().await,
        capacity: LOG_HISTORY,
    })
}

/// Decide what to launch. Resolution order lives in `SidecarConfig::from_env_with_cwd`
/// (ADR 0008); this only supplies the app data dir when the caller did not set one.
fn resolve_config(app: &AppHandle) -> Result<SidecarConfig, String> {
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let mut config = SidecarConfig::from_env_with_cwd(&cwd).map_err(|error| error.to_string())?;

    if config.data_dir.trim().is_empty() {
        let data_dir = app
            .path()
            .app_data_dir()
            .map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&data_dir).map_err(|error| error.to_string())?;
        config.data_dir = data_dir.display().to_string();
    }
    let data_dir = config.data_dir.clone();
    Ok(config.with_env("YUKINAL_DATA_DIR", &data_dir))
}

/// One task per launched sidecar: keeps the child's stderr visible in the desktop log
/// until the event → Tauri event mapping is not implemented yet.
fn forward_sidecar_events(app: AppHandle) {
    let supervisor = app.state::<AppState>().supervisor.clone();
    let mut receiver = supervisor.subscribe();
    tauri::async_runtime::spawn(async move {
        // The sidecar's startup lines are written before this task exists, and a
        // broadcast channel does not replay them. Print the retained tail first so
        // "what the agent said when it booted" is never invisible.
        for line in supervisor.logs().await {
            eprintln!("[agent] {line}");
        }
        loop {
            match receiver.recv().await {
                Ok(event) => match event {
                    SidecarEvent::Log(line) => eprintln!("[agent] {line}"),
                    // 上行通知：`agent.stream` 的 payload 是 AgentStreamEvent，按
                    // 其 type 原样转成 Tauri 事件（agent.thinking / tool_call / …）。
                    SidecarEvent::Frame(frame) => forward_agent_frame(&app, &frame),
                    SidecarEvent::Request { id, method, params } => {
                        let state = app.state::<AppState>();
                        let outcome = host::handle_sidecar_request(&state, &method, params).await;
                        if let Some(handle) = state.supervisor.handle().await {
                            if let Err(error) = handle.respond(id, outcome).await {
                                eprintln!("[agent] host response failed: {error}");
                            }
                        }
                    }
                    SidecarEvent::Exited { code, signal } => {
                        eprintln!("[agent] exited code={code:?} signal={signal:?}");
                        break;
                    }
                },
                Err(error) => {
                    eprintln!("[agent] event stream closed: {error}");
                    break;
                }
            }
        }
    });
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentToolResultEvent {
    trace_id: String,
    step_id: String,
    call_id: String,
    tool_name: String,
    input: Value,
    target: AgentToolTarget,
    risk_level: RiskLevel,
    decision: PermissionMode,
    approved_by: Option<AgentApprovalSource>,
    status: ToolExecutionStatus,
    output_summary: String,
    error: Option<String>,
    started_at: String,
    ended_at: String,
    duration_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentToolTarget {
    host: AgentToolHost,
    server_id: Option<String>,
    workspace_id: Option<String>,
    environment: Environment,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum AgentToolHost {
    Local,
    Remote,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum AgentApprovalSource {
    User,
    Policy,
}

const MAX_AUDIT_TEXT_CHARS: usize = 4_000;
const MAX_AUDIT_INPUT_TEXT_CHARS: usize = 2_000;

fn persist_agent_tool_result(app: &AppHandle, params: &Value) {
    let event = match serde_json::from_value::<AgentToolResultEvent>(params.clone()) {
        Ok(event) => event,
        Err(error) => {
            eprintln!("[agent] ignored malformed tool result event: {error}");
            return;
        }
    };

    if event.trace_id.trim().is_empty()
        || event.step_id.trim().is_empty()
        || event.call_id.trim().is_empty()
        || event.tool_name.trim().is_empty()
        || event.started_at.trim().is_empty()
        || event.ended_at.trim().is_empty()
        || event.duration_ms > i64::MAX as u64
    {
        eprintln!("[agent] ignored tool result event with an invalid audit identity");
        return;
    }

    if !is_valid_agent_target(&event.target) {
        eprintln!("[agent] ignored tool result event with an invalid target");
        return;
    }

    let summary = safe_audit_summary(&event.output_summary, MAX_AUDIT_TEXT_CHARS);
    let error = event
        .error
        .as_deref()
        .map(|value| safe_audit_summary(value, MAX_AUDIT_TEXT_CHARS))
        .or_else(|| (event.decision == PermissionMode::Deny).then(|| summary.clone()));
    let output = if event.status == ToolExecutionStatus::Success {
        Some(json!({ "summary": summary.clone() }))
    } else {
        None
    };
    let outcome = match event.status {
        ToolExecutionStatus::Success => ActivityOutcome::Success,
        ToolExecutionStatus::Cancelled => ActivityOutcome::Cancelled,
        _ if event.decision == PermissionMode::Deny => ActivityOutcome::Denied,
        _ => ActivityOutcome::Failure,
    };
    let activity_description = output
        .as_ref()
        .and_then(|value| value.get("summary"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| error.clone());
    let tool_name = bounded_audit_text(&event.tool_name, 160);
    let record = ToolExecutionRecord {
        trace_id: event.trace_id.clone(),
        step_id: bounded_audit_text(&event.step_id, 160),
        call_id: bounded_audit_text(&event.call_id, 160),
        tool_name: tool_name.clone(),
        server_id: event.target.server_id.clone(),
        environment: event.target.environment,
        risk_level: event.risk_level,
        decision: event.decision,
        approved_by: event.approved_by.map(|source| match source {
            AgentApprovalSource::User => "user".to_string(),
            AgentApprovalSource::Policy => "policy".to_string(),
        }),
        status: event.status,
        input: sanitize_audit_input(event.input),
        output,
        error,
        started_at: bounded_audit_text(&event.started_at, 80),
        ended_at: Some(bounded_audit_text(&event.ended_at, 80)),
        duration_ms: Some(event.duration_ms),
    };

    let state = app.state::<AppState>();
    if let Err(error) = state.database.executions().insert(&record) {
        eprintln!("[agent] failed to persist tool execution: {error}");
        return;
    }

    let activity = Activity {
        id: crate::commands::server::next_id("act"),
        server_id: record.server_id.clone(),
        workspace_id: event.target.workspace_id,
        r#type: ActivityType::AgentAction,
        title: format!("Agent 执行 {tool_name}"),
        description: activity_description,
        source: ActivitySource::Agent,
        actor: "agent".to_string(),
        reason: Some("Agent 按已解析目标和权限决策执行工具".to_string()),
        outcome: Some(outcome),
        trace_id: Some(record.trace_id.clone()),
        created_at: record
            .ended_at
            .clone()
            .unwrap_or_else(|| record.started_at.clone()),
    };
    if let Err(error) = state.database.activities().insert(&activity) {
        eprintln!("[agent] failed to persist tool activity: {error}");
    } else if let Ok(payload) = serde_json::to_value(&activity) {
        let _ = app.emit("activity.created", payload);
    }
}

fn is_valid_agent_target(target: &AgentToolTarget) -> bool {
    match (&target.host, target.server_id.as_deref()) {
        (AgentToolHost::Remote, Some(server_id)) => is_stable_server_id(server_id),
        (AgentToolHost::Local, None) => true,
        _ => false,
    }
}

fn is_stable_server_id(value: &str) -> bool {
    value.len() > 4
        && value.starts_with("srv_")
        && value
            .bytes()
            .skip(4)
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn bounded_audit_text(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let bounded: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{bounded}\n…[truncated]")
    } else {
        bounded
    }
}

fn safe_audit_summary(value: &str, max_chars: usize) -> String {
    let lowered = value.to_ascii_lowercase();
    const SENSITIVE_MARKERS: &[&str] = &[
        "api_key",
        "apikey",
        "authorization",
        "password",
        "passwd",
        "private_key",
        "private key",
        "secret",
        "token",
    ];
    if SENSITIVE_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
    {
        return "[sensitive output omitted]".to_string();
    }
    bounded_audit_text(value, max_chars)
}

fn sanitize_audit_input(value: Value) -> Value {
    match value {
        Value::Object(mut object) => {
            for (key, value) in &mut object {
                if is_sensitive_key(key) {
                    *value = Value::String("[redacted]".to_string());
                } else {
                    let nested = std::mem::take(value);
                    *value = sanitize_audit_input(nested);
                }
            }
            Value::Object(object)
        }
        Value::Array(values) => {
            Value::Array(values.into_iter().map(sanitize_audit_input).collect())
        }
        Value::String(value) => {
            Value::String(bounded_audit_text(&value, MAX_AUDIT_INPUT_TEXT_CHARS))
        }
        other => other,
    }
}

fn is_sensitive_key(value: &str) -> bool {
    let normalized: String = value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|character| character.to_ascii_lowercase())
        .collect();
    matches!(
        normalized.as_str(),
        "apikey"
            | "authorization"
            | "credential"
            | "credentials"
            | "password"
            | "passwd"
            | "privatekey"
            | "secret"
            | "token"
    )
}

/// `agent.stream` 通知 → Tauri event（事件名 = AgentStreamEvent.type）。
fn forward_agent_frame(app: &AppHandle, frame: &Value) {
    let Some(method) = frame.get("method").and_then(Value::as_str) else {
        return;
    };
    if method != "agent.stream" {
        return;
    }
    let Some(params) = frame.get("params") else {
        return;
    };
    let Some(event_type) = params.get("type").and_then(Value::as_str) else {
        return;
    };
    if event_type == "agent.tool_result" {
        persist_agent_tool_result(app, params);
    }
    let _ = app.emit(event_type, params.clone());
}

#[cfg(test)]
mod tests {
    use super::{is_stable_server_id, safe_audit_summary, sanitize_audit_input};
    use serde_json::json;

    #[test]
    fn stable_server_ids_are_lowercase_and_scoped() {
        assert!(is_stable_server_id("srv_01abc"));
        assert!(!is_stable_server_id("server_01abc"));
        assert!(!is_stable_server_id("srv_ABC"));
    }

    #[test]
    fn audit_input_redacts_secret_keys_and_bounds_strings() {
        let input = sanitize_audit_input(json!({
            "command": "echo hello",
            "apiKey": "do-not-persist",
            "nested": { "password": "also-do-not-persist" },
        }));
        assert_eq!(input["apiKey"], "[redacted]");
        assert_eq!(input["nested"]["password"], "[redacted]");
        assert_eq!(input["command"], "echo hello");
    }

    #[test]
    fn audit_summary_omits_sensitive_output_and_truncates_other_output() {
        assert_eq!(
            safe_audit_summary("token=do-not-persist", 4000),
            "[sensitive output omitted]"
        );
        assert_eq!(safe_audit_summary("abcdef", 3), "abc\n…[truncated]");
    }
}
