//! Server commands: list/add（持久化到 SQLite，凭据进 OS keychain）与 overview 的
//! 实时快照（真实采集，不做假数据）。

use serde::Serialize;
use tauri::State;

use crate::commands::activity::record_user_activity;
use crate::commands::terminal::ensure_session;
use crate::commands::EmptyResponse;
use crate::state::AppState;
use yukinal_core::identity::{insert_server_and_attach_identity, IdentityWrite};
use yukinal_database::models::{
    ActivityOutcome, ActivityType, Server, ServerCapabilities, ServerConnection, ServerMetadata,
    ServerStatus,
};
use yukinal_database::UpdateServerInput;
use yukinal_database::{AddServerInput, AuthenticationInput};

/// 身份写入与回收的 keychain 侧（需要 `CredentialStore` 句柄，所以住不进 `crates/core`）。
mod identity;

use identity::{reclaim_identity, store_identity};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerListResponse {
    pub servers: Vec<Server>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerAddResponse {
    pub server: Server,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConnectResponse {
    pub status: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerDeleteResponse {
    pub deleted: bool,
}

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

/// `server_list`：从 SQLite 读出全部服务器（线形 = 契约）。
#[tauri::command]
pub async fn server_list(state: State<'_, AppState>) -> Result<ServerListResponse, String> {
    let servers = state
        .database
        .servers()
        .list()
        .map_err(|error| error.to_string())?;
    Ok(ServerListResponse { servers })
}

/// Establish and cache the SSH session used by terminal, snapshots and SFTP.
#[tauri::command]
pub async fn server_connect(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<ServerConnectResponse, String> {
    match ensure_session(&state, &server_id).await {
        Ok(()) => {
            state
                .database
                .servers()
                .set_status(
                    &server_id,
                    ServerStatus::Connected,
                    &yukinal_core::sidecar::iso8601_now(),
                )
                .map_err(|error| error.to_string())?;
            record_user_activity(
                &state,
                Some(&server_id),
                ActivityType::Connection,
                "已连接服务器",
                None,
                ActivityOutcome::Success,
            )?;
            Ok(ServerConnectResponse {
                status: "connected",
            })
        }
        Err(error) => {
            let _ = state.database.servers().set_status(
                &server_id,
                ServerStatus::Error,
                &yukinal_core::sidecar::iso8601_now(),
            );
            let _ = record_user_activity(
                &state,
                Some(&server_id),
                ActivityType::Connection,
                "连接服务器失败",
                Some(error.clone()),
                ActivityOutcome::Failure,
            );
            Err(error)
        }
    }
}

/// Close the cached session and every PTY attached to it. This operation is
/// idempotent so a stale UI can safely request disconnect twice.
#[tauri::command]
pub async fn server_disconnect(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<EmptyResponse, String> {
    state
        .terminals
        .disconnect(&server_id)
        .await
        .map_err(|error| error.to_string())?;
    state
        .database
        .servers()
        .set_status(
            &server_id,
            ServerStatus::Disconnected,
            &yukinal_core::sidecar::iso8601_now(),
        )
        .map_err(|error| error.to_string())?;
    record_user_activity(
        &state,
        Some(&server_id),
        ActivityType::Connection,
        "已断开服务器",
        None,
        ActivityOutcome::Success,
    )?;
    Ok(EmptyResponse {})
}

#[tauri::command]
pub async fn server_update(
    state: State<'_, AppState>,
    input: serde_json::Value,
) -> Result<ServerAddResponse, String> {
    let input = UpdateServerInput::from_value(&input)
        .map_err(|error| format!("invalid server-update input: {error}"))?;
    let mut server = state
        .database
        .servers()
        .get(&input.server_id)
        .map_err(|error| error.to_string())?;
    let old_identity_id = server.connection.identity_id.clone();

    // Validate and stage the replacement identity before disconnecting the old
    // session. A bad identity reference or keychain failure must not destroy a
    // connection that was still usable.
    let staged_identity_id = match input.authentication.as_ref() {
        Some(AuthenticationInput::Identity { .. }) | None => None,
        Some(authentication) => Some(
            store_identity(
                &state,
                authentication,
                &input.name,
                &input.server_id,
                IdentityWrite::Replace,
                &yukinal_core::sidecar::iso8601_now(),
            )
            .await?,
        ),
    };
    let new_identity_id = match input.authentication.as_ref() {
        Some(authentication) => Some(match authentication {
            AuthenticationInput::Identity { identity_id } => {
                state
                    .database
                    .identities()
                    .get(identity_id)
                    .map_err(|error| error.to_string())?;
                identity_id.clone()
            }
            _ => staged_identity_id
                .clone()
                .ok_or_else(|| "replacement identity was not staged".to_string())?,
        }),
        None => old_identity_id.clone(),
    };

    // A changed endpoint or credential must not leave the old authenticated
    // connection cached under the same stable server id.
    if let Err(error) = state.terminals.disconnect(&input.server_id).await {
        if let Some(identity_id) = staged_identity_id.as_deref() {
            let _ = reclaim_identity(&state, identity_id, &server.id);
        }
        return Err(error.to_string());
    }
    server.name = input.name;
    server.connection.host = input.host;
    server.connection.port = input.port.unwrap_or(22);
    server.connection.username = input.username;
    server.connection.identity_id = new_identity_id;
    server.group_id = input.group_id;
    server.metadata.environment = input.environment;
    server.status = ServerStatus::Disconnected;
    server.updated_at = yukinal_core::sidecar::iso8601_now();
    if let Err(error) = state.database.servers().update(&server) {
        if let Some(identity_id) = staged_identity_id.as_deref() {
            let _ = reclaim_identity(&state, identity_id, &server.id);
        }
        return Err(error.to_string());
    }

    if let Some(old_id) =
        old_identity_id.filter(|id| Some(id) != server.connection.identity_id.as_ref())
    {
        reclaim_identity(&state, &old_id, &server.id)?;
    }
    record_user_activity(
        &state,
        Some(&server.id),
        ActivityType::Configuration,
        "已更新服务器配置",
        None,
        ActivityOutcome::Success,
    )?;
    Ok(ServerAddResponse { server })
}

#[tauri::command]
pub async fn server_delete(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<ServerDeleteResponse, String> {
    let server = state
        .database
        .servers()
        .get(&server_id)
        .map_err(|error| error.to_string())?;
    state
        .terminals
        .disconnect(&server_id)
        .await
        .map_err(|error| error.to_string())?;
    state
        .database
        .servers()
        .delete(&server_id)
        .map_err(|error| error.to_string())?;
    if let Some(identity_id) = server.connection.identity_id {
        reclaim_identity(&state, &identity_id, &server_id)?;
    }
    record_user_activity(
        &state,
        Some(&server_id),
        ActivityType::Configuration,
        "已删除服务器",
        None,
        ActivityOutcome::Success,
    )?;
    Ok(ServerDeleteResponse { deleted: true })
}

/// `server_add`：表单输入 →（secret 进 keychain，SQLite 只存引用）→ 服务器行。
/// 返回已落库的服务器（含生成的稳定 `srv_` id）。
#[tauri::command]
pub async fn server_add(
    state: State<'_, AppState>,
    input: serde_json::Value,
) -> Result<ServerAddResponse, String> {
    let input = AddServerInput::from_value(&input)
        .map_err(|error| format!("invalid add-server input: {error}"))?;

    let now = yukinal_core::sidecar::iso8601_now();
    let id = next_id("srv");

    // 身份：secret 进 OS keychain，SQLite 只存 credentialRef / passphraseRef。
    let identity_id = store_identity(
        &state,
        &input.authentication,
        &input.name,
        &id,
        IdentityWrite::Add,
        &now,
    )
    .await?;

    let server = Server {
        id: id.clone(),
        name: input.name.clone(),
        connection: ServerConnection {
            host: input.host.clone(),
            port: input.port.unwrap_or(22),
            username: input.username.clone(),
            identity_id: Some(identity_id.clone()),
        },
        group_id: input.group_id.clone(),
        capabilities: ServerCapabilities::default(),
        status: ServerStatus::Disconnected,
        metadata: ServerMetadata {
            environment: input.environment,
            region: None,
            hostname: None,
            os: None,
            tags: None,
            workspace_ids: None,
        },
        created_at: now.clone(),
        updated_at: now,
    };
    insert_server_and_attach_identity(&state.database, &server, &identity_id)?;

    record_user_activity(
        &state,
        Some(&id),
        ActivityType::Configuration,
        "已添加服务器",
        None,
        ActivityOutcome::Success,
    )?;

    Ok(ServerAddResponse { server })
}

/// `srv_`/`idn_` 前缀 + 时间戳/millis + 进程内计数器：稳定、非 host 派生。
pub(crate) fn next_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|delta| delta.as_millis())
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}_{millis:x}{n:x}")
}
