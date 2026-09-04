//! Server commands: list/add（持久化到 SQLite，凭据进 OS keychain）与 overview 的
//! 实时快照（真实采集，不做假数据）。

use serde::Serialize;
use tauri::State;

use crate::commands::terminal::ensure_session;
use crate::state::AppState;
use yukinal_credentials::{CredentialStore, Secret};
use yukinal_database::models::{
    Activity, ActivityOutcome, ActivitySource, ActivityType, Identity, Server, ServerCapabilities,
    ServerConnection, ServerMetadata, ServerStatus,
};
use yukinal_database::UpdateServerInput;
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
            record_activity(
                &state,
                &server_id,
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
            let _ = record_activity(
                &state,
                &server_id,
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
) -> Result<(), String> {
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
    record_activity(
        &state,
        &server_id,
        ActivityType::Connection,
        "已断开服务器",
        None,
        ActivityOutcome::Success,
    )
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

    // A changed endpoint or credential must not leave the old authenticated
    // connection cached under the same stable server id.
    state
        .terminals
        .disconnect(&input.server_id)
        .await
        .map_err(|error| error.to_string())?;

    let new_identity_id = match input.authentication {
        Some(authentication) => Some(
            store_identity_input(
                &state,
                &authentication,
                &input.name,
                &input.server_id,
                &yukinal_core::sidecar::iso8601_now(),
            )
            .await?,
        ),
        None => old_identity_id.clone(),
    };
    server.name = input.name;
    server.connection.host = input.host;
    server.connection.port = input.port.unwrap_or(22);
    server.connection.username = input.username;
    server.connection.identity_id = new_identity_id;
    server.group_id = input.group_id;
    server.metadata.environment = input.environment;
    server.status = ServerStatus::Disconnected;
    server.updated_at = yukinal_core::sidecar::iso8601_now();
    state
        .database
        .servers()
        .update(&server)
        .map_err(|error| error.to_string())?;

    if let Some(old_id) =
        old_identity_id.filter(|id| Some(id) != server.connection.identity_id.as_ref())
    {
        reclaim_identity(&state, &old_id, &server.id)?;
    }
    record_activity(
        &state,
        &server.id,
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
    record_activity(
        &state,
        &server_id,
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

    // 身份：secret 进 OS keychain，SQLite 只存 credentialRef。
    let identity_id = store_identity(&state, &input, &id, &now).await?;

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

    record_activity(
        &state,
        &id,
        ActivityType::Configuration,
        "已添加服务器",
        None,
        ActivityOutcome::Success,
    )?;

    Ok(ServerAddResponse { server })
}

fn record_activity(
    state: &State<'_, AppState>,
    server_id: &str,
    activity_type: ActivityType,
    title: &str,
    description: Option<String>,
    outcome: ActivityOutcome,
) -> Result<(), String> {
    state
        .database
        .activities()
        .insert(&Activity {
            id: next_id("act"),
            server_id: Some(server_id.to_string()),
            workspace_id: None,
            r#type: activity_type,
            title: title.to_string(),
            description,
            source: ActivitySource::User,
            actor: "user".to_string(),
            reason: Some("用户在工作区执行操作".to_string()),
            outcome: Some(outcome),
            trace_id: None,
            created_at: yukinal_core::sidecar::iso8601_now(),
        })
        .map_err(|error| error.to_string())
}

fn insert_server_and_attach_identity(
    database: &yukinal_database::Database,
    server: &Server,
    identity_id: &str,
) -> Result<(), String> {
    database
        .servers()
        .insert(server)
        .map_err(|error| error.to_string())?;
    database
        .identities()
        .attach_to_server(&server.id, identity_id)
        .map_err(|error| error.to_string())
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
    Ok(identity.id)
}

async fn store_identity_input(
    state: &State<'_, AppState>,
    authentication: &AuthenticationInput,
    label: &str,
    server_id: &str,
    now: &str,
) -> Result<String, String> {
    match authentication {
        AuthenticationInput::Identity { identity_id } => {
            state
                .database
                .identities()
                .get(identity_id)
                .map_err(|error| error.to_string())?;
            Ok(identity_id.clone())
        }
        AuthenticationInput::Password { password } => {
            let reference = state
                .credentials
                .set(
                    "ssh",
                    &format!("{server_id}-{}", next_id("cred")),
                    &Secret::from_utf8(password.clone()),
                )
                .map_err(|error| error.to_string())?;
            let identity = Identity {
                id: next_id("idn"),
                label: format!("{} ({server_id})", label),
                method: "password".into(),
                credential_ref: reference.to_string_ref(),
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
        AuthenticationInput::PrivateKey {
            private_key_pem,
            passphrase,
        } => {
            if passphrase.as_ref().is_some_and(|value| !value.is_empty()) {
                return Err("鍔犲瘑绉侀挜鏆傛湭鏀寔".into());
            }
            let reference = state
                .credentials
                .set(
                    "ssh",
                    &format!("{server_id}-{}", next_id("cred")),
                    &Secret::from_utf8(private_key_pem.clone()),
                )
                .map_err(|error| error.to_string())?;
            let identity = Identity {
                id: next_id("idn"),
                label: format!("{} ({server_id})", label),
                method: "privateKey".into(),
                credential_ref: reference.to_string_ref(),
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
    }
}

fn reclaim_identity(
    state: &State<'_, AppState>,
    identity_id: &str,
    server_id: &str,
) -> Result<(), String> {
    if state
        .database
        .identities()
        .attached_to_other_server(identity_id, server_id)
        .map_err(|error| error.to_string())?
    {
        return Ok(());
    }
    if let Ok(identity) = state.database.identities().get(identity_id) {
        if let Ok(reference) = yukinal_credentials::CredentialRef::parse(&identity.credential_ref) {
            state
                .credentials
                .delete(&reference)
                .map_err(|error| error.to_string())?;
        }
        let _ = state.database.identities().delete(identity_id);
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::insert_server_and_attach_identity;
    use yukinal_database::models::{
        Environment, Identity, Server, ServerCapabilities, ServerConnection, ServerMetadata,
        ServerStatus,
    };

    #[test]
    fn server_is_inserted_before_identity_attachment() {
        let path = std::env::temp_dir().join(format!(
            "yukinal-server-add-order-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
        let database = yukinal_database::Database::open(&path).expect("open database");
        database
            .identities()
            .insert(&Identity {
                id: "idn_order".into(),
                label: "test identity".into(),
                method: "password".into(),
                credential_ref: "keychain://ssh/test".into(),
                created_at: "2026-01-01T00:00:00.000Z".into(),
            })
            .expect("insert identity");
        let server = Server {
            id: "srv_order".into(),
            name: "Order test".into(),
            connection: ServerConnection {
                host: "127.0.0.1".into(),
                port: 22,
                username: "test".into(),
                identity_id: Some("idn_order".into()),
            },
            group_id: None,
            capabilities: ServerCapabilities::default(),
            status: ServerStatus::Disconnected,
            metadata: ServerMetadata {
                environment: Environment::Development,
                region: None,
                hostname: None,
                os: None,
                tags: None,
                workspace_ids: None,
            },
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
        };

        insert_server_and_attach_identity(&database, &server, "idn_order")
            .expect("server and identity association");
        assert_eq!(
            database
                .identities()
                .ids_for_server("srv_order")
                .expect("ids"),
            vec!["idn_order".to_string()]
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }
}
