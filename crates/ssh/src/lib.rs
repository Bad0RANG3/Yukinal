//! yukinal-ssh — 可替换的 SSH backend。
//!
//! 目标：先把 trait 形状与跨边界数据类型钉死，具体实现走 russh（ADR 0002）。
//!
//! 注意 `async fn` 的拼写：这里用 `impl Future<...> + Send`，因为它与
//! `tokio::spawn` 兼容，也允许将来换成 `dyn SshBackend`。等实现落地时再确定 backend 是否
//! 需要 dyn 分发时再决定是否引入 `async-trait`。
//!
//! Secret 处理：`SshConfig` 只携带 `credential_ref`，私钥/口令由 `yukinal-credentials`
//! 在使用点解析（-R4）。

#![allow(dead_code)] // day-0 contract skeleton

use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    /// TCP / transport / handshake 层失败。
    Transport(String),
    /// 认证失败（密码错误、key 不可用、方法不被接受）。
    Authentication(String),
    /// Host key 与 known_hosts 不匹配（：必须中断，不能静默接受）。
    HostKeyVerification { fingerprint: String },
    /// Channel / session 层失败。
    Channel(String),
    /// 命令或连接超时。
    Timeout,
    /// 用户取消。
    Cancelled,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Transport(message) => write!(f, "ssh transport error: {message}"),
            Error::Authentication(message) => write!(f, "ssh authentication failed: {message}"),
            Error::HostKeyVerification { fingerprint } => {
                write!(f, "unverified host key (fingerprint {fingerprint})")
            }
            Error::Channel(message) => write!(f, "ssh channel error: {message}"),
            Error::Timeout => write!(f, "ssh operation timed out"),
            Error::Cancelled => write!(f, "ssh operation cancelled"),
        }
    }
}

impl std::error::Error for Error {}

/// 认证方式（MVP：Password + Private Key，）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authentication {
    Password {
        credential_ref: String,
    },
    PrivateKey {
        credential_ref: String,
        passphrase_ref: Option<String>,
    },
    /// 后续：ssh-agent / certificate / keyboard-interactive。
    Agent,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum KnownHostsPolicy {
    /// 默认必须匹配已知 host key。
    #[default]
    RequireMatch,
    /// 首次连接信任，之后必须匹配。UI 必须明确告知用户。
    TrustOnFirstUse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshConfig {
    /// 稳定 serverId：任何一次连接都必须绑定它，
    /// 不允许用自然语言（"production"）定位目标。
    pub server_id: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub authentication: Authentication,
    pub known_hosts_policy: KnownHostsPolicy,
    /// 0 = 关闭 keepalive。
    pub keepalive_interval_secs: u16,
}

/// 一次已建立的连接。`Clone` 得到的是同一连接句柄的引用计数副本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub session_id: String,
    pub server_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandResult {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl CommandResult {
    pub fn stdout_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

/// 远端 PTY。字节流通过 `yukinal-terminal` 以 event 形式推给 xterm.js。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtySession {
    pub pty_id: String,
    pub server_id: String,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SftpClient {
    pub session_id: String,
}

/// SSH backend 必须可替换（russh 默认实现，允许后续 openssh-compat）。
pub trait SshBackend {
    fn connect(
        &self,
        config: SshConfig,
    ) -> impl std::future::Future<Output = Result<Session>> + Send;

    fn execute(
        &self,
        session: &Session,
        command: &str,
        timeout: Option<std::time::Duration>,
    ) -> impl std::future::Future<Output = Result<CommandResult>> + Send;

    fn open_pty(
        &self,
        session: &Session,
        size: (u16, u16),
    ) -> impl std::future::Future<Output = Result<PtySession>> + Send;

    fn sftp(
        &self,
        session: &Session,
    ) -> impl std::future::Future<Output = Result<SftpClient>> + Send;
}
