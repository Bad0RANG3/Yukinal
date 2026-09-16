//! 身份写入与回收的 keychain 侧：一次认证在 keychain 里真正落下的条目，以及把它们
//! 全部收回来的路径。
//!
//! 纯规则（account 命名约定、口令归一化、服务器行与身份的挂载顺序）在
//! `yukinal_core::identity`；这里只留下**需要 keychain 句柄**的那一半 ——
//! `CredentialStore` 属于 `yukinal-credentials`，而 `yukinal-core` 不依赖它。

use tauri::State;

use crate::state::AppState;
use yukinal_core::identity::{
    identity_account, passphrase_for_storage, IdentityWrite, PASSPHRASE_ACCOUNT_SUFFIX,
};
use yukinal_credentials::{CredentialRef, CredentialStore, Secret};
use yukinal_database::models::Identity;
use yukinal_database::{AuthenticationInput, Database, DatabaseError};

use super::next_id;

/// 一次认证写入在 keychain 里真正落下的条目。
///
/// `agent` 两条都是 `None`：ssh-agent 的身份在 agent 自己手里，Yukinal 不落任何 secret。
#[derive(Debug)]
struct StagedSecrets {
    method: &'static str,
    credential_ref: Option<CredentialRef>,
    passphrase_ref: Option<CredentialRef>,
    private_key_path: Option<String>,
    certificate_path: Option<String>,
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
            private_key_path: self.private_key_path.clone(),
            certificate_path: self.certificate_path.clone(),
            created_at: now.to_string(),
        }
    }

    /// 回滚这一次写入：私钥与口令两条都删（`delete` 是幂等的，所以口令写失败时
    /// 已经删过一次私钥也无所谓）。
    fn rollback(&self, database: &yukinal_database::Database, credentials: &dyn CredentialStore) {
        for reference in self.credential_ref.iter().chain(self.passphrase_ref.iter()) {
            let _ = crate::state::credential_cleanup::reclaim(database, credentials, reference);
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
                private_key_path: None,
                certificate_path: None,
            }),
            AuthenticationInput::PrivateKey {
                private_key_pem,
                passphrase,
            } => self.stage_private_key(
                "privateKey",
                private_key_pem,
                passphrase.as_deref(),
                None,
                None,
                account,
            ),
            AuthenticationInput::Certificate {
                private_key_pem,
                passphrase,
                certificate_path,
                private_key_path,
            } => self.stage_private_key(
                "certificate",
                private_key_pem,
                passphrase.as_deref(),
                private_key_path.clone(),
                Some(certificate_path.clone()),
                account,
            ),
            AuthenticationInput::Agent => Ok(StagedSecrets {
                method: "agent",
                credential_ref: None,
                passphrase_ref: None,
                private_key_path: None,
                certificate_path: None,
            }),
            // 「引用已存在的身份」的全部语义就是不改凭据，调用点必须先处理掉它。
            AuthenticationInput::Identity { .. } => {
                Err("an identity reference stages no secrets".to_string())
            }
        }
    }

    fn stage_private_key(
        &self,
        method: &'static str,
        private_key_pem: &str,
        passphrase: Option<&str>,
        private_key_path: Option<String>,
        certificate_path: Option<String>,
        account: &str,
    ) -> Result<StagedSecrets, String> {
        let key_ref = self
            .credentials
            .set(
                "ssh",
                account,
                &Secret::from_utf8(private_key_pem.to_string()),
            )
            .map_err(|error| error.to_string())?;
        let Some(passphrase) = passphrase_for_storage(passphrase) else {
            return Ok(StagedSecrets {
                method,
                credential_ref: Some(key_ref),
                passphrase_ref: None,
                private_key_path,
                certificate_path,
            });
        };
        match self.credentials.set(
            "ssh",
            &format!("{account}{PASSPHRASE_ACCOUNT_SUFFIX}"),
            &Secret::from_utf8(passphrase),
        ) {
            Ok(passphrase_ref) => Ok(StagedSecrets {
                method,
                credential_ref: Some(key_ref),
                passphrase_ref: Some(passphrase_ref),
                private_key_path,
                certificate_path,
            }),
            Err(error) => {
                // Never leave a key whose passphrase failed to persist: the failure
                // would otherwise appear later as `PassphraseRequired`.
                if let Err(cleanup) = crate::state::credential_cleanup::reclaim(
                    self.database,
                    self.credentials,
                    &key_ref,
                ) {
                    return Err(format!(
                        "{error}; the staged private key could not be reclaimed immediately: {cleanup}"
                    ));
                }
                Err(error.to_string())
            }
        }
    }

    /// 删除一个身份的 SQLite 行，并回收它写下的**全部** keychain 条目。
    ///
    /// 口令条目必须一起删：只删私钥的话，口令会永远留在 keychain 里再也没人引用
    /// （拿不回来，也没人会清）。解析不出来的引用（agent 身份的空串）跳过 ——
    /// 它本来就没有对应条目。
    fn reclaim(&self, identity_id: &str) -> Result<(), String> {
        let identity = match self.database.identities().get(identity_id) {
            Ok(identity) => identity,
            Err(DatabaseError::NotFound) => return Ok(()),
            Err(error) => {
                return Err(format!(
                    "could not read identity `{identity_id}` before reclaiming it: {error}"
                ));
            }
        };

        // The row must stop referencing the secrets before they can be queued for
        // deletion. Otherwise a transient backend failure would leave a live identity
        // pointing at credentials that startup reconciliation later removes.
        self.database
            .identities()
            .delete(identity_id)
            .map_err(|error| format!("could not delete identity `{identity_id}`: {error}"))?;

        let mut failures = Vec::new();
        for reference in [
            Some(identity.credential_ref.as_str()),
            identity.passphrase_ref.as_deref(),
        ]
        .into_iter()
        .flatten()
        .filter(|reference| !reference.is_empty())
        {
            let reference = match CredentialRef::parse(reference) {
                Ok(reference) => reference,
                Err(error) => {
                    failures.push(format!(
                        "identity `{identity_id}` has an invalid credential reference: {error}"
                    ));
                    continue;
                }
            };
            if let Err(error) = crate::state::credential_cleanup::reclaim(
                self.database,
                self.credentials,
                &reference,
            ) {
                failures.push(error);
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
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

/// 写入一个新身份：secret 进 OS keychain，SQLite 只存引用。
///
/// 「已存在的身份」引用在这里短路返回 —— 那条路径不写任何凭据。
pub(super) async fn store_identity(
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

    let account = identity_account(server_id, write, || next_id("cred"));
    let staged = secrets.stage(authentication, &account)?;
    let identity = staged.to_identity(next_id("idn"), format!("{label} ({server_id})"), now);
    if let Err(error) = secrets.database.identities().insert(&identity) {
        staged.rollback(secrets.database, secrets.credentials);
        return Err(error.to_string());
    }
    if matches!(write, IdentityWrite::Replace) {
        if let Err(error) = secrets
            .database
            .identities()
            .attach_to_server(server_id, &identity.id)
        {
            let _ = secrets.database.identities().delete(&identity.id);
            staged.rollback(secrets.database, secrets.credentials);
            return Err(error.to_string());
        }
    }
    Ok(identity.id)
}

/// 回收身份的 keychain 条目与 SQLite 行（带 `attached_to_other_server` 守卫）。
pub(super) fn reclaim_identity(
    state: &State<'_, AppState>,
    identity_id: &str,
    server_id: &str,
) -> Result<(), String> {
    IdentitySecrets::from_state(state).reclaim_if_unshared(identity_id, server_id)
}

#[cfg(test)]
mod tests {
    // -- keychain 侧：两条条目 + 回收 -----------------------------------------

    use super::{DatabaseError, IdentitySecrets};
    use std::path::PathBuf;
    use yukinal_core::identity::PASSPHRASE_ACCOUNT_SUFFIX;
    use yukinal_credentials::memory::MemoryCredentialStore;
    use yukinal_credentials::{CredentialError, CredentialRef, CredentialStore, Secret};
    use yukinal_database::models::{
        Environment, Identity, Server, ServerCapabilities, ServerConnection, ServerMetadata,
        ServerStatus,
    };
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

    #[derive(Debug)]
    struct FailPassphraseDelete {
        inner: MemoryCredentialStore,
    }

    impl FailPassphraseDelete {
        fn new() -> Self {
            Self {
                inner: MemoryCredentialStore::new(),
            }
        }
    }

    impl CredentialStore for FailPassphraseDelete {
        fn set(
            &self,
            service: &str,
            account: &str,
            secret: &Secret,
        ) -> Result<CredentialRef, CredentialError> {
            self.inner.set(service, account, secret)
        }

        fn get(&self, reference: &CredentialRef) -> Result<Secret, CredentialError> {
            self.inner.get(reference)
        }

        fn delete(&self, reference: &CredentialRef) -> Result<(), CredentialError> {
            if reference.account().ends_with(PASSPHRASE_ACCOUNT_SUFFIX) {
                return Err(CredentialError::Backend(
                    "injected passphrase deletion failure".into(),
                ));
            }
            self.inner.delete(reference)
        }

        fn has(&self, reference: &CredentialRef) -> Result<bool, CredentialError> {
            self.inner.has(reference)
        }
    }

    #[test]
    fn reclaim_removes_the_database_reference_before_queueing_failed_secret_deletion() {
        let (path, database) = temp_database("queue-before-delete");
        let credentials = FailPassphraseDelete::new();
        let key = credentials
            .set(
                "ssh",
                "srv_queue",
                &Secret::from_utf8("private-key-material"),
            )
            .expect("stage key");
        let passphrase = credentials
            .set(
                "ssh",
                &format!("srv_queue{PASSPHRASE_ACCOUNT_SUFFIX}"),
                &Secret::from_utf8("passphrase"),
            )
            .expect("stage passphrase");
        database
            .identities()
            .insert(&Identity {
                id: "idn_queue".into(),
                label: "queued".into(),
                method: "privateKey".into(),
                credential_ref: key.to_string_ref(),
                passphrase_ref: Some(passphrase.to_string_ref()),
                private_key_path: None,
                certificate_path: None,
                created_at: NOW.into(),
            })
            .expect("insert identity");

        let secrets = IdentitySecrets {
            database: &database,
            credentials: &credentials,
        };
        let error = secrets
            .reclaim("idn_queue")
            .expect_err("passphrase reclaim fails");
        assert!(error.contains("queued"), "{error}");
        assert!(matches!(
            database.identities().get("idn_queue"),
            Err(DatabaseError::NotFound)
        ));
        assert!(!credentials.has(&key).expect("key lookup"));
        assert!(credentials.has(&passphrase).expect("passphrase lookup"));
        let queued = database.credential_cleanup().list().expect("cleanup queue");
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].reference, passphrase.to_string_ref());

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
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

    #[test]
    fn a_certificate_identity_stores_key_secret_and_public_paths_separately() {
        let (_path, database) = temp_database("certificate");
        let store = MemoryCredentialStore::new();
        let secrets = IdentitySecrets {
            database: &database,
            credentials: &store,
        };
        let staged = secrets
            .stage(
                &AuthenticationInput::Certificate {
                    private_key_pem: "private key".into(),
                    passphrase: Some("passphrase".into()),
                    certificate_path: "/home/dev/.ssh/id_ed25519-cert.pub".into(),
                    private_key_path: Some("/home/dev/.ssh/id_ed25519".into()),
                },
                "srv_certificate",
            )
            .expect("stage certificate");
        let identity = staged.to_identity("idn_certificate".into(), "prod".into(), NOW);

        assert_eq!(identity.method, "certificate");
        assert_eq!(
            identity.certificate_path.as_deref(),
            Some("/home/dev/.ssh/id_ed25519-cert.pub")
        );
        assert_eq!(
            identity.private_key_path.as_deref(),
            Some("/home/dev/.ssh/id_ed25519")
        );
        assert_eq!(identity.credential_ref, "keychain://ssh/srv_certificate");
        assert_eq!(
            identity.passphrase_ref.as_deref(),
            Some("keychain://ssh/srv_certificate-passphrase")
        );
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
        let expected_passphrase_ref =
            format!("keychain://ssh/{account}{}", PASSPHRASE_ACCOUNT_SUFFIX);

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
                format!("{account}{}", PASSPHRASE_ACCOUNT_SUFFIX)
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
                private_key_path: None,
                certificate_path: None,
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
            created_at: NOW.into(),
            updated_at: NOW.into(),
        }
    }
}
