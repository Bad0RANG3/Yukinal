//! Sidecar lifecycle and Agent event forwarding.
//!
//! This module owns the native process seam. It does not define the shared IPC contract;
//! it adapts the supervisor's typed events to the Tauri window and delegates host requests.

use super::*;

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
