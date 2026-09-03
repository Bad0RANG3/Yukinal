//! Tauri entry: registers commands and owns process state.
//!
//! Deliberately thin. No SSH logic, no permission logic, no collector logic lives in
//! this crate — those belong in `crates/*` so they stay testable without a window.

mod commands;
mod state;

use tauri::{Emitter, Manager};

use state::AppState;
use yukinal_terminal::TerminalAppEvent;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // 数据目录：SQLite、known_hosts、终端服务都挂在这里（全部由 Rust 侧装配）。
            let data_dir = app.path().app_data_dir()?;
            let app_state = AppState::bootstrap(&data_dir)?;
            app.manage(app_state);

            forward_terminal_events(app.handle().clone());

            // Dev/test affordance only (never set by a shipped app): starts the sidecar
            // through `agent_spawn`'s own code path so CI can assert the real chain.
            if std::env::var("YUKINAL_AUTOSTART_AGENT").is_ok() {
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
            }
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
            commands::server::server_list,
            commands::server::server_add,
            commands::provider::provider_list,
            commands::provider::provider_save_openai,
            commands::server::server_snapshot,
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
                        "terminal.data",
                        serde_json::json!({
                            "terminalSessionId": terminal_session_id,
                            "data": data,
                        }),
                    );
                }
                Ok(TerminalAppEvent::Opened { payload }) => {
                    let _ = app.emit(
                        "terminal.opened",
                        serde_json::to_value(payload).unwrap_or_default(),
                    );
                }
                Ok(TerminalAppEvent::Closed {
                    terminal_session_id,
                }) => {
                    let _ = app.emit(
                        "terminal.closed",
                        serde_json::json!({ "terminalSessionId": terminal_session_id }),
                    );
                }
                Err(_) => break,
            }
        }
    });
}
