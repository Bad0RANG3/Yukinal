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
                    Err(error) => eprintln!("[yukinal] autostart failed: {error}"),
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
            commands::chat::chat_session_delete,
            commands::server::server_list,
            commands::workspace::workspace_list,
            commands::server::server_add,
            commands::server::server_update,
            commands::server::server_delete,
            commands::server::server_connect,
            commands::server::server_disconnect,
            commands::files::remote_file_list,
            commands::files::remote_file_read,
            commands::provider::provider_list,
            commands::provider::provider_save_openai,
            commands::provider::provider_activate,
            commands::provider::provider_models,
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
                let supervisor = app_handle.state::<AppState>().supervisor.clone();
                tauri::async_runtime::block_on(async move {
                    let _ = supervisor.stop().await;
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
