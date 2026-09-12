//! Terminal IPC commands: the UI only sees `terminal_session_id`s.
//!
//! Wire: resolve server (SQLite) → identity (SQLite) → credential (OS keychain) →
//! ssh connect (cached per server) → PTY → TerminalManager. React never holds an
//! ssh `Session`.

use serde::Serialize;
use tauri::State;

use crate::commands::EmptyResponse;
use crate::state::AppState;
use yukinal_credentials::{CredentialRef, CredentialStore, Secret};
use yukinal_database::models::{Identity, Server};
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

    let (authentication, secrets) =
        resolve_identity_authentication(&identity, state.credentials.as_ref())?;

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

/// 身份行 + keychain →（`Authentication`、解析好的 `ConnectionSecrets`）。
///
/// 从 `resolve_capabilities` 里抽出来只为一个理由：**可测**。`AppState` 绑着 Tauri
/// runtime、`RusshBackend` 和真实的 `OsCredentialStore`，单元测试里造不出来；这个
/// 函数只吃「身份行 + 一个凭据存储」，于是「agent 身份产出 `Authentication::Agent`」
/// 「口令引用被解析成 `private_key_passphrase`」这些真正会连错服务器的分支，可以用
/// `MemoryCredentialStore` 直接驱动。凭据仍然只在使用点解析 —— 这一层没有把任何
/// 材料交给 IPC。
fn resolve_identity_authentication(
    identity: &Identity,
    credentials: &dyn CredentialStore,
) -> Result<(Authentication, ConnectionSecrets), String> {
    let read = |reference: &str| -> Result<String, String> {
        let parsed = CredentialRef::parse(reference).map_err(|error| error.to_string())?;
        let secret = credentials
            .get(&parsed)
            .map_err(|error| error.to_string())?;
        secret_to_string(&secret)
    };

    match identity.method.as_str() {
        "password" => Ok((
            Authentication::Password {
                credential_ref: identity.credential_ref.clone(),
            },
            ConnectionSecrets {
                password: Some(read(&identity.credential_ref)?),
                private_key_pem: None,
                private_key_passphrase: None,
            },
        )),
        "privateKey" => {
            // 口令按引用取，取不到就**报错**：静默降级成「没有口令」会把一个
            // keychain 故障变成认证期的 PassphraseRequired，根因更难查。
            let passphrase = match identity.passphrase_ref.as_deref() {
                Some(reference) => Some(read(reference)?),
                None => None,
            };
            Ok((
                Authentication::PrivateKey {
                    credential_ref: identity.credential_ref.clone(),
                    // 引用随身份给出：它是「这把 key 该配哪个口令条目」的记录，
                    // 不是口令材料本身。
                    passphrase_ref: identity.passphrase_ref.clone(),
                },
                ConnectionSecrets {
                    password: None,
                    private_key_pem: Some(read(&identity.credential_ref)?),
                    private_key_passphrase: passphrase,
                },
            ))
        }
        // ssh-agent：身份在 agent 手里，Yukinal 一个 secret 都没有。
        // `socket_path: None` = 按平台约定发现（Unix 的 `SSH_AUTH_SOCK`；Windows 的
        // OpenSSH 命名管道，其次 Pageant）。这是**唯一**不做任何回退的认证方式：
        // agent 不可用就报错，绝不退到密码或别的 key。
        "agent" => Ok((
            Authentication::Agent { socket_path: None },
            ConnectionSecrets::empty(),
        )),
        // 未知 method 保持响亮失败，不做静默默认 —— 数据库被手改过、或更新版本写了
        // 新 method 时，静默挑一个认证方式等于用错凭据去连服务器。
        other => Err(format!("unsupported identity method `{other}`")),
    }
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
) -> Result<EmptyResponse, String> {
    state
        .terminals
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
    state
        .terminals
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
        .terminals
        .close(&terminal_session_id)
        .await
        .map_err(|error| error.to_string())?;
    Ok(EmptyResponse {})
}

#[cfg(test)]
mod tests {
    use super::{resolve_capabilities, resolve_identity_authentication};
    use crate::state::AppState;
    use yukinal_credentials::memory::MemoryCredentialStore;
    use yukinal_credentials::{CredentialStore, Secret};
    use yukinal_database::models::{
        Environment, Identity, Server, ServerCapabilities, ServerConnection, ServerMetadata,
        ServerStatus,
    };
    use yukinal_ssh::Authentication;

    const NOW: &str = "2026-01-01T00:00:00.000Z";

    fn identity(method: &str, credential_ref: &str, passphrase_ref: Option<&str>) -> Identity {
        Identity {
            id: "idn_wire".into(),
            label: "wire".into(),
            method: method.into(),
            credential_ref: credential_ref.into(),
            passphrase_ref: passphrase_ref.map(str::to_string),
            created_at: NOW.into(),
        }
    }

    /// 这是整条链上最容易连错服务器的分支：agent 身份必须产出
    /// `Authentication::Agent`，并且**一个 secret 都不带**（数据库里 agent 行没有
    /// 凭据条目，去解析 `credential_ref` 只会得到一个错误）。
    #[test]
    fn an_agent_identity_becomes_agent_authentication_with_no_secrets() {
        let store = MemoryCredentialStore::new();
        let (authentication, secrets) =
            resolve_identity_authentication(&identity("agent", "", None), &store)
                .expect("an agent identity resolves without any credential entry");

        assert_eq!(
            authentication,
            Authentication::Agent { socket_path: None },
            "socket_path: None = 按平台约定发现 agent"
        );
        assert_eq!(secrets.password, None);
        assert_eq!(secrets.private_key_pem, None);
        assert_eq!(secrets.private_key_passphrase, None);
    }

    /// 加密私钥：`passphrase_ref` 要真的被解析成 `ConnectionSecrets`，引用本身也随
    /// `Authentication::PrivateKey` 传下去。旧代码在这条路径上把
    /// `private_key_passphrase` 强制清成 `None`，加密 key 因此永远连不上。
    #[test]
    fn an_encrypted_private_key_carries_its_passphrase_through() {
        let store = MemoryCredentialStore::new();
        let key_ref = store
            .set(
                "ssh",
                "srv_x",
                &Secret::from_utf8("-----BEGIN OPENSSH PRIVATE KEY-----"),
            )
            .expect("set key");
        let pass_ref = store
            .set("ssh", "srv_x-passphrase", &Secret::from_utf8("s3cret"))
            .expect("set passphrase");

        let (authentication, secrets) = resolve_identity_authentication(
            &identity(
                "privateKey",
                &key_ref.to_string_ref(),
                Some(&pass_ref.to_string_ref()),
            ),
            &store,
        )
        .expect("resolve");

        assert_eq!(
            authentication,
            Authentication::PrivateKey {
                credential_ref: "keychain://ssh/srv_x".into(),
                passphrase_ref: Some("keychain://ssh/srv_x-passphrase".into()),
            }
        );
        assert_eq!(
            secrets.private_key_pem.as_deref(),
            Some("-----BEGIN OPENSSH PRIVATE KEY-----")
        );
        assert_eq!(secrets.private_key_passphrase.as_deref(), Some("s3cret"));
        assert_eq!(secrets.password, None);
    }

    /// 明文 key：没有 `passphrase_ref`，`private_key_passphrase` 必须是 `None`
    /// （空口令与「不给口令」在 ssh 后端是同一件事，见 `load_private_key`）。
    #[test]
    fn a_plaintext_private_key_carries_no_passphrase() {
        let store = MemoryCredentialStore::new();
        let key_ref = store
            .set("ssh", "srv_plain", &Secret::from_utf8("key"))
            .expect("set key");

        let (authentication, secrets) = resolve_identity_authentication(
            &identity("privateKey", &key_ref.to_string_ref(), None),
            &store,
        )
        .expect("resolve");
        assert_eq!(
            authentication,
            Authentication::PrivateKey {
                credential_ref: key_ref.to_string_ref(),
                passphrase_ref: None,
            }
        );
        assert_eq!(secrets.private_key_passphrase, None);
    }

    /// 口令条目被删掉了（比如 keychain 被清理）：必须**报错**，不能静默当成
    /// 「这把 key 没有口令」—— 那样故障会在认证阶段以 PassphraseRequired 出现，
    /// 根因和现象隔了一层。
    #[test]
    fn a_dangling_passphrase_reference_fails_instead_of_degrading() {
        let store = MemoryCredentialStore::new();
        let key_ref = store
            .set("ssh", "srv_gone", &Secret::from_utf8("key"))
            .expect("set key");

        let error = resolve_identity_authentication(
            &identity(
                "privateKey",
                &key_ref.to_string_ref(),
                Some("keychain://ssh/srv_gone-passphrase"),
            ),
            &store,
        )
        .expect_err("a missing passphrase entry must not resolve");
        assert!(error.contains("srv_gone-passphrase"), "unexpected: {error}");
    }

    /// 未知 method 仍然响亮失败（`other =>` 分支不能被改成静默默认）。
    #[test]
    fn an_unknown_method_still_fails_loudly() {
        let store = MemoryCredentialStore::new();
        let error = resolve_identity_authentication(
            &identity("certificate", "keychain://ssh/x", None),
            &store,
        )
        .expect_err("an unknown method must not pick a default");
        assert_eq!(error, "unsupported identity method `certificate`");
    }

    /// 密码认证的老路径没有被这次改动碰坏。
    #[test]
    fn a_password_identity_still_resolves_its_password() {
        let store = MemoryCredentialStore::new();
        let password_ref = store
            .set("ssh", "srv_pw", &Secret::from_utf8("hunter2"))
            .expect("set password");

        let (authentication, secrets) = resolve_identity_authentication(
            &identity("password", &password_ref.to_string_ref(), None),
            &store,
        )
        .expect("resolve");
        assert_eq!(
            authentication,
            Authentication::Password {
                credential_ref: password_ref.to_string_ref(),
            }
        );
        assert_eq!(secrets.password.as_deref(), Some("hunter2"));
        assert_eq!(secrets.private_key_pem, None);
    }

    /// 全链用例：真 `AppState`（临时数据目录 + 真 SQLite）+ 数据库里的 agent 身份行 →
    /// `resolve_capabilities` 产出 `Authentication::Agent`。
    ///
    /// 能这样做是因为 agent 分支**不读任何 secret**：`AppState.credentials` 是具体的
    /// `OsCredentialStore`（不是 `dyn CredentialStore`），测试里换不成内存实现，而
    /// 真实 keychain 在 CI/无头环境里不可用 —— agent 恰好是唯一不需要它的认证方式。
    /// 其余分支（密码 / 私钥 / 口令）由上面那些用 `MemoryCredentialStore` 驱动
    /// `resolve_identity_authentication` 的用例覆盖。
    #[test]
    fn an_agent_identity_resolves_to_agent_authentication_through_the_app_state() {
        let dir = std::env::temp_dir().join(format!("yukinal-caps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let state = AppState::bootstrap(&dir).expect("bootstrap a real AppState");

        state
            .database
            .identities()
            .insert(&Identity {
                id: "idn_agent_wire".into(),
                label: "agent".into(),
                method: "agent".into(),
                // agent 身份没有凭据条目：这一列是 NOT NULL，空串就是「没有」。
                credential_ref: String::new(),
                passphrase_ref: None,
                created_at: NOW.into(),
            })
            .expect("insert the agent identity");
        state
            .database
            .servers()
            .insert(&Server {
                id: "srv_agent_wire".into(),
                name: "agent host".into(),
                connection: ServerConnection {
                    host: "127.0.0.1".into(),
                    port: 2222,
                    username: "deploy".into(),
                    identity_id: Some("idn_agent_wire".into()),
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
                created_at: NOW.into(),
                updated_at: NOW.into(),
            })
            .expect("insert the server");

        let server = state
            .database
            .servers()
            .get("srv_agent_wire")
            .expect("read the server back");
        let (config, secrets) =
            resolve_capabilities(&state, &server).expect("an agent server must resolve");

        assert_eq!(
            config.authentication,
            Authentication::Agent { socket_path: None },
            "socket_path: None = 按平台约定发现 agent（SSH_AUTH_SOCK / OpenSSH 管道）"
        );
        assert_eq!(config.server_id, "srv_agent_wire");
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.port, 2222);
        assert_eq!(config.username, "deploy");
        // 一个 secret 都不带 —— 连 keychain 都没碰。
        assert_eq!(secrets.password, None);
        assert_eq!(secrets.private_key_pem, None);
        assert_eq!(secrets.private_key_passphrase, None);

        drop(state);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
