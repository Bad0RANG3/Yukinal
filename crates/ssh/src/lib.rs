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
//! - 认证材料的问题各自成套上报（[`Error::PrivateKey`] / [`Error::Agent`] /
//!   [`Error::Certificate`]），并且一种方式失败后**绝不**静默改试另一种：
//!   「agent 没起来」不能表现成「密码不对」。

// 这里原来有一行 `#![allow(dead_code)]`，理由是「契约先行；终端/服务器工具落地前
// 部分方法尚未被上层调用」。那句话描述的其实是 `pub` 项 —— 而库 crate 里的 `pub`
// 项本来就不会触发 dead_code（外部可达），所以这个 allow 从头到尾没有遮住它声称
// 要遮的东西。它实际遮住的只有两处 `pub(crate)`：`SessionHandle::keepalive_task`
// 与 `SftpHandle::new`，两处都已按各自的真实情况处理（见 conn.rs）。
//
// 拿掉它是有代价意识的选择：留着等于让整个 crate 的私有代码失去死代码检查，
// 而这个 crate 的私有部分正是连接生命周期与 PTY/SFTP 句柄这些最容易留下残骸的
// 地方。契约先行的 `pub` 接口不需要靠 lint 豁免来存在。

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
    /// 服务器**拒绝**了这次认证（密码不对、key 不被接受）。
    ///
    /// 「本地的材料有问题」不走这里 —— 那是 [`Error::PrivateKey`] /
    /// [`Error::Agent`] / [`Error::Certificate`]。分开的理由是这两类失败对用户的
    /// 含义完全不同：一个要改服务器上的授权，另一个要改本机的 key 或 agent。
    Authentication(String),
    /// 私钥材料本身不可用（读不出 / 需要口令 / 口令不对）。
    PrivateKey(PrivateKeyError),
    /// ssh-agent 不可达、没有可用身份、或拒绝签名。绝不在 agent 失败后静默换成
    /// 别的认证方式：那会让「agent 没起来」表现成「密码不对」。
    Agent(AgentError),
    /// OpenSSH 用户证书不可用（路径定不下来 / 读不出 / 解析失败 / 与私钥不配对）。
    Certificate(CertificateError),
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
            Error::PrivateKey(error) => write!(f, "ssh private key error: {error}"),
            Error::Agent(error) => write!(f, "ssh-agent error: {error}"),
            Error::Certificate(error) => write!(f, "ssh certificate error: {error}"),
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

/// 私钥材料不可用的具体原因。
///
/// 这四种必须彼此可分，因为调用点要做的事完全不同：需要口令时要去问用户，
/// 口令不对时要重新问，key 本来没加密时是调用点传错了参数，读不出来则是文件或
/// 格式的问题。一句「认证失败」把这四条路合并成一条，用户就只能靠猜。
///
/// 注意：**任何变体都不携带口令本身**，`reason` 一律来自底层解析器的措辞
/// （`ssh-key` 的错误里不含口令），所以这里可以安全地进日志和界面。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PrivateKeyError {
    /// PEM 解析失败（格式不对、被截断、算法不支持）。
    #[error("cannot parse the private key: {reason}")]
    Unreadable { reason: String },
    /// key 是加密的，而调用点没有解析出口令。
    #[error("the private key is encrypted and no passphrase was resolved at the call site")]
    PassphraseRequired,
    /// 给了非空口令，但这把 key 根本没有加密 —— 调用点配错了，不是口令错了。
    #[error("a passphrase was supplied but this private key is not encrypted")]
    NotEncrypted,
    /// key 是加密的，但给出的口令解不开它（口令错误，或密文已损坏 ——
    /// `ssh-key` 对这两种情况都返回同一个加密错误，这里也无法区分）。
    #[error("the supplied passphrase did not decrypt this private key")]
    PassphraseRejected,
}

/// ssh-agent 失败的具体原因。
///
/// `detail` 里必须出现**我们连的是什么**（`SSH_AUTH_SOCK`、OpenSSH 的命名管道、
/// Pageant）以及底层错误，否则界面只能说「agent 出错」，用户不知道该启动谁。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentError {
    /// 找不到或连不上 agent。
    #[error("cannot reach {detail}")]
    Unavailable { detail: String },
    /// 连上了，但 agent 里没有任何身份（`ssh-add` 没跑，或者 key 已被移除）。
    #[error("the ssh-agent holds no identities")]
    NoIdentities,
    /// 通讯正常，但 agent 拒绝为某个身份签名（身份被移除、agent 被锁、或该身份
    /// 带确认约束）。
    #[error("the ssh-agent refused to sign: {detail}")]
    SigningRejected { detail: String },
    /// agent 交换在签名阶段失败，且底层原因无法在类型上细分（见 `backend.rs` 的
    /// `map_agent_sign_error`：`AgentAuthError` 在 russh 的私有模块里）。
    #[error("the ssh-agent exchange failed while signing: {detail}")]
    SigningFailed { detail: String },
    /// agent 的应答不符合 agent 协议。
    #[error("the ssh-agent reply is not valid agent protocol: {detail}")]
    Protocol { detail: String },
    /// 服务器拒绝了 agent 持有的**全部**身份。
    ///
    /// `partial_success` 为真表示服务器认可了某个身份但还要求第二种认证 ——
    /// 这仍然是我们无法完成的登录，但和「没有一把 key 被接受」不是同一件事。
    #[error("the server rejected all {identities_tried} identities offered by the ssh-agent")]
    Rejected {
        identities_tried: usize,
        partial_success: bool,
    },
}

/// OpenSSH 用户证书不可用的具体原因。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CertificateError {
    /// 既没给证书路径，也没给可以推导 `-cert.pub` sibling 的私钥路径。
    #[error(
        "cannot locate the certificate: supply certificate_path, or private_key_path to derive \
         the `<private-key-path>-cert.pub` sibling"
    )]
    PathUndetermined,
    /// 证书文件读不出来（不存在、权限、不是 UTF-8 的文本）。
    #[error("cannot read the certificate at {path}: {detail}")]
    Read { path: String, detail: String },
    /// 文件读到了，但不是 OpenSSH 证书（例如误指到公钥或私钥文件）。
    #[error("cannot parse the certificate at {path}: {detail}")]
    Parse { path: String, detail: String },
    /// 证书里的公钥与同时提供的私钥不是一对。
    ///
    /// 在本地拦住而不是让服务器报「key 不对」：这一对发出去服务器只会说认证失败，
    /// 而这里能直接告诉调用点「你把 A 的证书和 B 的私钥配在一起了」。
    #[error("the certificate does not certify the private key it was offered with")]
    KeyMismatch,
}

/// 认证方式。秘密只以引用出现（材料由调用点解析后经 [`ConnectionSecrets`] 传入）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authentication {
    Password {
        credential_ref: String,
    },
    PrivateKey {
        credential_ref: String,
        /// 加密私钥的**口令引用**（不是口令本身）：口令材料经
        /// [`ConnectionSecrets::private_key_passphrase`] 传入。`None` 表示这把 key
        /// 不带口令 —— 若实际材料是加密的，会得到 [`PrivateKeyError::PassphraseRequired`]。
        passphrase_ref: Option<String>,
    },
    /// OpenSSH **用户证书**认证（`ssh-keygen -s` 签发的 `*-cert.pub`）。
    ///
    /// 私钥材料与 [`Authentication::PrivateKey`] 同源（仍走
    /// `ConnectionSecrets::private_key_pem`）；证书是**公开**材料，所以从路径读取，
    /// 这也让 sibling 推导成为可能。OpenSSH 的约定是把证书放在私钥旁边、名字加上
    /// `-cert.pub`（`ssh-keygen -s`、`ssh-add`、`ssh -i` 都按它配对），但那是**约定**
    /// 而非协议要求，所以推导不出时显式报错，绝不退化成「用裸 key 认证」。
    Certificate {
        credential_ref: String,
        passphrase_ref: Option<String>,
        /// 私钥文件路径。只用来推导 sibling —— 私钥材质不从磁盘读。
        private_key_path: Option<String>,
        /// 显式证书路径。`Some` 时优先，不再推导 sibling。
        certificate_path: Option<String>,
    },
    /// ssh-agent 认证：连上运行中的 agent，把它持有的身份逐个交给服务器。
    Agent {
        /// 显式 socket / 命名管道路径；`None` = 按平台约定发现
        /// （Unix：`SSH_AUTH_SOCK`；Windows：OpenSSH 命名管道，其次 Pageant）。
        socket_path: Option<String>,
    },
}

/// 连接时由使用点解析出的 secret 材料。短暂存在，绝不持久化、绝不进日志
/// （本类型没有能打印内容的 `Debug`/`Display`）。
///
/// `private_key_passphrase` 只在一次认证调用里被借用（`backend::load_private_key`）；
/// 解密出来的私钥不会跟着 `Session` 存活 —— 重连时用这里的材料重新解一次。
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
    ///
    /// 返回的是**有界**接收端：远端产出快于消费时，生产端会在 `send().await` 上挂起，
    /// 把背压经 russh 传回远端，而不是在宿主进程里无限堆积。容量见
    /// `conn::PTY_OUTPUT_CAPACITY`。
    fn pty_output(&self, pty: &PtySession) -> mpsc::Receiver<PtyEvent>;

    /// Close one PTY channel without tearing down the shared SSH session.
    fn pty_close(&self, pty: &PtySession) -> impl std::future::Future<Output = Result<()>> + Send;

    /// 关闭会话（终止 keepalive 与连接）。
    fn close(&self, session: &Session) -> impl std::future::Future<Output = Result<()>> + Send;
}
