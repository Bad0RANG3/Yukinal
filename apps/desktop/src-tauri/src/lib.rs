//! Tauri entry: registers commands and owns process state.
//!
//! Deliberately thin. No SSH logic, no permission logic, no collector logic lives in
//! this crate — those belong in `crates/*` so they stay testable without a window.

mod commands;
mod state;

use std::path::PathBuf;

use tauri::{Emitter, Manager};

use state::AppState;
use yukinal_terminal::TerminalAppEvent;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // 数据目录：SQLite、known_hosts、终端服务都挂在这里（全部由 Rust 侧装配）。
            // Keep native state and the sidecar's YUKINAL_DATA_DIR aligned for
            // portable/dev runs; otherwise the host can silently use another DB.
            let data_dir = configured_data_dir(app)?;
            let app_state = AppState::bootstrap(&data_dir)?;
            app.manage(app_state);

            forward_terminal_events(app.handle().clone());
            // Once per window, before anything can start the agent: the forwarder has to
            // outlive an agent crash so the restarted process is still reported, and it must
            // never be duplicated (two forwarders execute every `host.*` request twice).
            commands::forward_sidecar_events(app.handle().clone());

            // The Agent is a core interaction service, so it starts with the window
            // instead of being hidden behind a status control in the rail. The same
            // `start_sidecar` path remains used by tests and future recovery actions.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match commands::start_sidecar(&handle).await {
                    Ok(spawned) => eprintln!(
                        "[yukinal] autostart ok pid={} protocol={} tools={}",
                        spawned.pid, spawned.protocol_version, spawned.tool_count
                    ),
                    Err(error) => {
                        eprintln!("[yukinal] autostart failed: {error}");
                        // 失败的原因几乎总在 sidecar 死之前写下的 stderr 里，而事件泵只转发
                        // 「它订阅之后」的行：启动期（含整个握手）的那些行只进了保留尾巴，
                        // 而泵在建立时打的那一次尾巴还是空的 —— 它的注释说「agent 启动时说了
                        // 什么永远不会看不见」，那对「启动之后才订阅」这种情况才成立。
                        // 所以这里必须再打一次，否则一次握手中的崩溃在日志里只剩下
                        // 「agent sidecar exited」这几个字，看日志的人无从下手。
                        let supervisor = handle.state::<AppState>().supervisor.clone();
                        for line in supervisor.logs().await {
                            eprintln!("[agent:last] {line}");
                        }
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::core_ping,
            commands::agent_spawn,
            commands::agent_status,
            commands::agent_kill,
            commands::agent_logs,
            commands::agent_run::agent_run_start,
            commands::agent_run::agent_run_stop,
            commands::agent_run::agent_approval_respond,
            commands::chat::chat_session_list,
            commands::chat::chat_session_get,
            commands::chat::chat_session_create,
            commands::chat::chat_message_append,
            commands::chat::chat_session_archive,
            commands::chat::chat_session_rename,
            commands::chat::chat_session_delete,
            commands::server::server_list,
            commands::workspace::workspace_list,
            commands::server::server_add,
            commands::server::server_update,
            commands::server::server_delete,
            commands::server::server_connect,
            commands::server::server_disconnect,
            commands::host_key::server_host_key_status,
            commands::host_key::server_host_key_probe,
            commands::host_key::server_host_key_trust,
            commands::host_key::server_host_key_forget,
            commands::files::remote_file_list,
            commands::files::remote_file_read,
            commands::provider::provider_list,
            commands::provider::provider_save,
            commands::provider::provider_activate,
            commands::provider::provider_delete,
            commands::provider::provider_models,
            commands::provider::provider_test,
            commands::mcp::mcp_server_list,
            commands::mcp::mcp_server_save,
            commands::mcp::mcp_server_delete,
            commands::mcp::mcp_server_start,
            commands::mcp::mcp_server_stop,
            commands::server::server_snapshot,
            commands::services::server_services,
            commands::logs::server_logs,
            commands::activity::activity_list,
            commands::execution::tool_execution_list,
            commands::terminal::terminal_open,
            commands::terminal::terminal_write,
            commands::terminal::terminal_resize,
            commands::terminal::terminal_close
        ])
        .build(tauri::generate_context!())
        .expect("failed to start Yukinal")
        .run(|app_handle, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                // Killing the sidecar here is the "no orphan process" guarantee
                //; `kill_on_drop` is the backstop if we never get here.
                let state = app_handle.state::<AppState>();
                let supervisor = state.supervisor.clone();
                // MCP servers are third-party programs we started, so they get the same
                // treatment as our own sidecar. `kill_on_drop` alone is not enough: it is
                // a `Drop` impl, and a force-kill on Windows never runs one — the children
                // would outlive the window that owns them, which is precisely the outcome
                // the MCP boundary refuses to accept.
                let mcp = state.mcp.clone();
                tauri::async_runtime::block_on(async move {
                    let _ = supervisor.stop().await;
                    // Servers are independent of each other, so one failing to die must
                    // not keep the rest alive. `shutdown_all` reports per-server outcomes
                    // instead of failing as a whole.
                    let _ = mcp.shutdown_all().await;
                });
            }
        });
}

fn configured_data_dir(app: &tauri::App) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(path) = std::env::var_os("YUKINAL_DATA_DIR").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    Ok(app.path().app_data_dir()?)
}

/// PTY Manager 事件 → Tauri events，UI 只认这几个名字（`@yukinal/shared` 里有契）。
fn forward_terminal_events(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut receiver = app.state::<AppState>().terminals.subscribe();
        loop {
            match receiver.recv().await {
                Ok(TerminalAppEvent::Data {
                    terminal_session_id,
                    data,
                }) => {
                    let _ = app.emit(
                        &commands::tauri_event_name("terminal.data"),
                        serde_json::json!({
                            "terminalSessionId": terminal_session_id,
                            "data": data,
                        }),
                    );
                }
                Ok(TerminalAppEvent::Opened { payload }) => {
                    let _ = app.emit(
                        &commands::tauri_event_name("terminal.opened"),
                        serde_json::to_value(payload).unwrap_or_default(),
                    );
                }
                Ok(TerminalAppEvent::Closed {
                    terminal_session_id,
                    exit_code,
                }) => {
                    let _ = app.emit(
                        &commands::tauri_event_name("terminal.closed"),
                        serde_json::json!({
                            "terminalSessionId": terminal_session_id,
                            "exitCode": exit_code,
                        }),
                    );
                }
                Err(_) => break,
            }
        }
    });
}
