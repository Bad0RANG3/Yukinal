//! Server commands: list/add（持久化到 SQLite，凭据进 OS keychain）与 overview 的
//! 实时快照（真实采集，不做假数据）。

use serde::Serialize;
use tauri::State;

use crate::commands::activity::record_user_activity;
use crate::commands::terminal::ensure_session;
use crate::commands::EmptyResponse;
use crate::state::AppState;
use yukinal_credentials::{CredentialRef, CredentialStore, Secret};
use yukinal_database::models::{
    ActivityOutcome, ActivityType, Identity, Server, ServerCapabilities, ServerConnection,
    ServerMetadata, ServerStatus,
};
use yukinal_database::UpdateServerInput;
use yukinal_database::{AddServerInput, AuthenticationInput, Database};

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

fn insert_server_and_attach_identity(
    database: &yukinal_database::Database,
    server: &Server,
    identity_id: &str,
) -> Result<(), String> {
    database
        .servers()
        .insert(server)
        .map_err(|error| error.to_string())?;
    if let Err(error) = database
        .identities()
        .attach_to_server(&server.id, identity_id)
    {
        // Do not leave a server row behind when the second half of the add
        // operation fails (for example, a stale identity reference).
        let _ = database.servers().delete(&server.id);
        return Err(error.to_string());
    }
    Ok(())
}

/// 口令条目在 keychain 里的 account 后缀。
///
/// 私钥与口令是同一个 `ssh` service 下的**两条**条目：`{account}` 与
/// `{account}-passphrase`。分开存而不是拼成一个 blob，有两个理由：
///
/// 1. 一个条目解析失败只丢它自己 —— 拼成一个 blob 的话，口令部分坏掉就等于把私钥
///    一起丢了，而两者是完全独立的材料；
/// 2. 私钥条目的 account 与升级前完全一致，旧版本代码读的是同一个条目，不会因为
///    新版本写过一次就找不到 key。
const PASSPHRASE_ACCOUNT_SUFFIX: &str = "-passphrase";

/// 空 / 纯空白口令 = 「没有口令」。
///
/// 与 `crates/ssh` 的规则一致（`load_private_key` 把空口令过滤成「没给口令」）。
/// 只判断是否「全是空白」，材料本身原样保留 —— 口令里的空格是口令的一部分，
/// 擅自 `trim` 会把一把能用的 key 变成解不开的 key。
fn passphrase_for_storage(passphrase: Option<&str>) -> Option<String> {
    passphrase
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

/// 一次认证写入在 keychain 里真正落下的条目。
///
/// `agent` 两条都是 `None`：ssh-agent 的身份在 agent 自己手里，Yukinal 不落任何 secret。
#[derive(Debug)]
struct StagedSecrets {
    method: &'static str,
    credential_ref: Option<CredentialRef>,
    passphrase_ref: Option<CredentialRef>,
}

impl StagedSecrets {
    /// `identities.credential_ref` 的取值。agent 身份没有凭据条目，写空串 ——
    /// 该列是 `NOT NULL`，空串诚实表示「这个身份没有 secret」，比编一个指向不存在
    /// 条目的假引用好（假引用会让「引用存在」和「条目存在」这两件事对不上）。
    fn credential_ref_string(&self) -> String {
        self.credential_ref
            .as_ref()
            .map_or_else(String::new, CredentialRef::to_string_ref)
    }

    fn passphrase_ref_string(&self) -> Option<String> {
        self.passphrase_ref
            .as_ref()
            .map(CredentialRef::to_string_ref)
    }

    /// SQLite 侧的 `identities` 行。两条引用（私钥 + 口令）都在这里落到列上，
    /// 而不是由调用点各拼一次 —— 少一个调用点忘掉 `passphrase_ref` 的机会。
    fn to_identity(&self, id: String, label: String, now: &str) -> Identity {
        Identity {
            id,
            label,
            method: self.method.to_string(),
            credential_ref: self.credential_ref_string(),
            passphrase_ref: self.passphrase_ref_string(),
            created_at: now.to_string(),
        }
    }

    /// 回滚这一次写入：私钥与口令两条都删（`delete` 是幂等的，所以口令写失败时
    /// 已经删过一次私钥也无所谓）。
    fn rollback(&self, credentials: &dyn CredentialStore) {
        for reference in self.credential_ref.iter().chain(self.passphrase_ref.iter()) {
            let _ = credentials.delete(reference);
        }
    }
}

/// 身份编排需要的两个句柄：SQLite + keychain。
///
/// 收成这个结构体而不是直接吃 `State<AppState>`，是为了**可测**：`AppState` 绑着
/// Tauri runtime、`RusshBackend` 与真实的 `OsCredentialStore`，单元测试里造不出来。
/// 这里只要求两个句柄，于是「带口令的身份写两条条目、回收时删两条」这条路径可以用
/// `MemoryCredentialStore` + 临时库真实驱动，而不是靠读代码相信它。
#[derive(Clone, Copy)]
struct IdentitySecrets<'a> {
    database: &'a Database,
    credentials: &'a dyn CredentialStore,
}

impl<'a> IdentitySecrets<'a> {
    fn from_state(state: &'a State<'_, AppState>) -> Self {
        Self {
            database: &state.database,
            credentials: state.credentials.as_ref(),
        }
    }

    /// `AuthenticationInput` → （`identities` 字段、keychain 条目）。
    ///
    /// 三种认证方式在这里收口，新增与更新两条路径共用同一份实现：之前两处各写一份
    /// `match`，正是「一处支持口令、另一处显式拒绝」这种漂移的温床。
    fn stage(
        &self,
        authentication: &AuthenticationInput,
        account: &str,
    ) -> Result<StagedSecrets, String> {
        match authentication {
            AuthenticationInput::Password { password } => Ok(StagedSecrets {
                method: "password",
                credential_ref: Some(
                    self.credentials
                        .set("ssh", account, &Secret::from_utf8(password.clone()))
                        .map_err(|error| error.to_string())?,
                ),
                passphrase_ref: None,
            }),
            AuthenticationInput::PrivateKey {
                private_key_pem,
                passphrase,
            } => {
                let key_ref = self
                    .credentials
                    .set("ssh", account, &Secret::from_utf8(private_key_pem.clone()))
                    .map_err(|error| error.to_string())?;
                // 空 / 纯空白口令：不写第二条条目，身份就是「明文 key」。
                let Some(passphrase) = passphrase_for_storage(passphrase.as_deref()) else {
                    return Ok(StagedSecrets {
                        method: "privateKey",
                        credential_ref: Some(key_ref),
                        passphrase_ref: None,
                    });
                };
                match self.credentials.set(
                    "ssh",
                    &format!("{account}{PASSPHRASE_ACCOUNT_SUFFIX}"),
                    &Secret::from_utf8(passphrase),
                ) {
                    Ok(passphrase_ref) => Ok(StagedSecrets {
                        method: "privateKey",
                        credential_ref: Some(key_ref),
                        passphrase_ref: Some(passphrase_ref),
                    }),
                    Err(error) => {
                        // 不能留下「有 key、没口令」的身份：加密 key 要等到认证那一刻
                        // 才报 PassphraseRequired，用户根本不知道是保存失败。当场回滚私钥。
                        let _ = self.credentials.delete(&key_ref);
                        Err(error.to_string())
                    }
                }
            }
            AuthenticationInput::Agent => Ok(StagedSecrets {
                method: "agent",
                credential_ref: None,
                passphrase_ref: None,
            }),
            // 「引用已存在的身份」的全部语义就是不改凭据，调用点必须先处理掉它。
            AuthenticationInput::Identity { .. } => {
                Err("an identity reference stages no secrets".to_string())
            }
        }
    }

    /// 删除一个身份的 SQLite 行，并回收它写下的**全部** keychain 条目。
    ///
    /// 口令条目必须一起删：只删私钥的话，口令会永远留在 keychain 里再也没人引用
    /// （拿不回来，也没人会清）。解析不出来的引用（agent 身份的空串）跳过 ——
    /// 它本来就没有对应条目。
    fn reclaim(&self, identity_id: &str) -> Result<(), String> {
        if let Ok(identity) = self.database.identities().get(identity_id) {
            for reference in [
                Some(identity.credential_ref.as_str()),
                identity.passphrase_ref.as_deref(),
            ]
            .into_iter()
            .flatten()
            {
                if let Ok(reference) = CredentialRef::parse(reference) {
                    self.credentials
                        .delete(&reference)
                        .map_err(|error| error.to_string())?;
                }
            }
            let _ = self.database.identities().delete(identity_id);
        }
        Ok(())
    }

    /// 带回守卫的回收：身份还挂在别的服务器上时**什么都不删**。
    ///
    /// 身份是共享的，只要它还挂在别的服务器上，它的凭据（私钥与口令）就还在被那条
    /// 服务器用，删掉等于把别人的连接毁掉。守卫和删除写在同一个函数里，是为了能用
    /// 真实数据同时测出「该跳过时跳过」和「该删时删」两种结果 —— 守卫散在调用点的话，
    /// 只能靠读代码确认它还在。
    fn reclaim_if_unshared(&self, identity_id: &str, server_id: &str) -> Result<(), String> {
        if self
            .database
            .identities()
            .attached_to_other_server(identity_id, server_id)
            .map_err(|error| error.to_string())?
        {
            return Ok(());
        }
        self.reclaim(identity_id)
    }
}

/// 两条创建路径的差异，收成一个参数而不是两份实现。
enum IdentityWrite {
    /// `server_add`：account 沿用历史约定 `{serverId}`；挂载由
    /// `insert_server_and_attach_identity` 在服务器行落库后完成。
    Add,
    /// `server_update`：account 用 `{serverId}-{cred…}`，与旧凭据条目区分开；
    /// 并且当场挂到服务器上（旧会话已经被断开，新身份必须立刻生效）。
    Replace,
}

/// 写入一个新身份：secret 进 OS keychain，SQLite 只存引用。
///
/// 「已存在的身份」引用在这里短路返回 —— 那条路径不写任何凭据。
async fn store_identity(
    state: &State<'_, AppState>,
    authentication: &AuthenticationInput,
    label: &str,
    server_id: &str,
    write: IdentityWrite,
    now: &str,
) -> Result<String, String> {
    let secrets = IdentitySecrets::from_state(state);
    if let AuthenticationInput::Identity { identity_id } = authentication {
        secrets
            .database
            .identities()
            .get(identity_id)
            .map_err(|error| error.to_string())?;
        return Ok(identity_id.clone());
    }

    let account = match write {
        IdentityWrite::Add => server_id.to_string(),
        IdentityWrite::Replace => format!("{server_id}-{}", next_id("cred")),
    };
    let staged = secrets.stage(authentication, &account)?;
    let identity = staged.to_identity(next_id("idn"), format!("{label} ({server_id})"), now);
    if let Err(error) = secrets.database.identities().insert(&identity) {
        staged.rollback(secrets.credentials);
        return Err(error.to_string());
    }
    if matches!(write, IdentityWrite::Replace) {
        if let Err(error) = secrets
            .database
            .identities()
            .attach_to_server(server_id, &identity.id)
        {
            let _ = secrets.database.identities().delete(&identity.id);
            staged.rollback(secrets.credentials);
            return Err(error.to_string());
        }
    }
    Ok(identity.id)
}

/// 回收身份的 keychain 条目与 SQLite 行（带 `attached_to_other_server` 守卫）。
fn reclaim_identity(
    state: &State<'_, AppState>,
    identity_id: &str,
    server_id: &str,
) -> Result<(), String> {
    IdentitySecrets::from_state(state).reclaim_if_unshared(identity_id, server_id)
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
                passphrase_ref: None,
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

    // -- keychain 侧：两条条目 + 回收 -----------------------------------------

    use super::{passphrase_for_storage, IdentitySecrets};
    use std::path::PathBuf;
    use yukinal_credentials::memory::MemoryCredentialStore;
    use yukinal_credentials::{CredentialRef, CredentialStore, Secret};
    use yukinal_database::{AuthenticationInput, Database};

    const NOW: &str = "2026-01-01T00:00:00.000Z";

    fn temp_database(tag: &str) -> (PathBuf, Database) {
        let path = std::env::temp_dir().join(format!(
            "yukinal-identity-{tag}-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
        let database = Database::open(&path).expect("open database");
        (path, database)
    }

    /// 带口令的私钥写下**两条**条目：私钥 `keychain://ssh/{account}`、口令
    /// `keychain://ssh/{account}-passphrase`。两条分开是刻意的（见
    /// `PASSPHRASE_ACCOUNT_SUFFIX`），这条用例把它钉住：一旦有人「简化」成一个
    /// blob，这里会红。
    #[test]
    fn a_passphrase_writes_a_second_entry_under_its_own_account() {
        let (_path, database) = temp_database("pair");
        let store = MemoryCredentialStore::new();
        let secrets = IdentitySecrets {
            database: &database,
            credentials: &store,
        };

        let staged = secrets
            .stage(
                &AuthenticationInput::PrivateKey {
                    private_key_pem: "-----BEGIN OPENSSH PRIVATE KEY-----".into(),
                    passphrase: Some("correct horse".into()),
                },
                "srv_pair",
            )
            .expect("stage an encrypted key");

        let identity = staged.to_identity("idn_pair".into(), "prod".into(), NOW);
        assert_eq!(identity.method, "privateKey");
        assert_eq!(identity.credential_ref, "keychain://ssh/srv_pair");
        assert_eq!(
            identity.passphrase_ref.as_deref(),
            Some("keychain://ssh/srv_pair-passphrase"),
        );
        assert_ne!(
            identity.credential_ref,
            identity.passphrase_ref.clone().unwrap_or_default(),
            "私钥与口令必须落在不同条目上"
        );

        let key_ref = CredentialRef::parse(&identity.credential_ref).expect("key ref");
        let pass_ref =
            CredentialRef::parse(identity.passphrase_ref.as_deref().expect("passphrase ref"))
                .expect("passphrase ref");
        assert_eq!(
            store.get(&key_ref).expect("key").as_utf8().expect("utf8"),
            "-----BEGIN OPENSSH PRIVATE KEY-----"
        );
        assert_eq!(
            store
                .get(&pass_ref)
                .expect("passphrase")
                .as_utf8()
                .expect("utf8"),
            "correct horse"
        );

        database
            .identities()
            .insert(&identity)
            .expect("insert identity");
        secrets.reclaim("idn_pair").expect("reclaim");

        assert!(!store.has(&key_ref).expect("has key"));
        assert!(!store.has(&pass_ref).expect("has passphrase"));
        assert!(
            database.identities().get("idn_pair").is_err(),
            "回收后 SQLite 行也要消失"
        );
    }

    /// 明文 key（口令为空）只写一条条目，`passphrase_ref` 留空 —— 这是
    /// `crates/ssh`「空口令 = 没有口令」那条规则在存储侧的对应。
    #[test]
    fn an_empty_or_blank_passphrase_stores_no_passphrase_entry() {
        let (_path, database) = temp_database("plain");
        let store = MemoryCredentialStore::new();
        let secrets = IdentitySecrets {
            database: &database,
            credentials: &store,
        };

        for passphrase in [None, Some(String::new()), Some("   ".to_string())] {
            let staged = secrets
                .stage(
                    &AuthenticationInput::PrivateKey {
                        private_key_pem: "-----BEGIN OPENSSH PRIVATE KEY-----".into(),
                        passphrase,
                    },
                    "srv_plain",
                )
                .expect("stage a plaintext key");
            let identity = staged.to_identity("idn_plain".into(), "prod".into(), NOW);
            assert_eq!(identity.passphrase_ref, None);
            // 口令条目**不存在**，而不是存在但为空。
            assert!(
                store
                    .get(&CredentialRef::new("ssh", "srv_plain-passphrase"))
                    .is_err(),
                "空口令不该写第二条条目"
            );
        }
    }

    /// 口令里的前后空格是口令的一部分，不能 trim 掉；只有「全是空白」才等于没有口令。
    #[test]
    fn whitespace_inside_a_passphrase_is_preserved() {
        assert_eq!(
            passphrase_for_storage(Some("  spaced  ")).as_deref(),
            Some("  spaced  "),
        );
        assert_eq!(passphrase_for_storage(Some(" \t ")), None);
        assert_eq!(passphrase_for_storage(Some("")), None);
        assert_eq!(passphrase_for_storage(None), None);
    }

    /// agent 身份不写任何 secret，`credential_ref` 是空串（列是 NOT NULL，空串表示
    /// 「没有凭据条目」）。回收这种身份不能因为引用解析不出来就报错。
    #[test]
    fn an_agent_identity_stores_no_secret_and_reclaims_cleanly() {
        let (_path, database) = temp_database("agent");
        let store = MemoryCredentialStore::new();
        let secrets = IdentitySecrets {
            database: &database,
            credentials: &store,
        };

        let staged = secrets
            .stage(&AuthenticationInput::Agent, "srv_agent")
            .expect("stage an agent identity");
        let identity = staged.to_identity("idn_agent".into(), "prod".into(), NOW);
        assert_eq!(identity.method, "agent");
        assert_eq!(identity.credential_ref, "");
        assert_eq!(identity.passphrase_ref, None);

        database
            .identities()
            .insert(&identity)
            .expect("insert identity");
        secrets.reclaim("idn_agent").expect("reclaim an agent");
        assert!(database.identities().get("idn_agent").is_err());
    }

    /// 更新路径的 account 约定（`{serverId}-{cred…}`）同样必须支持口令 —— 这条用例
    /// 防止「新增支持口令、更新拒绝口令」再次出现。
    #[test]
    fn a_passphrase_survives_the_replace_paths_account_convention() {
        let (_path, database) = temp_database("replace");
        let store = MemoryCredentialStore::new();
        let secrets = IdentitySecrets {
            database: &database,
            credentials: &store,
        };
        let account = format!("srv_replace-{}", super::next_id("cred"));
        let expected_passphrase_ref = format!(
            "keychain://ssh/{account}{}",
            super::PASSPHRASE_ACCOUNT_SUFFIX
        );

        let staged = secrets
            .stage(
                &AuthenticationInput::PrivateKey {
                    private_key_pem: "key".into(),
                    passphrase: Some("pw".into()),
                },
                &account,
            )
            .expect("stage via the update path's account");
        let identity = staged.to_identity("idn_replace".into(), "prod".into(), NOW);
        assert_eq!(identity.credential_ref, format!("keychain://ssh/{account}"));
        assert_eq!(
            identity.passphrase_ref.as_deref(),
            Some(expected_passphrase_ref.as_str()),
        );
        // 两条条目都在，且落在不同 account 上。
        assert!(store
            .has(&CredentialRef::new("ssh", account.clone()))
            .expect("key"));
        assert!(store
            .has(&CredentialRef::new(
                "ssh",
                format!("{account}{}", super::PASSPHRASE_ACCOUNT_SUFFIX)
            ))
            .expect("passphrase"));
    }

    /// 共享身份：还挂在别的服务器上时回收必须**什么都不删**，解挂之后才真的删掉两条
    /// 条目。这正是 `reclaim_identity` 那个守卫要守的东西。
    #[test]
    fn a_shared_identity_is_reclaimed_only_after_the_last_server_lets_go() {
        let (_path, database) = temp_database("shared");
        let store = MemoryCredentialStore::new();
        let secrets = IdentitySecrets {
            database: &database,
            credentials: &store,
        };
        let key_ref = store
            .set("ssh", "shared-key", &Secret::from_utf8("key material"))
            .expect("set key");
        let pass_ref = store
            .set("ssh", "shared-key-passphrase", &Secret::from_utf8("pw"))
            .expect("set passphrase");
        database
            .identities()
            .insert(&Identity {
                id: "idn_shared".into(),
                label: "shared".into(),
                method: "privateKey".into(),
                credential_ref: key_ref.to_string_ref(),
                passphrase_ref: Some(pass_ref.to_string_ref()),
                created_at: NOW.into(),
            })
            .expect("insert identity");
        for server_id in ["srv_a", "srv_b"] {
            database
                .servers()
                .insert(&attached_server(server_id))
                .expect("insert server");
            database
                .identities()
                .attach_to_server(server_id, "idn_shared")
                .expect("attach");
        }

        // srv_b 还挂着它 → srv_a 的回收必须放过它。
        secrets
            .reclaim_if_unshared("idn_shared", "srv_a")
            .expect("guarded reclaim");
        assert!(store.has(&key_ref).expect("key survives"));
        assert!(store.has(&pass_ref).expect("passphrase survives"));
        assert!(database.identities().get("idn_shared").is_ok());

        // srv_b 解挂 → 最后一个服务器放手，两条条目一起回收。
        database
            .identities()
            .detach_from_server("srv_b", "idn_shared")
            .expect("detach");
        secrets
            .reclaim_if_unshared("idn_shared", "srv_a")
            .expect("unshared reclaim");
        assert!(!store.has(&key_ref).expect("key gone"));
        assert!(!store.has(&pass_ref).expect("passphrase gone"));
        assert!(database.identities().get("idn_shared").is_err());
    }

    fn attached_server(id: &str) -> Server {
        Server {
            id: id.into(),
            name: id.into(),
            connection: ServerConnection {
                host: "127.0.0.1".into(),
                port: 22,
                username: "test".into(),
                identity_id: Some("idn_shared".into()),
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
        }
    }
}
