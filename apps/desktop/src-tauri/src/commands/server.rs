//! Server commands: list/add（持久化到 SQLite，凭据进 OS keychain）与 overview 的
//! 实时快照（真实采集，不做假数据）。

use serde::Serialize;
use tauri::State;

use crate::commands::terminal::ensure_session;
use crate::state::AppState;
use yukinal_credentials::{CredentialStore, Secret};
use yukinal_database::models::{
    Identity, Server, ServerCapabilities, ServerConnection, ServerMetadata, ServerStatus,
};
use yukinal_database::{AddServerInput, AuthenticationInput};

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

    // 身份：secret 进 OS keychain，SQLite 只存 credentialRef。
    let identity_id = store_identity(&state, &input, &id, &now).await?;

    let server = Server {
        id: id.clone(),
        name: input.name.clone(),
        connection: ServerConnection {
            host: input.host.clone(),
            port: input.port.unwrap_or(22),
            username: input.username.clone(),
            identity_id: Some(identity_id),
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
    state
        .database
        .servers()
        .insert(&server)
        .map_err(|error| error.to_string())?;

    Ok(ServerAddResponse { server })
}

async fn store_identity(
    state: &State<'_, AppState>,
    input: &AddServerInput,
    server_id: &str,
    now: &str,
) -> Result<String, String> {
    let (method, credential_ref) = match &input.authentication {
        yukinal_database::AuthenticationInput::Password { password } => {
            let reference = state
                .credentials
                .set("ssh", server_id, &Secret::from_utf8(password.clone()))
                .map_err(|error| error.to_string())?;
            ("password", reference.to_string_ref())
        }
        yukinal_database::AuthenticationInput::PrivateKey {
            private_key_pem,
            passphrase,
        } => {
            if passphrase.as_ref().is_some_and(|value| !value.is_empty()) {
                return Err("加密私钥暂未支持（SSH 后端显式拒绝）".into());
            }
            let reference = state
                .credentials
                .set(
                    "ssh",
                    server_id,
                    &Secret::from_utf8(private_key_pem.clone()),
                )
                .map_err(|error| error.to_string())?;
            ("privateKey", reference.to_string_ref())
        }
        AuthenticationInput::Identity { identity_id } => {
            // 引用已存在的身份：不改凭据，直接挂上。
            return Ok(identity_id.clone());
        }
    };

    let identity = Identity {
        id: next_id("idn"),
        label: format!("{} ({server_id})", input.name),
        method: method.to_string(),
        credential_ref,
        created_at: now.to_string(),
    };
    state
        .database
        .identities()
        .insert(&identity)
        .map_err(|error| error.to_string())?;
    state
        .database
        .identities()
        .attach_to_server(server_id, &identity.id)
        .map_err(|error| error.to_string())?;
    Ok(identity.id)
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
