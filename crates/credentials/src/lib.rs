//! yukinal-credentials — 全项目唯一接触 secret 材料的出入口。
//!
//! 规则（与 shared 契约一致）：
//! - 私钥 / SSH 密码 / API Key / Cloud Credential / Provider Token **绝不**进 SQLite；
//!   SQLite 只存 [`CredentialRef`]，例如 `keychain://ssh/ssh_deploy_key_1`。
//! - 后端：macOS Keychain / Windows Credential Manager / Linux Secret Service
//!   （经 `keyring`），外加 [`memory::MemoryCredentialStore`] 供测试与开发。
//! - `Secret` 没有会打印内容的 `Display`/`Debug`：任何把 secret 写进日志的路径
//!   都过不了编译（直接 `{:?}` 只会得到 `Secret(<redacted>)`）。
//! - 删除 server 时的凭据回收 = 按 `CredentialRef` 调 [`CredentialStore::delete`]；
//!   SQLite 侧的 identity/server_identities 行由 database crate 处理（FK 级联），
//!   编排点放在 command 层（server delete 落库时的两条腿）。

pub mod memory;
pub mod os;

use std::borrow::Cow;
use std::fmt;

/// 不透明字节。故意不实现会泄露内容的 `Display`/`Debug`。
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Vec<u8>);

impl Secret {
    #[must_use]
    pub fn from_utf8(value: impl Into<String>) -> Self {
        Self(value.into().into_bytes())
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// UTF-8 视图；私钥等二进制材料不保证是 UTF-8。
    pub fn as_utf8(&self) -> Result<Cow<'_, str>, CredentialError> {
        std::str::from_utf8(&self.0)
            .map(Cow::Borrowed)
            .map_err(|_| CredentialError::NotUtf8)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 稳定占位符：即使有人把 secret 误打进日志，也只泄露「它存在」。
        f.write_str("Secret(<redacted>)")
    }
}

/// `keychain://<service>/<account>` —— SQLite 里唯一允许存在的凭据引用形态。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CredentialRef {
    service: String,
    account: String,
}

impl CredentialRef {
    #[must_use]
    pub fn new(service: impl Into<String>, account: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            account: account.into(),
        }
    }

    pub fn parse(reference: &str) -> Result<Self, CredentialError> {
        let rest = reference
            .strip_prefix("keychain://")
            .ok_or_else(|| CredentialError::InvalidReference(reference.to_string()))?;
        let (service, account) = rest
            .split_once('/')
            .ok_or_else(|| CredentialError::InvalidReference(reference.to_string()))?;
        if service.is_empty()
            || account.is_empty()
            || service.contains('/')
            || account.contains('/')
        {
            return Err(CredentialError::InvalidReference(reference.to_string()));
        }
        Ok(Self {
            service: service.to_string(),
            account: account.to_string(),
        })
    }

    #[must_use]
    pub fn to_string_ref(&self) -> String {
        format!("keychain://{}/{}", self.service, self.account)
    }

    #[must_use]
    pub fn service(&self) -> &str {
        &self.service
    }

    #[must_use]
    pub fn account(&self) -> &str {
        &self.account
    }
}

impl fmt::Display for CredentialRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_string_ref())
    }
}

/// 凭据存储的统一视图。实现必须满足：
/// - `set` 返回的引用可以被 `get`/`delete` 原样使用；
/// - `get` 对不存在的引用返回 [`CredentialError::NotFound`]；
/// - 错误信息不得携带 secret 内容。
pub trait CredentialStore: Send + Sync {
    /// 存入并在 OS store 内定位；返回的 `CredentialRef` 即持久化到 SQLite 的引用。
    fn set(
        &self,
        service: &str,
        account: &str,
        secret: &Secret,
    ) -> Result<CredentialRef, CredentialError>;

    fn get(&self, reference: &CredentialRef) -> Result<Secret, CredentialError>;

    /// 回收：删除后该引用即失效；已不存在时按成功处理（幂等回收）。
    fn delete(&self, reference: &CredentialRef) -> Result<(), CredentialError>;

    fn has(&self, reference: &CredentialRef) -> Result<bool, CredentialError>;
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("credential `{reference}` not found")]
    NotFound { reference: String },
    #[error("malformed credential reference `{0}`")]
    InvalidReference(String),
    #[error("credential store backend error: {0}")]
    Backend(String),
    #[error("secret is not valid UTF-8")]
    NotUtf8,
    #[error("os credential store is unavailable on this platform: {0}")]
    Unavailable(String),
}
