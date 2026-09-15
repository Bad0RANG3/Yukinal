//! 网络设置：出站 HTTP 走直连还是系统代理（ADR 0022）。
//!
//! 三条不做让步的规则：
//!
//! - **默认直连，而且是显式的**：装上代理软件不会悄悄改变应用的连接路径。
//! - **代理只改变连接怎么走**：endpoint 的 HTTPS 策略、重定向策略、认证头一条都不放宽；
//!   代理读到了却用不了的配置（只配了 PAC、URL 里内嵌凭据）**失败**，不静默直连。
//! - **凭据只进系统凭据库**：SQLite 里只有引用，响应里只有「有没有」，日志与 Debug 里没有值。

use serde::{Deserialize, Serialize};
use std::fmt;
use tauri::State;
use yukinal_credentials::{CredentialRef, CredentialStore, Secret};
use yukinal_database::models::NetworkProxyConfig;
use yukinal_database::Database;
use yukinal_net::{NetworkProxy, NetworkProxyMode, OutboundProxy, ProxyCredential};

use crate::commands::server::next_id;
use crate::state::AppState;

/// 设置页看到的代理状态：选了什么、有没有凭据、**此刻**解析成什么。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkProxyView {
    pub mode: NetworkProxyMode,
    /// 凭据库里有没有代理凭据。值本身永远不出现在这里。
    pub has_credential: bool,
    pub resolution: NetworkProxyResolution,
}

/// 解析结果。`kind` 是判别式，UI 按它分支，不需要猜字段在不在。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum NetworkProxyResolution {
    /// 直连。
    Direct,
    /// 经这个代理；`source` 说明它是从哪读来的。
    #[serde(rename_all = "camelCase")]
    Proxy {
        url: String,
        source: String,
        /// 系统配置里有没有「不走代理」的例外列表。
        has_no_proxy: bool,
    },
    /// 读到了配置但不能用（PAC、URL 非法……）：连接会失败，理由在这里。
    Unusable { reason: String },
}

/// 保存入参。`credential` 是只写字段：非空表示轮换，缺省或空表示保留已存的。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NetworkProxyInput {
    pub mode: NetworkProxyMode,
    #[serde(default)]
    pub credential: Option<String>,
    /// 显式删除已存凭据（与「留空保留」区分开）。
    #[serde(default)]
    pub clear_credential: bool,
}

impl fmt::Debug for NetworkProxyInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NetworkProxyInput")
            .field("mode", &self.mode)
            .field(
                "credential",
                &self.credential.as_ref().map(|_| "<redacted>"),
            )
            .field("clear_credential", &self.clear_credential)
            .finish()
    }
}

/// 读一次应用级代理设置，并把凭据从系统凭据库取出来。
///
/// MCP 传输、OAuth 与 SSH 的 KRL 下载都用这一个函数：分头各读一次会得到「同一个进程里
/// 两条请求走了两条路」，而那种不一致只在网络出问题时才显形。
pub(crate) fn resolve_outbound_proxy(
    database: &Database,
    credentials: &dyn CredentialStore,
) -> Result<OutboundProxy, String> {
    let config = database
        .app_settings()
        .network_proxy()
        .map_err(|error| format!("could not read the network proxy setting: {error}"))?;
    let proxy = yukinal_net::resolve(config.mode);
    let credential = match (config.mode, config.credential_ref.as_deref()) {
        (NetworkProxyMode::System, Some(reference)) => {
            let reference = CredentialRef::parse(reference)
                .map_err(|error| format!("invalid proxy credential reference: {error}"))?;
            let secret = credentials
                .get(&reference)
                .map_err(|error| format!("could not read the proxy credential: {error}"))?
                .as_utf8()
                .map_err(|error| format!("the proxy credential is not UTF-8: {error}"))?
                .into_owned();
            Some(ProxyCredential::new(secret)?)
        }
        _ => None,
    };
    Ok(OutboundProxy { proxy, credential })
}

#[tauri::command]
pub async fn network_proxy_get(state: State<'_, AppState>) -> Result<NetworkProxyView, String> {
    view(&state.database, state.credentials.as_ref())
}

#[tauri::command]
pub async fn network_proxy_save(
    state: State<'_, AppState>,
    input: NetworkProxyInput,
) -> Result<NetworkProxyView, String> {
    save(&state.database, state.credentials.as_ref(), input)
}

/// 保存逻辑本身（命令只是把它接到 `AppState` 上），这样它可以在测试里被直接调用。
fn save(
    database: &Database,
    credentials: &dyn CredentialStore,
    input: NetworkProxyInput,
) -> Result<NetworkProxyView, String> {
    if input.credential.is_some() && input.clear_credential {
        return Err("代理凭据与清除操作不能同时提交".to_string());
    }
    if input.mode == NetworkProxyMode::Direct
        && input
            .credential
            .as_deref()
            .is_some_and(|value| !value.is_empty())
    {
        return Err("直连模式不能提交新的代理凭据，请先选择系统代理".to_string());
    }
    let existing = database
        .app_settings()
        .network_proxy()
        .map_err(|error| format!("could not read the network proxy setting: {error}"))?;
    let mut staged: Option<CredentialRef> = None;
    let credential_ref = if input.clear_credential {
        None
    } else {
        match input.credential.filter(|value| !value.is_empty()) {
            Some(value) => {
                let credential = ProxyCredential::new(value)
                    .map_err(|reason| format!("代理凭据无法使用：{reason}"))?;
                let reference = credentials
                    .set(
                        "mcp",
                        &format!("network-proxy-{}", next_id("credential")),
                        &Secret::from_utf8(credential.expose().to_string()),
                    )
                    .map_err(|error| format!("could not store the proxy credential: {error}"))?;
                staged = Some(reference.clone());
                Some(reference.to_string_ref())
            }
            None => existing.credential_ref.clone(),
        }
    };

    let config = NetworkProxyConfig {
        mode: input.mode,
        credential_ref: credential_ref.clone(),
    };
    if let Err(error) = database.app_settings().save_network_proxy(&config) {
        if let Some(staged) = &staged {
            let _ = credentials.delete(staged);
        }
        return Err(format!("could not save the network proxy setting: {error}"));
    }
    // 旧凭据只在没人指向它时回收：留空保留、轮换与显式清除都会走到这里。
    if let Some(previous) = existing.credential_ref {
        if credential_ref.as_deref() != Some(previous.as_str()) {
            if let Ok(reference) = CredentialRef::parse(&previous) {
                if let Err(error) = credentials.delete(&reference) {
                    tracing::warn!(
                        "saved the network proxy setting but could not reclaim its previous credential: {error}"
                    );
                }
            }
        }
    }
    view(database, credentials)
}

fn view(
    database: &Database,
    credentials: &dyn CredentialStore,
) -> Result<NetworkProxyView, String> {
    let config = database
        .app_settings()
        .network_proxy()
        .map_err(|error| format!("could not read the network proxy setting: {error}"))?;
    let has_credential = match config.credential_ref.as_deref() {
        Some(reference) => {
            let reference = CredentialRef::parse(reference)
                .map_err(|error| format!("invalid proxy credential reference: {error}"))?;
            credentials
                .has(&reference)
                .map_err(|error| format!("could not check the proxy credential: {error}"))?
        }
        None => false,
    };
    Ok(NetworkProxyView {
        mode: config.mode,
        has_credential,
        resolution: describe(yukinal_net::resolve(config.mode)),
    })
}

fn describe(proxy: NetworkProxy) -> NetworkProxyResolution {
    match proxy {
        NetworkProxy::Direct => NetworkProxyResolution::Direct,
        NetworkProxy::Proxy {
            url,
            source,
            no_proxy,
        } => NetworkProxyResolution::Proxy {
            url,
            source: source.describe().to_string(),
            has_no_proxy: no_proxy.is_some(),
        },
        NetworkProxy::Unusable { reason } => NetworkProxyResolution::Unusable { reason },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use yukinal_credentials::memory::MemoryCredentialStore;
    use yukinal_database::models::NetworkProxyConfig;

    /// 一个临时库。测试之间用文件名区分，避免并行跑测试时互相看见对方的写入。
    fn temp_db(name: &str) -> (PathBuf, Database) {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "yukinal-network-{}-{}-{}.sqlite",
            std::process::id(),
            name,
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        remove(&path);
        let database = Database::open(&path).expect("open temp database");
        (path, database)
    }

    fn remove(path: &Path) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(PathBuf::from(format!("{}{suffix}", path.display())));
        }
    }

    #[test]
    fn the_default_is_direct_and_needs_no_credential() {
        let (path, db) = temp_db("default");
        let credentials = MemoryCredentialStore::new();
        let view = view(&db, &credentials).expect("view");
        assert_eq!(view.mode, NetworkProxyMode::Direct);
        assert!(!view.has_credential);
        assert!(matches!(view.resolution, NetworkProxyResolution::Direct));
        assert_eq!(
            resolve_outbound_proxy(&db, &credentials)
                .expect("resolve")
                .proxy,
            NetworkProxy::Direct
        );
        remove(&path);
    }

    #[test]
    fn direct_mode_does_not_require_a_stored_proxy_credential() {
        let (path, db) = temp_db("direct-missing-credential");
        let credentials = MemoryCredentialStore::new();
        db.app_settings()
            .save_network_proxy(&NetworkProxyConfig {
                mode: NetworkProxyMode::Direct,
                credential_ref: Some("keychain://mcp/missing-proxy-credential".to_string()),
            })
            .expect("save");

        let resolved = resolve_outbound_proxy(&db, &credentials).expect("direct resolution");
        assert_eq!(resolved.proxy, NetworkProxy::Direct);
        assert!(resolved.credential.is_none());
        remove(&path);
    }

    #[test]
    fn saving_a_credential_keeps_the_value_out_of_sqlite_and_out_of_the_response() {
        let (path, db) = temp_db("save-credential");
        let credentials = MemoryCredentialStore::new();

        let saved = save(
            &db,
            &credentials,
            NetworkProxyInput {
                mode: NetworkProxyMode::System,
                credential: Some("corp\\user:p@ss".to_string()),
                clear_credential: false,
            },
        )
        .expect("save");
        assert!(saved.has_credential);
        assert_eq!(saved.mode, NetworkProxyMode::System);
        let rendered = serde_json::to_string(&saved).expect("serialize");
        assert!(
            !rendered.contains("p@ss"),
            "the settings response must not carry the credential: {rendered}"
        );
        let stored = db.app_settings().network_proxy().expect("stored");
        let reference = stored.credential_ref.clone().expect("reference");
        assert!(
            reference.starts_with("keychain://"),
            "SQLite keeps a credential-store reference, never the value: {reference}"
        );

        // 留空保留：引用不变，凭据还在。
        let preserved = save(
            &db,
            &credentials,
            NetworkProxyInput {
                mode: NetworkProxyMode::System,
                credential: Some(String::new()),
                clear_credential: false,
            },
        )
        .expect("preserve");
        assert!(preserved.has_credential);
        assert_eq!(
            db.app_settings()
                .network_proxy()
                .expect("stored")
                .credential_ref
                .as_deref(),
            Some(reference.as_str())
        );

        // 显式清除：引用消失，凭据被回收。
        let cleared = save(
            &db,
            &credentials,
            NetworkProxyInput {
                mode: NetworkProxyMode::Direct,
                credential: None,
                clear_credential: true,
            },
        )
        .expect("clear");
        assert!(!cleared.has_credential);
        assert!(db
            .app_settings()
            .network_proxy()
            .expect("stored")
            .credential_ref
            .is_none());
        assert!(
            !credentials
                .has(&CredentialRef::parse(&reference).expect("reference"))
                .expect("has"),
            "a cleared credential must be reclaimed from the store"
        );
        remove(&path);
    }

    #[test]
    fn a_credential_that_is_not_user_colon_password_is_refused() {
        let (path, db) = temp_db("bad-credential");
        let credentials = MemoryCredentialStore::new();
        let error = save(
            &db,
            &credentials,
            NetworkProxyInput {
                mode: NetworkProxyMode::System,
                credential: Some("no-colon".to_string()),
                clear_credential: false,
            },
        )
        .expect_err("a credential without `user:password` must be refused");
        assert!(error.contains("代理凭据无法使用"), "{error}");
        assert!(
            db.app_settings()
                .network_proxy()
                .expect("stored")
                .credential_ref
                .is_none(),
            "a refused save must not leave a credential behind"
        );
        remove(&path);
    }

    #[test]
    fn network_proxy_input_debug_redacts_the_credential() {
        let input = NetworkProxyInput {
            mode: NetworkProxyMode::System,
            credential: Some("user:secret".to_string()),
            clear_credential: false,
        };
        let debug = format!("{input:?}");
        assert!(!debug.contains("user:secret"), "{debug}");
        assert!(debug.contains("redacted"), "{debug}");
    }

    #[test]
    fn a_new_credential_cannot_be_combined_with_clear_or_direct_mode() {
        let (path, db) = temp_db("invalid-input");
        let credentials = MemoryCredentialStore::new();
        let both = save(
            &db,
            &credentials,
            NetworkProxyInput {
                mode: NetworkProxyMode::System,
                credential: Some("user:pass".to_string()),
                clear_credential: true,
            },
        )
        .expect_err("replacement and clear must be mutually exclusive");
        assert!(both.contains("不能同时"), "{both}");

        let direct = save(
            &db,
            &credentials,
            NetworkProxyInput {
                mode: NetworkProxyMode::Direct,
                credential: Some("user:pass".to_string()),
                clear_credential: false,
            },
        )
        .expect_err("a new proxy credential must not be accepted in direct mode");
        assert!(direct.contains("系统代理"), "{direct}");
        assert!(
            db.app_settings()
                .network_proxy()
                .expect("stored")
                .credential_ref
                .is_none(),
            "rejected input must not store a credential"
        );
        remove(&path);
    }

    #[test]
    fn a_credential_reference_resolves_into_a_material_without_leaking_it() {
        let (path, db) = temp_db("credential");
        let credentials = MemoryCredentialStore::new();
        let reference = credentials
            .set(
                "mcp",
                "network-proxy-test",
                &Secret::from_utf8("corp\\user:p@ss".to_string()),
            )
            .expect("store");
        db.app_settings()
            .save_network_proxy(&NetworkProxyConfig {
                mode: NetworkProxyMode::System,
                credential_ref: Some(reference.to_string_ref()),
            })
            .expect("save");

        let resolved = resolve_outbound_proxy(&db, &credentials).expect("resolve");
        assert!(
            !format!("{resolved:?}").contains("p@ss"),
            "Debug must redact"
        );
        let credential = resolved.credential.clone().expect("credential");
        assert_eq!(credential.expose(), "corp\\user:p@ss");
        assert!(credentials.has(&reference).expect("has"));
        remove(&path);
    }
}
