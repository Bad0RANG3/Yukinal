//! The complete native surface available to React (-R9-R10).
//!
//! Every command mirrors a key of `IpcCommandMap` in `@yukinal/shared`; field naming is
//! camelCase on both sides. If a command is not in that map, it does not exist for the
//! UI and must not be added here.

use tauri::{AppHandle, Emitter, Manager, State};

use crate::state::AppState;
use yukinal_core::ipc::{AgentKillResponse, AgentLogsResponse, AgentSpawnResponse, PingResponse};
use yukinal_core::sidecar::{SidecarConfig, SidecarEvent};
use yukinal_core::supervisor::{SupervisorStatus, LOG_HISTORY};

pub mod activity;
pub mod agent_run;
pub mod files;
pub mod provider;
pub mod server;
pub mod terminal;

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

/// `agent.stream` 通知 → Tauri event（事件名 = AgentStreamEvent.type）。
fn forward_agent_frame(app: &AppHandle, frame: &serde_json::Value) {
    let Some(method) = frame.get("method").and_then(serde_json::Value::as_str) else {
        return;
    };
    if method != "agent.stream" {
        return;
    }
    let Some(params) = frame.get("params") else {
        return;
    };
    let Some(event_type) = params.get("type").and_then(serde_json::Value::as_str) else {
        return;
    };
    let _ = app.emit(event_type, params.clone());
}
