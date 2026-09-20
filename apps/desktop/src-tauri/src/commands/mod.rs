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
pub mod scheduler;
pub mod server;
pub mod services;
pub mod terminal;
pub mod workspace;

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

    // Nothing to wire up here: the event forwarder belongs to the window (see
    // `forward_sidecar_events`), so a start — from the UI, from the dev autostart hook, or
    // from the supervisor's own restart path — reuses the one that is already running.

    // 逐字段手抄改成 `From`：那条映射现在住在 `crates/core/src/ipc.rs`（契约的所在地），
    // 见那里的注释 —— 手抄的问题不是风格，而是给 RuntimeInfo 加一个字段却忘了这里时，
    // 响应照样编译、照样序列化，只是悄悄少一个字段。
    Ok(AgentSpawnResponse::from(&outcome))
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

/// Decide what to launch. Resolution order lives in
/// `SidecarConfig::from_env_with_resources` (ADR 0008, ADR 0013); this only tells it where
/// this build keeps its resources and supplies the app data dir when the caller did not set
/// one.
///
/// The resource dir is passed in both shapes, and the original comment here was wrong about
/// dev: `tauri dev` *does* stage `bundle.resources` into the target directory
/// (`target/debug/agent/index.js`), so in a dev run the packaged path usually wins and the
/// repo walk-up is the fallback (a bare `cargo run` without Tauri's staging). One resolution
/// path for both shapes is still the point — it is what makes the installed case the
/// exercised one instead of a `cfg!(debug_assertions)` branch nothing runs.
///
/// Whatever this returns hands Node a path that went through
/// `SidecarConfig::for_command_line`: `resource_dir()` is canonicalised, and Node cannot
/// resolve a `\\?\` path — it dies with `EISDIR` on `lstat('C:')` before running the agent.
fn resolve_config(app: &AppHandle) -> Result<SidecarConfig, String> {
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let resources = app.path().resource_dir().ok();
    let mut config = SidecarConfig::from_env_with_resources(&cwd, resources.as_deref())
        .map_err(|error| error.to_string())?;

    if config.data_dir.trim().is_empty() {
        let data_dir = app
            .path()
            .app_data_dir()
            .map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&data_dir).map_err(|error| error.to_string())?;
        config.data_dir = data_dir.display().to_string();
    }
    let data_dir = config.data_dir.clone();
    // 「为什么起不来」的一半答案是「到底起了什么」。一条启动行比事后猜文件名便宜得多：
    // `node C:` 这种崩法在日志里看起来完全不像路径解析问题，而它确实是。
    eprintln!(
        "[yukinal] sidecar command: {} {:?} (data dir {})",
        config.program.display(),
        config.args,
        data_dir
    );
    Ok(config.with_env("YUKINAL_DATA_DIR", &data_dir))
}

/// One task per launched sidecar: keeps stderr visible and maps sidecar notifications
/// onto the desktop event channels.
///
/// Created **once**, from the window setup, not per start. It used to be called by
/// `start_sidecar` for every non-reused start and relied on `SidecarEvent::Exited` to end
/// the task; the supervisor deliberately did not republish that event, so after a crash
/// and a restart two forwarders were attached to the same broadcast channel. Every frame
/// was then forwarded twice, and every `host.tool.execute` request — which is answered by a
/// task spawned per received event — was *executed twice* on the target.
pub(crate) fn forward_sidecar_events(app: AppHandle) {
    let supervisor = app.state::<AppState>().supervisor.clone();
    let shutdown = app.state::<AppState>().shutdown.clone();
    let mut receiver = supervisor.subscribe();
    let cancellations: host::HostCancellationRegistry =
        Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    tauri::async_runtime::spawn(async move {
        // The sidecar's startup lines are written before this task exists, and a
        // broadcast channel does not replay them. Print the retained tail first so
        // "what the agent said when it booted" is never invisible.
        for line in supervisor.logs().await {
            eprintln!("[agent] {line}");
        }
        loop {
            let event = tokio::select! {
                biased;
                _ = shutdown.cancelled() => {
                    if let Ok(pending) = cancellations.lock() {
                        for token in pending.values() {
                            token.cancel();
                        }
                    }
                    break;
                }
                event = receiver.recv() => event,
            };
            match event {
                Ok(event) => match event {
                    SidecarEvent::Log(line) => eprintln!("[agent] {line}"),
                    // 上行通知：`agent.stream` 的 payload 是 AgentStreamEvent，按
                    // 其 type 原样转成 Tauri 事件（agent.thinking / tool_call / …）。
                    SidecarEvent::Frame(frame) => forward_agent_frame(&app, &frame),
                    SidecarEvent::Request { id, method, params } => {
                        // Register the token before spawning the request task. This closes the
                        // race where a cancellation frame arrives immediately after execute.
                        let registration = if method == host::HOST_TOOL_CANCEL {
                            Ok(None)
                        } else {
                            let token = CancellationToken::new();
                            match cancellations.lock() {
                                Ok(mut pending) => {
                                    pending.insert(id, token.clone());
                                    Ok(Some(token))
                                }
                                Err(_) => Err("host cancellation registry is poisoned".to_string()),
                            }
                        };
                        let app = app.clone();
                        let supervisor = supervisor.clone();
                        let cancellations = Arc::clone(&cancellations);
                        tauri::async_runtime::spawn(async move {
                            let is_cancel = method == host::HOST_TOOL_CANCEL;
                            let outcome = if is_cancel {
                                host::cancel_sidecar_request(&cancellations, params)
                            } else {
                                let outcome = match registration {
                                    Ok(Some(token)) => {
                                        let state = app.state::<AppState>();
                                        host::handle_sidecar_request_with_cancel(
                                            &state, &method, params, token,
                                        )
                                        .await
                                    }
                                    Ok(None) => {
                                        Err("host request was not registered for cancellation"
                                            .to_string())
                                    }
                                    Err(error) => Err(error),
                                };
                                if let Ok(mut pending) = cancellations.lock() {
                                    pending.remove(&id);
                                }
                                outcome
                            };
                            if let Some(handle) = supervisor.handle().await {
                                if let Err(error) = handle.respond(id, outcome).await {
                                    eprintln!("[agent] host response failed: {error}");
                                }
                            }
                        });
                    }
                    SidecarEvent::Exited { code, signal } => {
                        // Observed, not a reason to leave: this forwarder is created once for
                        // the life of the window, and the supervisor may already be bringing
                        // a new agent up behind it. Breaking here would silently stop
                        // forwarding the *restarted* agent's frames, and the next start would
                        // have to create a second forwarder — which is how one `host.*`
                        // request ends up executed twice.
                        eprintln!("[agent] exited code={code:?} signal={signal:?}");
                        let state = app.state::<AppState>();
                        let now = yukinal_core::sidecar::iso8601_now();
                        if let Err(error) = state
                            .database
                            .investigations()
                            .interrupt_active_investigation_runs(&now, "sidecar_exited")
                        {
                            eprintln!("[agent] investigation recovery failed: {error}");
                        }
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
    error_code: Option<String>,
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
    Agent,
}

const MAX_AUDIT_TEXT_CHARS: usize = 4_000;
const MAX_AUDIT_INPUT_TEXT_CHARS: usize = 2_000;
const FILE_CONTENT_AUDIT_OMITTED: &str = "[file content omitted from audit]";

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

    // 一个**结果**事件必须是终态。`AgentToolResultEvent.status` 的类型是完整的六值
    // `ToolExecutionStatus`（它按线上契约反序列化，契约里两者共用同一套名字），所以
    // 一个声称 `status: "running"` 的结果事件在类型上是合法的 —— 但它自相矛盾：它同时
    // 带着 `ended_at`。落库就会留下一行「还在跑、但已经结束」的记录，而那正是
    // 「崩溃中断的调用」本该由**缺失**表达的东西，审计里从此分不清两者。
    if !is_terminal_result_status(&event.status) {
        eprintln!("[agent] ignored tool result event whose status is not terminal");
        return;
    }

    let summary =
        if event.tool_name == "filesystem.read" && event.status == ToolExecutionStatus::Success {
            FILE_CONTENT_AUDIT_OMITTED.to_string()
        } else {
            safe_audit_summary(&event.output_summary, MAX_AUDIT_TEXT_CHARS)
        };
    let error = event
        .error
        .as_deref()
        .map(|value| safe_audit_summary(value, MAX_AUDIT_TEXT_CHARS))
        .or_else(|| {
            event
                .error_code
                .as_deref()
                .map(|code| format!("tool error: {code}"))
        })
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
            AgentApprovalSource::Agent => "agent".to_string(),
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
        let _ = app.emit(&tauri_event_name("activity.created"), payload);
    }
}

fn is_valid_agent_target(target: &AgentToolTarget) -> bool {
    match (&target.host, target.server_id.as_deref()) {
        (AgentToolHost::Remote, Some(server_id)) => is_stable_server_id(server_id),
        (AgentToolHost::Local, None) => true,
        _ => false,
    }
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

/// 一个工具**结果**的状态只能是终态。
///
/// `pending` / `running` / `waiting_approval` 描述的是「还没结束」，而结果事件同时带着
/// `ended_at`。放它们进来，审计里就会出现「已结束但还在跑」的行，而崩溃中断的调用在账本
/// 里是**没有行**——两者一旦混同，就再也分不出「跑完了」和「进程死了」。
///
/// 这条规则由契约保证（`TOOL_RESULT_STATUSES` 只有三个取值），但 Rust 侧的类型是从线上
/// 反序列化的完整六值枚举，所以必须在这里挡一次。
fn is_terminal_result_status(status: &ToolExecutionStatus) -> bool {
    matches!(
        status,
        ToolExecutionStatus::Success | ToolExecutionStatus::Failed | ToolExecutionStatus::Cancelled
    )
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
            | "content"
            // `filesystem.edit` 的参数里装着**任意文件内容**，和 `filesystem.write` 的
            // `content` 是同一种东西 —— 一份 `.env` 的改动片段里就有密钥。三者都要标成
            // 敏感，否则审计里只有 write 被抹掉，而 edit 把同一份内容原样留下。
            | "oldstring"
            | "newstring"
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
    if !matches!(
        event_type,
        "agent.started"
            | "agent.thinking"
            | "agent.text"
            | "agent.usage"
            | "agent.tool_call"
            | "agent.tool_result"
            | "agent.waiting_approval"
            | "agent.approval_expired"
            | "agent.completed"
            | "agent.failed"
    ) {
        return;
    }
    let Some(run_id) = params.get("runId").and_then(Value::as_str) else {
        return;
    };
    if run_id.trim().is_empty() || run_id.len() > 256 {
        return;
    }
    // The sidecar transport already caps a frame at 8 MiB. Keep a malformed
    // event from becoming a similarly large Tauri/UI allocation.
    if serde_json::to_vec(params)
        .map(|payload| payload.len() > 1_000_000)
        .unwrap_or(true)
    {
        return;
    }
    sync_investigation_task_status(app, event_type, params);
    persist_investigation_event(app, event_type, params);
    if event_type == "agent.tool_result" {
        persist_agent_tool_result(app, params);
    }
    let _ = app.emit(&tauri_event_name(event_type), params.clone());
}

/// Keep the durable task row in step with the terminal/approval events emitted by a run.
///
/// The sidecar reports what happened; the host still owns the state transition. A malformed or
/// stale event is ignored, and an event can never jump across the transition table in
/// `commands::investigation`.
fn sync_investigation_task_status(app: &AppHandle, event_type: &str, params: &Value) {
    let state = app.state::<AppState>();
    let _ = sync_investigation_task_status_state(&state, event_type, params);
}

/// Apply the host-owned task transition without needing a Tauri runtime. The
/// production event forwarder and the offline cross-layer tests both use this
/// exact state path; UI emission remains outside the helper.
fn sync_investigation_task_status_state(
    state: &AppState,
    event_type: &str,
    params: &Value,
) -> bool {
    let Some(task_id) = params.get("taskId").and_then(Value::as_str) else {
        return false;
    };
    if task_id.trim().is_empty() || task_id.len() > 256 {
        return false;
    }

    let current = match state.database.investigations().get_task(task_id) {
        Ok(task) => task,
        Err(error) => {
            eprintln!("[yukinal] investigation status lookup failed for {task_id}: {error}");
            return false;
        }
    };
    let Some(run_id) = params.get("runId").and_then(Value::as_str) else {
        return false;
    };
    // A task can only be advanced by the run currently admitted for it.  A
    // sidecar may flush a frame after the host has stopped/recovered the old
    // run; accepting that frame here would let stale completion/approval
    // events rewind a newer task state before the finalisation fence below.
    if !is_current_investigation_run(current.active_run_id.as_deref(), run_id) {
        return false;
    }
    let result_state = params
        .get("result")
        .and_then(|result| result.get("state"))
        .and_then(Value::as_str);
    let Some(next) = next_investigation_status(current.status, event_type, result_state) else {
        return false;
    };
    let completed_at = matches!(
        next,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Stopped | TaskStatus::Expired
    )
    .then(yukinal_core::sidecar::iso8601_now);
    if let Err(error) = state
        .database
        .investigations()
        .update_task_status_if_active(
            task_id,
            next,
            &yukinal_core::sidecar::iso8601_now(),
            completed_at.as_deref(),
            run_id,
        )
    {
        eprintln!("[yukinal] investigation status update failed for {task_id}: {error}");
        return false;
    }
    true
}

fn next_investigation_status(
    current: TaskStatus,
    event_type: &str,
    result_state: Option<&str>,
) -> Option<TaskStatus> {
    let candidate = match event_type {
        "agent.waiting_approval" => Some(TaskStatus::WaitingUser),
        "agent.failed" => Some(TaskStatus::Failed),
        "agent.completed" => match result_state {
            Some("cancelled") => Some(TaskStatus::Stopped),
            Some("failed") => Some(TaskStatus::Failed),
            Some("completed") if current == TaskStatus::Executing => Some(TaskStatus::Verifying),
            Some("completed")
                if matches!(
                    current,
                    TaskStatus::Investigating | TaskStatus::WaitingUser | TaskStatus::Verifying
                ) =>
            {
                Some(TaskStatus::WaitingUser)
            }
            _ => None,
        },
        _ => None,
    }?;
    investigation::can_transition(current, candidate).then_some(candidate)
}

/// Persist the durable run/step ledger independently of the UI event stream. Ordinary chat
/// runs have no row and are ignored; investigation runs are updated even when the window is
/// disconnected, so a later task detail request can explain what happened.
fn persist_investigation_event(app: &AppHandle, event_type: &str, params: &Value) {
    let state = app.state::<AppState>();
    let Some(run) = persist_investigation_event_state(&state, event_type, params) else {
        return;
    };
    if !matches!(
        run.status,
        InvestigationRunStatus::Completed
            | InvestigationRunStatus::Failed
            | InvestigationRunStatus::Cancelled
    ) {
        return;
    }

    let schedule = match state
        .database
        .investigations()
        .get_schedule_for_run(&run.id)
    {
        Ok(schedule) => Some(schedule),
        Err(yukinal_database::DatabaseError::NotFound) => None,
        Err(error) => {
            eprintln!(
                "[yukinal] scheduled comparison lookup failed for {}: {error}",
                run.id
            );
            None
        }
    };
    let comparison = if schedule.is_some() && run.status == InvestigationRunStatus::Completed {
        match state
            .database
            .investigations()
            .compare_schedule_run(&run.id)
        {
            Ok(comparison) => Some(comparison),
            Err(error) => {
                eprintln!(
                    "[yukinal] scheduled evidence comparison failed for {}: {error}",
                    run.id
                );
                None
            }
        }
    } else {
        None
    };
    let change_material = if let Some(comparison) = comparison
        .as_ref()
        .filter(|value| value.status == InvestigationScheduleComparisonStatus::Changed)
    {
        persist_schedule_change_material(&state, &run, comparison, &run.updated_at)
    } else {
        None
    };
    let schedule_status = match run.status {
        InvestigationRunStatus::Completed => {
            yukinal_database::models::InvestigationScheduleRunStatus::Succeeded
        }
        InvestigationRunStatus::Failed => {
            yukinal_database::models::InvestigationScheduleRunStatus::Failed
        }
        InvestigationRunStatus::Cancelled => {
            yukinal_database::models::InvestigationScheduleRunStatus::Interrupted
        }
        _ => unreachable!(),
    };
    let outcome = match (run.status, comparison.as_ref()) {
        (InvestigationRunStatus::Completed, Some(comparison)) => Some(comparison.status.as_str()),
        (InvestigationRunStatus::Completed, None) => Some("completed"),
        (InvestigationRunStatus::Failed, _) => Some("failed"),
        (InvestigationRunStatus::Cancelled, _) => Some("cancelled"),
        _ => None,
    };
    let error = run.failure.as_ref().map(|failure| failure.message.as_str());
    // Ordinary user runs have no schedule row; the repository returns NotFound/
    // validation and we intentionally leave those rows alone.
    let _ = state.database.investigations().finish_schedule_run(
        &run.id,
        schedule_status,
        outcome,
        error,
        &run.updated_at,
    );
    if let Some(schedule) = schedule.as_ref() {
        let outcome = outcome.unwrap_or("completed");
        if schedule_notification_allowed(
            schedule.notification_policy,
            run.status,
            comparison.as_ref().map(|value| value.status),
        ) {
            let (evidence_ids, finding_id, brief_id, title) = change_material
                .as_ref()
                .map(|material| {
                    (
                        material.evidence_ids.clone(),
                        Some(material.finding_id.clone()),
                        Some(material.brief_id.clone()),
                        material.title.clone(),
                    )
                })
                .unwrap_or_else(|| {
                    (
                        Vec::new(),
                        None,
                        None,
                        if run.status == InvestigationRunStatus::Failed {
                            "持续巡检运行失败".to_string()
                        } else {
                            "持续巡检结果".to_string()
                        },
                    )
                });
            let _ = app.emit(
                &tauri_event_name("investigation.schedule_notification"),
                json!({
                    "scheduleId": schedule.id,
                    "scheduleRunId": run.id,
                    "taskId": run.task_id,
                    "outcome": outcome,
                    "notificationPolicy": schedule.notification_policy.as_str(),
                    "title": title,
                    "evidenceIds": evidence_ids,
                    "findingId": finding_id,
                    "briefId": brief_id,
                    "at": run.updated_at,
                }),
            );
        }
    }
}

/// Persist one sidecar event using only the host state. Keeping the run/step
/// transition here makes the same logic executable in a real forwarder and a
/// SQLite cross-layer fixture without constructing a Tauri window.
fn persist_investigation_event_state(
    state: &AppState,
    event_type: &str,
    params: &Value,
) -> Option<yukinal_database::models::InvestigationRun> {
    let run_id = params.get("runId").and_then(Value::as_str)?;
    let mut run = match state.database.investigations().get_run(run_id) {
        Ok(run) => run,
        Err(yukinal_database::DatabaseError::NotFound) => return None,
        Err(error) => {
            eprintln!("[yukinal] investigation run lookup failed for {run_id}: {error}");
            return None;
        }
    };
    // Recovery and terminal finalisation are host decisions. Once one of
    // those states is durable, a late sidecar frame is audit noise, not a new
    // fact; in particular it must never resurrect an interrupted run.
    if is_terminal_investigation_run(run.status) {
        return None;
    }
    if let Some(event_task_id) = params.get("taskId").and_then(Value::as_str) {
        if event_task_id != run.task_id {
            return None;
        }
    }
    let now = yukinal_core::sidecar::iso8601_now();
    if let Some(trace_id) = params.get("traceId").and_then(Value::as_str).or_else(|| {
        params
            .get("result")
            .and_then(|value| value.get("traceId"))
            .and_then(Value::as_str)
    }) {
        run.trace_id = Some(bounded_audit_text(trace_id, 256));
    }

    match event_type {
        "agent.started" => run.status = InvestigationRunStatus::Running,
        "agent.waiting_approval" => run.status = InvestigationRunStatus::WaitingUser,
        "agent.tool_call" => {
            if let Some(step) = investigation_step_from_tool_call(&run, params, state) {
                if let Err(error) = state.database.investigations().upsert_step(&step) {
                    eprintln!("[yukinal] investigation step insert failed: {error}");
                }
            }
            run.status = InvestigationRunStatus::Running;
        }
        "agent.tool_result" => {
            if let Some(step) = investigation_step_from_tool_result(&run, params, state) {
                if let Err(error) = state.database.investigations().upsert_step(&step) {
                    eprintln!("[yukinal] investigation step update failed: {error}");
                }
            }
        }
        "agent.completed" => {
            let result_state = params
                .get("result")
                .and_then(|result| result.get("state"))
                .and_then(Value::as_str);
            run.status = match result_state {
                Some("cancelled") => InvestigationRunStatus::Cancelled,
                Some("failed") => InvestigationRunStatus::Failed,
                _ => InvestigationRunStatus::Completed,
            };
            if matches!(run.status, InvestigationRunStatus::Failed) {
                run.failure = failure_from_event(params, run.attempt);
            } else if matches!(run.status, InvestigationRunStatus::Cancelled) {
                run.failure = Some(InvestigationFailure {
                    code: TaskFailureCode::Cancelled,
                    message: "运行被用户停止".into(),
                    retryable: true,
                    attempt: run.attempt,
                    at: params
                        .get("at")
                        .and_then(Value::as_str)
                        .map(|value| bounded_audit_text(value, 80))
                        .unwrap_or_else(yukinal_core::sidecar::iso8601_now),
                    detail: None,
                    options: Some(failure_options(TaskFailureCode::Cancelled, true)),
                });
            }
        }
        "agent.failed" => {
            run.status = InvestigationRunStatus::Failed;
            run.failure = failure_from_event(params, run.attempt);
        }
        _ => return None,
    }

    run.updated_at = now.clone();
    if matches!(
        run.status,
        InvestigationRunStatus::Completed
            | InvestigationRunStatus::Failed
            | InvestigationRunStatus::Cancelled
            | InvestigationRunStatus::Interrupted
    ) {
        run.ended_at = Some(now.clone());
    }
    run.checkpoint = Some(json!({
        "lastEvent": event_type,
        "at": now,
        "stepId": params.get("stepId").and_then(Value::as_str),
    }));
    if let Err(error) = state.database.investigations().update_run(&run) {
        eprintln!("[yukinal] investigation run update failed for {run_id}: {error}");
        return None;
    }

    // Close the task's active-run fence before emitting any schedule notification.
    // The UI may immediately select the notification's decision brief; leaving the
    // completed run active until after `app.emit` creates a race where the explicit
    // continuation is rejected as "already active".
    if matches!(
        run.status,
        InvestigationRunStatus::Completed
            | InvestigationRunStatus::Failed
            | InvestigationRunStatus::Cancelled
    ) {
        finalize_investigation_task_if_active(state, &run);
    }
    Some(run)
}

fn finalize_investigation_task_if_active(
    state: &AppState,
    run: &yukinal_database::models::InvestigationRun,
) {
    let Ok(task) = state.database.investigations().get_task(&run.task_id) else {
        return;
    };
    if task.active_run_id.as_deref() != Some(run.id.as_str()) {
        // A retry may have superseded this run. Its own ledger is still updated above,
        // but a late terminal frame must never rewind the task to an older outcome.
        return;
    }
    let phase = task_phase_for_status(task.status);
    let failure = run.failure.as_ref();
    if let Err(error) = state
        .database
        .investigations()
        .update_task_progress_if_active(
            &TaskProgressUpdate {
                id: &task.id,
                status: task.status,
                phase,
                active_run_id: None,
                last_failure: failure,
                updated_at: &run.updated_at,
                completed_at: task.completed_at.as_deref(),
            },
            &run.id,
        )
        .map(|_| ())
    {
        eprintln!(
            "[yukinal] investigation task finalisation failed for {}: {error}",
            task.id
        );
    }
}

fn is_terminal_investigation_run(status: InvestigationRunStatus) -> bool {
    matches!(
        status,
        InvestigationRunStatus::Completed
            | InvestigationRunStatus::Failed
            | InvestigationRunStatus::Cancelled
            | InvestigationRunStatus::Interrupted
    )
}

fn is_current_investigation_run(active_run_id: Option<&str>, event_run_id: &str) -> bool {
    active_run_id == Some(event_run_id)
}

#[derive(Debug, Clone)]
struct ScheduleChangeMaterial {
    finding_id: String,
    brief_id: String,
    evidence_ids: Vec<String>,
    title: String,
}

fn persist_schedule_change_material(
    state: &AppState,
    run: &yukinal_database::models::InvestigationRun,
    comparison: &yukinal_database::models::InvestigationScheduleComparison,
    now: &str,
) -> Option<ScheduleChangeMaterial> {
    let finding_id = format!("finding_schedule_{}", run.id);
    let brief_id = format!("brief_schedule_{}", run.id);
    let artifact_id = format!("artifact_schedule_{}", run.id);
    let title = "持续巡检发现数据变化".to_string();
    let statement = "本次只读巡检的证据指纹与上一轮成功样本不同；这只证明观测数据发生变化，不足以单独证明根因或授权变更。";
    let finding = Finding {
        id: finding_id.clone(),
        task_id: run.task_id.clone(),
        title: title.clone(),
        kind: FindingKind::Inference,
        statement: statement.into(),
        evidence_ids: comparison.current_evidence_ids.clone(),
        confidence: FindingConfidence::Low,
        next_verification: Some(
            "在相同范围内继续一次只读复核，并将变化与上一轮证据逐项对照".into(),
        ),
        created_at: now.into(),
    };
    match state.database.investigations().get_finding(&finding_id) {
        Ok(_) => {}
        Err(yukinal_database::DatabaseError::NotFound) => {
            if let Err(error) = state.database.investigations().add_finding(&finding) {
                eprintln!("[yukinal] scheduled finding persistence failed: {error}");
                return None;
            }
        }
        Err(error) => {
            eprintln!("[yukinal] scheduled finding lookup failed: {error}");
            return None;
        }
    }

    let brief = DecisionBrief {
        id: brief_id.clone(),
        task_id: run.task_id.clone(),
        plan_id: state
            .database
            .investigations()
            .latest_plan(&run.task_id)
            .ok()
            .flatten()
            .map(|plan| plan.id),
        generated_at: now.into(),
        status: DecisionBriefStatus::Presented,
        finding_ids: vec![finding_id.clone()],
        options: vec![
            DecisionOption {
                id: format!("option_schedule_inspect_{}", run.id),
                title: "继续有限只读复核".into(),
                summary: "保持原范围和只读策略，再收集一轮证据确认变化是否持续。".into(),
                impact: "不改变目标主机状态；会消耗本任务的只读预算。".into(),
                risk_level: RiskLevel::Read,
                evidence_ids: comparison.current_evidence_ids.clone(),
                finding_ids: vec![finding_id.clone()],
                preview: Some("只调用已保存任务允许的只读工具".into()),
                verification: "下一轮样本应能说明变化是否仍然存在".into(),
                rollback: None,
                requires_approval: false,
                status: DecisionOptionStatus::Available,
                continuation: Some(DecisionOptionContinuation::ContinueReadonly),
            },
            DecisionOption {
                id: format!("option_schedule_plan_{}", run.id),
                title: "等待人工决定是否规划变更".into(),
                summary: "暂不执行任何写入；如果只读复核确认问题，再由用户要求 Agent 生成新的 dry-run 方案。".into(),
                impact: "维持现状，避免把观测差异误当成变更授权。".into(),
                risk_level: RiskLevel::Low,
                evidence_ids: comparison.current_evidence_ids.clone(),
                finding_ids: vec![finding_id.clone()],
                preview: Some("仅生成方案，不直接执行远端写操作".into()),
                verification: "用户明确要求后，新的方案必须重新通过宿主审批".into(),
                rollback: Some("没有写入动作，因此当前没有回退操作".into()),
                requires_approval: true,
                status: DecisionOptionStatus::Available,
                continuation: Some(DecisionOptionContinuation::WaitUser),
            },
        ],
        selected_option_id: None,
    };
    let already_saved = state
        .database
        .investigations()
        .latest_decision_brief(&run.task_id)
        .ok()
        .flatten()
        .is_some_and(|saved| saved.id == brief_id);
    if !already_saved {
        if let Err(error) = state.database.investigations().save_decision_brief(&brief) {
            eprintln!("[yukinal] scheduled decision brief persistence failed: {error}");
            return None;
        }
    }

    let artifact = yukinal_database::models::InvestigationArtifact {
        id: artifact_id,
        task_id: run.task_id.clone(),
        run_id: Some(run.id.clone()),
        plan_id: brief.plan_id.clone(),
        plan_step_id: None,
        phase: yukinal_database::models::TaskPhase::Decision,
        kind: TaskArtifactKind::EvidenceSet,
        status: TaskArtifactStatus::Ready,
        title: title.clone(),
        summary: statement.into(),
        content: json!({
            "comparison": comparison,
            "source": "host.scheduler",
            "decisionBriefId": brief_id,
        }),
        evidence_ids: comparison.current_evidence_ids.clone(),
        created_at: now.into(),
        updated_at: now.into(),
    };
    if let Err(error) = state.database.investigations().upsert_artifact(&artifact) {
        eprintln!("[yukinal] scheduled evidence artifact persistence failed: {error}");
        return None;
    }
    Some(ScheduleChangeMaterial {
        finding_id,
        brief_id,
        evidence_ids: comparison.current_evidence_ids.clone(),
        title,
    })
}

fn schedule_notification_allowed(
    policy: yukinal_database::models::InvestigationNotificationPolicy,
    run_status: InvestigationRunStatus,
    comparison: Option<InvestigationScheduleComparisonStatus>,
) -> bool {
    match policy {
        yukinal_database::models::InvestigationNotificationPolicy::Silent => false,
        yukinal_database::models::InvestigationNotificationPolicy::Always => true,
        yukinal_database::models::InvestigationNotificationPolicy::FailedRunsOnly => {
            matches!(
                run_status,
                InvestigationRunStatus::Failed | InvestigationRunStatus::Cancelled
            )
        }
        yukinal_database::models::InvestigationNotificationPolicy::OnChange => {
            comparison == Some(InvestigationScheduleComparisonStatus::Changed)
                || matches!(
                    run_status,
                    InvestigationRunStatus::Failed | InvestigationRunStatus::Cancelled
                )
        }
    }
}

fn investigation_step_from_tool_call(
    run: &yukinal_database::models::InvestigationRun,
    params: &Value,
    state: &AppState,
) -> Option<InvestigationStep> {
    let id = params.get("stepId").and_then(Value::as_str)?.trim();
    let title = params.get("toolName").and_then(Value::as_str)?.trim();
    if id.is_empty() || title.is_empty() {
        return None;
    }
    let ordinal = state
        .database
        .investigations()
        .list_steps(&run.task_id, 512)
        .ok()
        .and_then(|steps| steps.iter().map(|step| step.ordinal).max())
        .unwrap_or(0)
        .saturating_add(1);
    let target = params
        .get("target")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok());
    let input_summary = params.get("input").map(|value| {
        safe_audit_summary(
            &serde_json::to_string(&sanitize_audit_input(value.clone())).unwrap_or_default(),
            4_096,
        )
    });
    let kind = if params.get("riskLevel").and_then(Value::as_str) == Some("read") {
        InvestigationStepKind::Evidence
    } else {
        InvestigationStepKind::Action
    };
    Some(InvestigationStep {
        id: bounded_audit_text(id, 256),
        task_id: run.task_id.clone(),
        run_id: run.id.clone(),
        ordinal,
        kind,
        title: bounded_audit_text(title, 512),
        status: InvestigationStepStatus::Running,
        attempt: 1,
        tool_name: Some(bounded_audit_text(title, 256)),
        plan_id: params
            .get("planId")
            .and_then(Value::as_str)
            .map(|value| bounded_audit_text(value, 256)),
        plan_step_id: params
            .get("planStepId")
            .and_then(Value::as_str)
            .map(|value| bounded_audit_text(value, 256)),
        target,
        input_summary,
        output_summary: None,
        evidence_ids: plan_evidence_ids_from_event(params).unwrap_or_default(),
        started_at: Some(
            params
                .get("at")
                .and_then(Value::as_str)
                .map(|value| bounded_audit_text(value, 80))
                .unwrap_or_else(yukinal_core::sidecar::iso8601_now),
        ),
        ended_at: None,
        failure: None,
    })
}

fn investigation_step_from_tool_result(
    run: &yukinal_database::models::InvestigationRun,
    params: &Value,
    state: &AppState,
) -> Option<InvestigationStep> {
    let id = params.get("stepId").and_then(Value::as_str)?.trim();
    if id.is_empty() {
        return None;
    }
    let mut step = state
        .database
        .investigations()
        .list_steps(&run.task_id, 512)
        .ok()
        .and_then(|steps| steps.into_iter().find(|candidate| candidate.id == id))
        .unwrap_or(InvestigationStep {
            id: bounded_audit_text(id, 256),
            task_id: run.task_id.clone(),
            run_id: run.id.clone(),
            ordinal: 0,
            kind: InvestigationStepKind::Evidence,
            title: bounded_audit_text(
                params
                    .get("toolName")
                    .and_then(Value::as_str)
                    .unwrap_or("tool"),
                512,
            ),
            status: InvestigationStepStatus::Pending,
            attempt: 1,
            tool_name: params
                .get("toolName")
                .and_then(Value::as_str)
                .map(|value| bounded_audit_text(value, 256)),
            plan_id: params
                .get("planId")
                .and_then(Value::as_str)
                .map(|value| bounded_audit_text(value, 256)),
            plan_step_id: params
                .get("planStepId")
                .and_then(Value::as_str)
                .map(|value| bounded_audit_text(value, 256)),
            target: params
                .get("target")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok()),
            input_summary: None,
            output_summary: None,
            evidence_ids: plan_evidence_ids_from_event(params).unwrap_or_default(),
            started_at: None,
            ended_at: None,
            failure: None,
        });
    let status = params
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("failed");
    step.status = match status {
        "success" => InvestigationStepStatus::Succeeded,
        "cancelled" => InvestigationStepStatus::Skipped,
        _ => InvestigationStepStatus::Failed,
    };
    step.output_summary = params
        .get("outputSummary")
        .and_then(Value::as_str)
        .map(|value| safe_audit_summary(value, 4_096));
    if let Some(evidence_ids) = plan_evidence_ids_from_event(params) {
        step.evidence_ids = evidence_ids;
    }
    if let Some(plan_id) = params
        .get("planId")
        .and_then(Value::as_str)
        .map(|value| bounded_audit_text(value, 256))
    {
        step.plan_id = Some(plan_id);
    }
    if let Some(plan_step_id) = params
        .get("planStepId")
        .and_then(Value::as_str)
        .map(|value| bounded_audit_text(value, 256))
    {
        step.plan_step_id = Some(plan_step_id);
    }
    step.ended_at = params
        .get("endedAt")
        .and_then(Value::as_str)
        .map(|value| bounded_audit_text(value, 80));
    if matches!(
        step.status,
        InvestigationStepStatus::Failed | InvestigationStepStatus::Skipped
    ) {
        step.failure = failure_from_event(params, step.attempt);
    }
    Some(step)
}

fn plan_evidence_ids_from_event(params: &Value) -> Option<Vec<String>> {
    let raw = params.get("evidenceIds")?.as_array()?;
    Some(
        raw.iter()
            .filter_map(Value::as_str)
            .map(|value| bounded_audit_text(value, 256))
            .filter(|value| !value.is_empty())
            .take(256)
            .collect(),
    )
}

fn failure_from_event(params: &Value, attempt: u32) -> Option<InvestigationFailure> {
    let message = params
        .get("error")
        .and_then(Value::as_str)
        .or_else(|| {
            params
                .get("result")
                .and_then(|result| result.get("error"))
                .and_then(Value::as_str)
        })
        .or_else(|| params.get("outputSummary").and_then(Value::as_str))?
        .trim();
    let message = bounded_audit_text(message, 4_096);
    let code = params
        .get("errorCode")
        .and_then(Value::as_str)
        .and_then(task_failure_code_from_tool_error)
        .unwrap_or_else(|| classify_task_failure(&message));
    Some(InvestigationFailure {
        code,
        message,
        retryable: matches!(
            code,
            TaskFailureCode::Timeout | TaskFailureCode::Transport | TaskFailureCode::Authentication
        ),
        attempt,
        at: params
            .get("at")
            .and_then(Value::as_str)
            .or_else(|| params.get("endedAt").and_then(Value::as_str))
            .map(|value| bounded_audit_text(value, 80))
            .unwrap_or_else(yukinal_core::sidecar::iso8601_now),
        detail: None,
        options: Some(failure_options(
            code,
            matches!(
                code,
                TaskFailureCode::Timeout
                    | TaskFailureCode::Transport
                    | TaskFailureCode::Authentication
            ),
        )),
    })
}

pub(crate) fn failure_options(
    code: TaskFailureCode,
    retryable: bool,
) -> Vec<InvestigationFailureOption> {
    let mut options = Vec::new();
    if retryable {
        options.push(InvestigationFailureOption {
            id: "retry".into(),
            action: FailureOptionAction::Retry,
            title: "重试当前阶段".into(),
            description: "重新校验目标后再尝试一次；不会自动重放不可安全重试的写入。".into(),
            requires_approval: false,
        });
    }
    match code {
        TaskFailureCode::PlanDeviation | TaskFailureCode::EvidenceMissing => {
            options.push(InvestigationFailureOption {
                id: "replan".into(),
                action: FailureOptionAction::Replan,
                title: "要求 Agent 重新规划".into(),
                description: "保留现有证据与失败现场，要求生成新的计划修订。".into(),
                requires_approval: false,
            });
        }
        TaskFailureCode::ApprovalRequired
        | TaskFailureCode::ApprovalRejected
        | TaskFailureCode::PermissionDenied => {
            options.push(InvestigationFailureOption {
                id: "wait_user".into(),
                action: FailureOptionAction::WaitUser,
                title: "等待我重新决定".into(),
                description: "不自动重试；由用户补充批准、缩小范围或终止任务。".into(),
                requires_approval: true,
            });
        }
        TaskFailureCode::Cancelled => {
            options.push(InvestigationFailureOption {
                id: "resume".into(),
                action: FailureOptionAction::Resume,
                title: "从检查点恢复".into(),
                description: "重新检查连接、计划和目标后从未完成阶段继续。".into(),
                requires_approval: false,
            });
        }
        TaskFailureCode::BudgetExhausted => {
            options.push(InvestigationFailureOption {
                id: "replan".into(),
                action: FailureOptionAction::Replan,
                title: "缩小范围后重新规划".into(),
                description: "预算已耗尽，必须由用户确认新的范围或预算。".into(),
                requires_approval: true,
            });
        }
        _ => {}
    }
    options.push(InvestigationFailureOption {
        id: "inspect".into(),
        action: FailureOptionAction::Inspect,
        title: "查看现场证据".into(),
        description: "保持任务停止，先查看已保存的证据、执行结果和失败工件。".into(),
        requires_approval: false,
    });
    options.push(InvestigationFailureOption {
        id: "stop".into(),
        action: FailureOptionAction::Stop,
        title: "结束任务".into(),
        description: "保留审计与现场证据，不再继续自动化。".into(),
        requires_approval: false,
    });
    options
}

fn task_failure_code_from_tool_error(value: &str) -> Option<TaskFailureCode> {
    Some(match value {
        "invalid_input" => TaskFailureCode::InvalidInput,
        "plan_deviation" => TaskFailureCode::PlanDeviation,
        "denied_by_policy" | "permission_denied" => TaskFailureCode::PermissionDenied,
        "approval_rejected" => TaskFailureCode::ApprovalRejected,
        "approval_timeout" => TaskFailureCode::ApprovalRequired,
        "timeout" => TaskFailureCode::Timeout,
        "cancelled" => TaskFailureCode::Cancelled,
        "not_found" => TaskFailureCode::TargetNotFound,
        "transport" => TaskFailureCode::Transport,
        "unsupported" => TaskFailureCode::Unsupported,
        "execution_failed" => TaskFailureCode::CommandFailed,
        "internal" => TaskFailureCode::Internal,
        _ => return None,
    })
}

fn classify_task_failure(message: &str) -> TaskFailureCode {
    let value = message.to_ascii_lowercase();
    if value.contains("maxsteps") || value.contains("budget") {
        TaskFailureCode::BudgetExhausted
    } else if value.contains("timeout") || value.contains("timed out") {
        TaskFailureCode::Timeout
    } else if value.contains("cancel") || value.contains("stopped") {
        TaskFailureCode::Cancelled
    } else if value.contains("approval") {
        TaskFailureCode::ApprovalRejected
    } else if value.contains("auth") || value.contains("credential") {
        TaskFailureCode::Authentication
    } else if value.contains("transport") || value.contains("connect") || value.contains("network")
    {
        TaskFailureCode::Transport
    } else if value.contains("not found") || value.contains("不存在") {
        TaskFailureCode::TargetNotFound
    } else if value.contains("permission") || value.contains("policy") || value.contains("denied") {
        TaskFailureCode::PermissionDenied
    } else if value.contains("invalid") {
        TaskFailureCode::InvalidInput
    } else if value.contains("unsupported") {
        TaskFailureCode::Unsupported
    } else if value.contains("truncat") {
        TaskFailureCode::OutputTruncated
    } else if value.contains("evidence") {
        TaskFailureCode::EvidenceMissing
    } else if value.contains("revision") || value.contains("stale") {
        TaskFailureCode::StaleTarget
    } else if value.contains("command") || value.contains("exit code") {
        TaskFailureCode::CommandFailed
    } else {
        TaskFailureCode::Internal
    }
}

fn task_phase_for_status(status: TaskStatus) -> TaskPhase {
    match status {
        TaskStatus::Pending | TaskStatus::Investigating => TaskPhase::Investigating,
        TaskStatus::WaitingUser => TaskPhase::Decision,
        TaskStatus::Executing => TaskPhase::Execution,
        TaskStatus::Verifying => TaskPhase::Verification,
        TaskStatus::Completed => TaskPhase::Completed,
        TaskStatus::Failed | TaskStatus::Stopped | TaskStatus::Expired => TaskPhase::Recovery,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        is_current_investigation_run, is_stable_server_id, is_terminal_investigation_run,
        is_terminal_result_status, next_investigation_status, persist_investigation_event_state,
        safe_audit_summary, sanitize_audit_input, sync_investigation_task_status_state,
        task_failure_code_from_tool_error, tauri_event_name,
    };
    use crate::state::AppState;
    use serde_json::json;
    use yukinal_database::models::{
        Environment, InvestigationPermissionMode, InvestigationRun, InvestigationRunMode,
        InvestigationRunStatus, InvestigationTarget, InvestigationTargetHost, InvestigationTask,
        TaskAutomationLevel, TaskBudget, TaskFailureCode, TaskPhase, TaskStatus,
        ToolExecutionStatus,
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
        let stopped_run =
            persist_investigation_event_state(&state, "agent.completed", &stopped_event)
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
        assert!(
            persist_investigation_event_state(&state, "agent.completed", &late_event).is_none()
        );
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
        let current_run =
            persist_investigation_event_state(&state, "agent.completed", &current_event)
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
        assert!(
            persist_investigation_event_state(&state, "agent.failed", &current_event).is_none()
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
}

#[cfg(test)]
mod fixture_contracts {
    use serde::de::DeserializeOwned;

    use super::*;

    fn assert_fixture<T: DeserializeOwned + Serialize>(name: &str, raw: &str) {
        let expected: serde_json::Value = serde_json::from_str(raw)
            .unwrap_or_else(|error| panic!("{name} is not valid JSON: {error}"));
        let typed = serde_json::from_value::<T>(expected.clone()).unwrap_or_else(|error| {
            panic!("{name} no longer matches its Rust response type: {error}")
        });
        let actual = serde_json::to_value(typed).expect("response type must serialize");
        assert_eq!(
            actual, expected,
            "{name} drops or renames a field at the Rust boundary"
        );
    }

    #[test]
    fn every_remaining_ipc_fixture_deserializes_into_its_response_type() {
        assert_fixture::<agent_run::RunStopResponse>(
            "agent_run_stop",
            include_str!("../../../../../packages/shared/fixtures/ipc/agent_run_stop.json"),
        );
        assert_fixture::<agent_run::ApprovalRespondResponse>(
            "agent_approval_respond",
            include_str!("../../../../../packages/shared/fixtures/ipc/agent_approval_respond.json"),
        );

        assert_fixture::<provider::ProviderListResponse>(
            "provider_list",
            include_str!("../../../../../packages/shared/fixtures/ipc/provider_list.json"),
        );
        for (name, raw) in [
            (
                "provider_save",
                include_str!("../../../../../packages/shared/fixtures/ipc/provider_save.json"),
            ),
            (
                "provider_save_anthropic",
                include_str!(
                    "../../../../../packages/shared/fixtures/ipc/provider_save_anthropic.json"
                ),
            ),
            (
                "provider_save_gemini",
                include_str!(
                    "../../../../../packages/shared/fixtures/ipc/provider_save_gemini.json"
                ),
            ),
        ] {
            assert_fixture::<provider::ProviderSaveResponse>(name, raw);
        }
        assert_fixture::<provider::ProviderActivateResponse>(
            "provider_activate",
            include_str!("../../../../../packages/shared/fixtures/ipc/provider_activate.json"),
        );
        assert_fixture::<provider::ProviderDeleteResponse>(
            "provider_delete",
            include_str!("../../../../../packages/shared/fixtures/ipc/provider_delete.json"),
        );
        assert_fixture::<provider::ProviderModelsResponse>(
            "provider_models",
            include_str!("../../../../../packages/shared/fixtures/ipc/provider_models.json"),
        );
        assert_fixture::<provider::ProviderTestResponse>(
            "provider_test",
            include_str!("../../../../../packages/shared/fixtures/ipc/provider_test.json"),
        );

        assert_fixture::<server::ServerListResponse>(
            "server_list",
            include_str!("../../../../../packages/shared/fixtures/ipc/server_list.json"),
        );
        assert_fixture::<server::ServerAddResponse>(
            "server_add",
            include_str!("../../../../../packages/shared/fixtures/ipc/server_add.json"),
        );
        assert_fixture::<server::ServerAddResponse>(
            "server_update",
            include_str!("../../../../../packages/shared/fixtures/ipc/server_update.json"),
        );
        assert_fixture::<server::ServerConnectResponse>(
            "server_connect",
            include_str!("../../../../../packages/shared/fixtures/ipc/server_connect.json"),
        );
        assert_fixture::<server::ServerDeleteResponse>(
            "server_delete",
            include_str!("../../../../../packages/shared/fixtures/ipc/server_delete.json"),
        );
        assert_fixture::<server::ServerAuthResponse>(
            "server_auth_respond",
            include_str!("../../../../../packages/shared/fixtures/ipc/server_auth_respond.json"),
        );
        assert_fixture::<server::ServerAuthResponse>(
            "server_auth_cancel",
            include_str!("../../../../../packages/shared/fixtures/ipc/server_auth_cancel.json"),
        );
        assert_fixture::<server::ServerSnapshotResponse>(
            "server_snapshot",
            include_str!("../../../../../packages/shared/fixtures/ipc/server_snapshot.json"),
        );
        assert_fixture::<EmptyResponse>(
            "server_disconnect",
            include_str!("../../../../../packages/shared/fixtures/ipc/server_disconnect.json"),
        );

        assert_fixture::<terminal::TerminalOpenResponse>(
            "terminal_open",
            include_str!("../../../../../packages/shared/fixtures/ipc/terminal_open.json"),
        );
        for (name, raw) in [
            (
                "terminal_write",
                include_str!("../../../../../packages/shared/fixtures/ipc/terminal_write.json"),
            ),
            (
                "terminal_resize",
                include_str!("../../../../../packages/shared/fixtures/ipc/terminal_resize.json"),
            ),
            (
                "terminal_close",
                include_str!("../../../../../packages/shared/fixtures/ipc/terminal_close.json"),
            ),
        ] {
            assert_fixture::<EmptyResponse>(name, raw);
        }

        assert_fixture::<files::RemoteFileListResponse>(
            "remote_file_list",
            include_str!("../../../../../packages/shared/fixtures/ipc/remote_file_list.json"),
        );
        assert_fixture::<files::RemoteFileReadResponse>(
            "remote_file_read",
            include_str!("../../../../../packages/shared/fixtures/ipc/remote_file_read.json"),
        );

        assert_fixture::<mcp::McpServerListResponse>(
            "mcp_server_list",
            include_str!("../../../../../packages/shared/fixtures/ipc/mcp_server_list.json"),
        );
        assert_fixture::<mcp::McpServerView>(
            "mcp_server_save",
            include_str!("../../../../../packages/shared/fixtures/ipc/mcp_server_save.json"),
        );
        assert_fixture::<mcp::McpServerView>(
            "mcp_server_start",
            include_str!("../../../../../packages/shared/fixtures/ipc/mcp_server_start.json"),
        );
        assert_fixture::<mcp::McpServerView>(
            "mcp_server_review",
            include_str!("../../../../../packages/shared/fixtures/ipc/mcp_server_review.json"),
        );
        assert_fixture::<mcp::McpOAuthConnectResult>(
            "mcp_oauth_connect",
            include_str!("../../../../../packages/shared/fixtures/ipc/mcp_oauth_connect.json"),
        );
        assert_fixture::<mcp::McpServerDeleteResponse>(
            "mcp_server_delete",
            include_str!("../../../../../packages/shared/fixtures/ipc/mcp_server_delete.json"),
        );
        assert_fixture::<mcp::McpServerStopResponse>(
            "mcp_server_stop",
            include_str!("../../../../../packages/shared/fixtures/ipc/mcp_server_stop.json"),
        );
        // 网络设置（ADR 0022）：两个命令的响应是同一个类型，fixture 覆盖「经代理」与
        // 「直连」两种解析结果。
        assert_fixture::<network::NetworkProxyView>(
            "network_proxy_get",
            include_str!("../../../../../packages/shared/fixtures/ipc/network_proxy_get.json"),
        );
        assert_fixture::<network::NetworkProxyView>(
            "network_proxy_save",
            include_str!("../../../../../packages/shared/fixtures/ipc/network_proxy_save.json"),
        );
        assert_fixture::<investigation::InvestigationBriefResponse>(
            "investigation_brief_select",
            include_str!(
                "../../../../../packages/shared/fixtures/ipc/investigation_brief_select.json"
            ),
        );
        assert_fixture::<investigation::InvestigationTaskStartResponse>(
            "investigation_task_start",
            include_str!(
                "../../../../../packages/shared/fixtures/ipc/investigation_task_start.json"
            ),
        );
        assert_fixture::<investigation::InvestigationTaskResponse>(
            "investigation_task_stop",
            include_str!(
                "../../../../../packages/shared/fixtures/ipc/investigation_task_stop.json"
            ),
        );
    }
}
