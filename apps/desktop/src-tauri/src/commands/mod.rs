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
    Activity, ActivityOutcome, ActivitySource, ActivityType, Environment, PermissionMode,
    RiskLevel, ToolExecutionRecord, ToolExecutionStatus,
};

pub mod activity;
pub mod agent_run;
pub mod chat;
pub mod execution;
pub mod files;
pub mod host;
pub mod host_key;
pub mod logs;
pub mod mcp;
pub mod provider;
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
#[derive(Debug, Serialize)]
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
            match receiver.recv().await {
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
    if event_type == "agent.tool_result" {
        persist_agent_tool_result(app, params);
    }
    let _ = app.emit(&tauri_event_name(event_type), params.clone());
}

#[cfg(test)]
mod tests {
    use super::{
        is_stable_server_id, is_terminal_result_status, safe_audit_summary, sanitize_audit_input,
        tauri_event_name,
    };
    use serde_json::json;
    use yukinal_database::models::ToolExecutionStatus;

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
