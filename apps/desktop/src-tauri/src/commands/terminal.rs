//! Terminal IPC commands: the UI only sees `terminal_session_id`s.
//!
//! Wire: resolve server (SQLite) → identity (SQLite) → credential (OS keychain) →
//! ssh connect (cached per server) → PTY → TerminalManager. React never holds an
//! ssh `Session`.

use serde::Serialize;
use tauri::State;

use crate::state::AppState;
use yukinal_credentials::{CredentialRef, CredentialStore, Secret};
use yukinal_database::models::Server;
use yukinal_ssh::{Authentication, ConnectionSecrets, KnownHostsPolicy, SshBackend, SshConfig};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOpenResponse {
    pub terminal_session_id: String,
}

/// 确保 `server_id` 有一条已认证的 ssh 连接（缓存命中 → 直接复用）。
pub(crate) async fn ensure_session(state: &AppState, server_id: &str) -> Result<(), String> {
    if state.terminals.cached_session(server_id).is_ok() {
        return Ok(());
    }

    let server = state
        .database
        .servers()
        .get(server_id)
        .map_err(|error| error.to_string())?;
    let (config, secrets) = resolve_capabilities(state, &server)?;
    let session = match state.ssh.connect(config, secrets).await {
        Ok(session) => session,
        Err(error) => {
            let _ = state.database.servers().set_status(
                server_id,
                yukinal_database::models::ServerStatus::Error,
                &yukinal_core::sidecar::iso8601_now(),
            );
            return Err(error.to_string());
        }
    };
    state.terminals.cache_session(server_id, session);
    let _ = state.database.servers().set_status(
        server_id,
        yukinal_database::models::ServerStatus::Connected,
        &yukinal_core::sidecar::iso8601_now(),
    );
    Ok(())
}

/// Server/identity/credential → `SshConfig` + 已解析 `ConnectionSecrets`。
/// 凭据引用在 SQLite，材料在 OS keychain，解析点在这里（使用点）。
fn resolve_capabilities(
    state: &AppState,
    server: &Server,
) -> Result<(SshConfig, ConnectionSecrets), String> {
    let identity_id = server
        .connection
        .identity_id
        .as_deref()
        .ok_or_else(|| format!("server `{}` has no identity configured", server.id))?;
    let identity = state
        .database
        .identities()
        .get(identity_id)
        .map_err(|error| error.to_string())?;

    let reference =
        CredentialRef::parse(&identity.credential_ref).map_err(|error| error.to_string())?;
    let secret = state
        .credentials
        .get(&reference)
        .map_err(|error| error.to_string())?;

    let (authentication, mut secrets) = match identity.method.as_str() {
        "password" => (
            Authentication::Password {
                credential_ref: identity.credential_ref.clone(),
            },
            ConnectionSecrets {
                password: Some(secret_to_string(&secret)?),
                private_key_pem: None,
                private_key_passphrase: None,
            },
        ),
        "privateKey" => (
            Authentication::PrivateKey {
                credential_ref: identity.credential_ref.clone(),
                passphrase_ref: None,
            },
            ConnectionSecrets {
                password: None,
                private_key_pem: Some(secret_to_string(&secret)?),
                private_key_passphrase: None,
            },
        ),
        other => return Err(format!("unsupported identity method `{other}`")),
    };
    secrets.private_key_passphrase = None;

    let config = SshConfig {
        server_id: server.id.clone(),
        host: server.connection.host.clone(),
        port: server.connection.port,
        username: server.connection.username.clone(),
        authentication,
        // MVP：终端首连自动信任并记录；host key 之后的严格匹配由 known_hosts 保证。
        known_hosts_policy: KnownHostsPolicy::TrustOnFirstUse,
        keepalive_interval_secs: 30,
    };
    Ok((config, secrets))
}

fn secret_to_string(secret: &Secret) -> Result<String, String> {
    secret
        .as_utf8()
        .map(|value| value.into_owned())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn terminal_open(
    state: State<'_, AppState>,
    server_id: String,
    cols: u16,
    rows: u16,
) -> Result<TerminalOpenResponse, String> {
    ensure_session(&state, &server_id).await?;
    let terminal_session_id = state
        .terminals
        .open(&server_id, cols, rows)
        .await
        .map_err(|error| error.to_string())?;
    Ok(TerminalOpenResponse {
        terminal_session_id,
    })
}

#[tauri::command]
pub async fn terminal_write(
    state: State<'_, AppState>,
    terminal_session_id: String,
    data: String,
) -> Result<(), String> {
    state
        .terminals
        .write(&terminal_session_id, data.as_bytes())
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn terminal_resize(
    state: State<'_, AppState>,
    terminal_session_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    state
        .terminals
        .resize(&terminal_session_id, cols, rows)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn terminal_close(
    state: State<'_, AppState>,
    terminal_session_id: String,
) -> Result<(), String> {
    state
        .terminals
        .close(&terminal_session_id)
        .await
        .map_err(|error| error.to_string())
}
