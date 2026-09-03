//! Server commands: the overview's live data comes from a real collection run over
//! the server's SSH session — no cached rows, no pretending.

use serde::Serialize;
use tauri::State;

use crate::commands::terminal::ensure_session;
use crate::state::AppState;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerSnapshotResponse {
    pub snapshot: yukinal_database::models::ServerSnapshot,
}

/// `server_snapshot`: connect（如未连）→ detect + collect（MVP 7 采集器）→
/// 组装成 `snapshots` 行并入库 → 返回。失败诚实上抛（服务器不可达 / 认证失败 /
/// 采集器整组失败都会以错误结束，不会返回假数据）。
#[tauri::command]
pub async fn server_snapshot(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<ServerSnapshotResponse, String> {
    ensure_session(&state, &server_id).await?;

    let session = state
        .terminals
        .cached_session(&server_id)
        .map_err(|error| error.to_string())?;
    let collected_at = yukinal_core::sidecar::iso8601_now();

    let (snapshot, _samples) =
        yukinal_core::collector::collect_snapshot(&state.ssh, &session, &server_id, &collected_at)
            .await
            .map_err(|error| error.to_string())?;

    // 先入库（audit / 趋势都靠 snapshots 行），再返回给 UI。
    state
        .database
        .snapshots()
        .insert(&snapshot)
        .map_err(|error| error.to_string())?;

    Ok(ServerSnapshotResponse { snapshot })
}
