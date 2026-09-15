//! 身份（identity）的纯规则：凭据条目的命名约定、口令归一化，以及服务器行与身份的挂载顺序。
//!
//! 按本仓库的分层规则（`apps/desktop/src-tauri` 只做参数编组与事件转发，真逻辑放
//! `crates/*`），这些规则原先住在 `apps/desktop/src-tauri/src/commands/server.rs` 里。
//! 它们不碰 Tauri、不碰 keychain，只描述**存储布局**，所以能在本 crate 里被直接测试。
//!
//! 真正写 keychain 的那一半（`IdentitySecrets` / `store_identity` / `reclaim_identity`）
//! 留在命令层：它需要 `yukinal-credentials` 的 `CredentialStore`，而本 crate 不依赖那个
//! crate（`crates/core/Cargo.toml` 里没有它）。规则与 I/O 因此沿着「谁需要什么句柄」这条
//! 线分开，而不是按文件长度切。

use yukinal_database::models::Server;
use yukinal_database::Database;

/// 口令条目在 keychain 里的 account 后缀。
///
/// 私钥与口令是同一个 `ssh` service 下的**两条**条目：`{account}` 与
/// `{account}-passphrase`。分开存而不是拼成一个 blob，有两个理由：
///
/// 1. 一个条目解析失败只丢它自己 —— 拼成一个 blob 的话，口令部分坏掉就等于把私钥
///    一起丢了，而两者是完全独立的材料；
/// 2. 私钥条目的 account 与升级前完全一致，旧版本代码读的是同一个条目，不会因为
///    新版本写过一次就找不到 key。
pub const PASSPHRASE_ACCOUNT_SUFFIX: &str = "-passphrase";

/// 空 / 纯空白口令 = 「没有口令」。
///
/// 与 `crates/ssh` 的规则一致（`load_private_key` 把空口令过滤成「没给口令」）。
/// 只判断是否「全是空白」，材料本身原样保留 —— 口令里的空格是口令的一部分，
/// 擅自 `trim` 会把一把能用的 key 变成解不开的 key。
#[must_use]
pub fn passphrase_for_storage(passphrase: Option<&str>) -> Option<String> {
    passphrase
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

/// 两条创建路径的差异，收成一个参数而不是两份实现。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityWrite {
    /// `server_add`：account 沿用历史约定 `{serverId}`；挂载由
    /// [`insert_server_and_attach_identity`] 在服务器行落库后完成。
    Add,
    /// `server_update`：account 用 `{serverId}-{cred…}`，与旧凭据条目区分开；
    /// 并且当场挂到服务器上（旧会话已经被断开，新身份必须立刻生效）。
    Replace,
}

/// 这次写入在 keychain 里的 account。
///
/// 只有 `Replace` 才铸新 id（`mint` 是惰性的）：`next_id` 会推进一个进程级计数器，
/// 在 `Add` 分支提前调用它会让后续所有 id 的形状变掉 —— 那是没有收益的行为改变。
///
/// 把 `{serverId}` 与 `{serverId}-{cred…}` 两种约定摆在一起，是为了让「新增」与「更新」
/// 写下的条目能不能相撞这件事在一处可见：更新路径必须换 account，否则新凭据会覆盖旧凭据
/// 的条目，而旧条目正是回滚时要删的东西。
#[must_use]
pub fn identity_account<F>(server_id: &str, write: IdentityWrite, mint: F) -> String
where
    F: FnOnce() -> String,
{
    match write {
        IdentityWrite::Add => server_id.to_string(),
        IdentityWrite::Replace => format!("{server_id}-{}", mint()),
    }
}

/// 服务器行先落库，身份挂载后落库；第二步失败时**不留下**一个半成品服务器行。
pub fn insert_server_and_attach_identity(
    database: &Database,
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

#[cfg(test)]
mod tests {
    use super::{insert_server_and_attach_identity, passphrase_for_storage};
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
                private_key_path: None,
                certificate_path: None,
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
                host_certificate_authority: None,
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
}
