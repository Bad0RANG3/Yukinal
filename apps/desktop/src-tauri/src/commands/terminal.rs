//! Terminal IPC commands: the UI only sees `terminal_session_id`s.
//!
//! Wire: resolve server and identity → read credentials at use time → C++ PTY child.
//! Keyboard-interactive prompts travel through the existing UI challenge broker while
//! the C++ worker owns the SSH connection. React keeps the same `terminal_session_id`
//! contract and terminal events.
//!
//! This is intentionally a manual UI path, not an Agent tool. It has no task/plan ticket and
//! must not be called as an autonomous write primitive; the durable-plan gate lives in
//! `commands::host` for Agent-side effectful tools.

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use crate::commands::{ssh_service, EmptyResponse};
use crate::state::AppState;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOpenResponse {
    pub terminal_session_id: String,
}

#[tauri::command]
pub async fn terminal_open(
    app: AppHandle,
    state: State<'_, AppState>,
    server_id: String,
    cols: u16,
    rows: u16,
) -> Result<TerminalOpenResponse, String> {
    validate_terminal_dimensions(cols, rows)?;
    if !ssh_service::supports_cpp_target(&state, &server_id)? {
        let _ = state.database.servers().set_status(
            &server_id,
            yukinal_database::models::ServerStatus::Error,
            &yukinal_core::sidecar::iso8601_now(),
        );
        return Err("the selected target is not supported by the C++ SSH service".into());
    }
    let pty = match ssh_service::open_terminal(&app, &state, &server_id, cols, rows).await {
        Ok(pty) => pty,
        Err(ssh_service::TerminalOpenFailure::Failed(error)) => {
            let _ = state.database.servers().set_status(
                &server_id,
                yukinal_database::models::ServerStatus::Error,
                &yukinal_core::sidecar::iso8601_now(),
            );
            return Err(error);
        }
    };
    let terminal_session_id = state
        .cpp_terminals
        .open(&server_id, cols, rows, pty)
        .await
        .map_err(|error| error.to_string())?;
    let _ = state.database.servers().set_status(
        &server_id,
        yukinal_database::models::ServerStatus::Connected,
        &yukinal_core::sidecar::iso8601_now(),
    );
    Ok(TerminalOpenResponse {
        terminal_session_id,
    })
}

fn validate_terminal_dimensions(cols: u16, rows: u16) -> Result<(), String> {
    if !(1..=1000).contains(&cols) || !(1..=1000).contains(&rows) {
        return Err("terminal dimensions must be between 1 and 1000".into());
    }
    Ok(())
}

#[tauri::command]
pub async fn terminal_write(
    state: State<'_, AppState>,
    terminal_session_id: String,
    data: String,
) -> Result<EmptyResponse, String> {
    state
        .cpp_terminals
        .write(&terminal_session_id, data.as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    Ok(EmptyResponse {})
}

#[tauri::command]
pub async fn terminal_resize(
    state: State<'_, AppState>,
    terminal_session_id: String,
    cols: u16,
    rows: u16,
) -> Result<EmptyResponse, String> {
    validate_terminal_dimensions(cols, rows)?;
    state
        .cpp_terminals
        .resize(&terminal_session_id, cols, rows)
        .await
        .map_err(|error| error.to_string())?;
    Ok(EmptyResponse {})
}

#[tauri::command]
pub async fn terminal_close(
    state: State<'_, AppState>,
    terminal_session_id: String,
) -> Result<EmptyResponse, String> {
    state
        .cpp_terminals
        .close(&terminal_session_id)
        .await
        .map_err(|error| error.to_string())?;
    Ok(EmptyResponse {})
}
