//! Tauri entry: registers commands and owns process state.
//!
//! Deliberately thin. No SSH logic, no permission logic, no collector logic lives in
//! this crate — those belong in `crates/*` so they stay testable without a window.

mod commands;
mod state;

use tauri::Manager;

use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::new())
        .setup(|app| {
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
            commands::agent_logs
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
