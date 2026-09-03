//! yukinal-ssh — 可替换的 SSH backend（ADR 0002：russh）。
//!
//! 边界：
//! - 上层只能看到本 crate 的类型（`SshConfig` / `Session` / `CommandResult` / …），
//!   russh 类型绝不跨出 `backend` 模块。
//! - `SshConfig` / `Authentication` 只携带 `credential_ref`；secret 材料由调用方
//!   （Rust core，使用点）经 `yukinal-credentials` 解析后以 [`ConnectionSecrets`]
//!   传入 —— 本 crate 不依赖 credentials crate，也从不解析引用。
//! - 超时 / 取消映射到 [`Error::Timeout`] / [`Error::Cancelled`]；host key 不匹配
//!   必须报 [`Error::HostKeyVerification`]，绝不静默接受。

#![allow(dead_code)] // 契约先行；S06/S09 之前部分方法尚未被上层调用

pub mod backend;
mod conn;
pub mod known_hosts;

pub use backend::RusshBackend;

use std::fmt;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::conn::SessionHandle;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    /// TCP / transport / handshake 层失败。
    Transport(String),
    /// 认证失败（密码 / key 不可用、方法不被接受、加密 key 未支持）。
    Authentication(String),
    /// Host key 不在 known_hosts（首次连接需显式信任），或与已存的指纹不一致
    /// （MITM 指示，必须中断）。
    HostKeyVerification { host: String, fingerprint: String },
    /// Channel / session 层失败。
    Channel(String),
    /// 命令或连接超时。
    Timeout,
    /// 用户取消。
    Cancelled,
    /// 配置非法（端口、参数组合等）。
    Configuration(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Transport(message) => write!(f, "ssh transport error: {message}"),
            Error::Authentication(message) => write!(f, "ssh authentication failed: {message}"),
            Error::HostKeyVerification { host, fingerprint } => {
                write!(
                    f,
                    "host key verification failed for {host} (fingerprint {fingerprint})"
                )
            }
            Error::Channel(message) => write!(f, "ssh channel error: {message}"),
            Error::Timeout => write!(f, "ssh operation timed out"),
            Error::Cancelled => write!(f, "ssh operation cancelled"),
            Error::Configuration(message) => write!(f, "ssh configuration error: {message}"),
        }
    }
}

impl std::error::Error for Error {}

/// 认证方式（MVP：Password + Private Key；。秘密只以引用出现）。
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

/// 连接时由使用点解析出的 secret 材料。短暂存在，绝不持久化、绝不进日志
/// （本类型没有能打印内容的 `Debug`/`Display`）。
#[derive(Clone)]
pub struct ConnectionSecrets {
    pub password: Option<String>,
    pub private_key_pem: Option<String>,
    pub private_key_passphrase: Option<String>,
}

impl fmt::Debug for ConnectionSecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ConnectionSecrets(<redacted>)")
    }
}

impl ConnectionSecrets {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            password: None,
            private_key_pem: None,
            private_key_passphrase: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum KnownHostsPolicy {
    /// 默认：host key 必须是 known_hosts 里已有的；未知主机直接失败，由 UI
    /// 显式决定是否信任。
    #[default]
    RequireMatch,
    /// 首次连接信任并记录，之后必须匹配。UI 必须明确告知用户。
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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandResult {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl CommandResult {
    #[must_use]
    pub fn stdout_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    #[must_use]
    pub fn stderr_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }

    #[must_use]
    pub fn combined_lossy(&self) -> String {
        let mut out = String::from_utf8_lossy(&self.stdout).into_owned();
        if !self.stderr.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&String::from_utf8_lossy(&self.stderr));
        }
        out
    }
}

/// 一次已建立的连接（引用计数句柄；多个 Clone 指向同一连接）。
pub struct Session {
    pub session_id: String,
    pub server_id: String,
    pub(crate) inner: Arc<SessionHandle>,
}

impl Clone for Session {
    fn clone(&self) -> Self {
        Self {
            session_id: self.session_id.clone(),
            server_id: self.server_id.clone(),
            inner: Arc::clone(&self.inner),
        }
    }
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("session_id", &self.session_id)
            .field("server_id", &self.server_id)
            .finish()
    }
}

/// 远端 PTY（xterm.js 的数据流由 `yukinal-terminal` 经 events 转发）。
pub struct PtySession {
    pub pty_id: String,
    pub server_id: String,
    pub cols: u16,
    pub rows: u16,
    pub(crate) inner: Arc<crate::conn::PtyHandle>,
}

impl fmt::Debug for PtySession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PtySession")
            .field("pty_id", &self.pty_id)
            .field("server_id", &self.server_id)
            .field("cols", &self.cols)
            .field("rows", &self.rows)
            .finish()
    }
}

/// 远端字节流事件（PTY 输出或 stderr）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyEvent {
    Output(Vec<u8>),
    Closed { code: Option<u32> },
}

pub struct SftpClient {
    pub session_id: String,
    pub server_id: String,
    pub(crate) inner: Arc<crate::conn::SftpHandle>,
}

impl fmt::Debug for SftpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SftpClient")
            .field("session_id", &self.session_id)
            .field("server_id", &self.server_id)
            .finish()
    }
}

/// SSH backend 必须可替换（russh 默认实现；未来可换 openssh-compat）。
pub trait SshBackend {
    /// 建立连接并完成认证。`secrets` 由使用点解析（见 [`ConnectionSecrets`]）。
    fn connect(
        &self,
        config: SshConfig,
        secrets: ConnectionSecrets,
    ) -> impl std::future::Future<Output = Result<Session>> + Send;

    /// 执行一次性命令；`cancel` 触发时返回 [`Error::Cancelled`] 并关闭通道，
    /// `None` 超时 = 不设上限。想跳过取消的调用方传一个全新 token。
    fn execute(
        &self,
        session: &Session,
        command: &str,
        timeout: Option<std::time::Duration>,
        cancel: &CancellationToken,
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

    // -- PTY 数据流 -----------------------------------------------------------

    fn pty_write(
        &self,
        pty: &PtySession,
        data: &[u8],
    ) -> impl std::future::Future<Output = Result<()>> + Send;

    fn pty_resize(
        &self,
        pty: &PtySession,
        cols: u16,
        rows: u16,
    ) -> impl std::future::Future<Output = Result<()>> + Send;

    /// 订阅该 PTY 的输出 / 关闭事件（每会话一个订阅者）。
    fn pty_output(&self, pty: &PtySession) -> mpsc::UnboundedReceiver<PtyEvent>;

    /// 关闭会话（终止 keepalive 与连接）。
    fn close(&self, session: &Session) -> impl std::future::Future<Output = Result<()>> + Send;
}
