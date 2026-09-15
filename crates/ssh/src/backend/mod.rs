//! russh implementation of `SshBackend`（ADR 0002）。
//!
//! russh 类型严格封闭在本模块：trait 边界之上只有 `crates/ssh` 自己的类型。
//! 连接前的 known_hosts 预检 + 认证期密钥校验双重把关：主机指纹未知（RequireMatch）
//! 或与已存指纹不一致时拒绝连接；TOFU 策略下首次连接成功后才落盘记录。
//!
//! 认证路径（password / private key / 加密 private key / OpenSSH 用户证书 /
//! ssh-agent）全部落在本模块：材料问题一律以 `crate::PrivateKeyError` /
//! `AgentError` / `CertificateError` 报出，并且**一种方式失败绝不静默改试另一种**
//! —— 「agent 没起来」必须看起来就是「agent 没起来」。
//!
//! 本模块拆成一组按关注点划分的子模块，这里只留 [`RusshBackend`] 本身的构造、
//! known_hosts 记账与建连/探针/关闭三个 trait 入口：
//!
//! - `auth`：密码 / 私钥 / 口令 / 用户证书四条认证材料路径；
//! - `agent`：ssh-agent 的连接、身份遍历与错误分类；
//! - `hostkey`：known_hosts 预检、握手、指纹表示与两个 `client::Handler`；
//! - `command`：一次性命令执行与有界输出；
//! - `pty`：PTY 通道打开与数据流；
//! - `sftp`：SFTP 句柄与读写；
//! - `error`：`russh::Error` / `HandshakeError` → [`crate::Error`] 的映射。
//!
//! `establish` 与 `ConnHandler` 由本模块转出（`pub(crate) use`），因为 `conn` 要按名字
//! 引用它们；其余子模块的自由函数仍在 `backend` 内部可见。

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use russh::client::Handle;

use crate::conn::SessionHandle;
use crate::known_hosts::{Check, ForgetOutcome, KnownHostsError, KnownHostsStore, TrustDecision};
use crate::{ConnectionSecrets, Error, HostKeyProbe, Result, Session, SshBackend, SshConfig};

mod agent;
mod auth;
mod command;
mod error;
mod hostkey;
mod pty;
mod sftp;
#[cfg(test)]
pub(crate) mod test_support;

pub(crate) use hostkey::{establish, ConnHandler};
/// SFTP 文件属性与有守卫替换的公开形状（[`crate::SftpFileStat`] 等）。
pub use sftp::{
    link_count_probe_command, parse_link_count, shell_single_quote, SftpEntryKind, SftpFileStat,
    SftpReplaceError, SftpReplaceGuard, SftpReplacement,
};

/// TCP/握手超时；认证另有普通或交互式上限，避免 MFA 等待被握手预算截断。
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const AUTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
pub(super) const INTERACTIVE_AUTH_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(150);

pub struct RusshBackend {
    known_hosts: Arc<StdMutex<KnownHostsStore>>,
    token: AtomicU64,
}

impl RusshBackend {
    #[must_use]
    pub fn new(known_hosts: Arc<StdMutex<KnownHostsStore>>) -> Self {
        Self {
            known_hosts,
            token: AtomicU64::new(1),
        }
    }

    /// 从数据目录加载 known_hosts 存储（不存在 = 空存储，不报错）。
    pub fn from_data_dir(
        data_dir: &Path,
    ) -> std::result::Result<Self, crate::known_hosts::KnownHostsError> {
        let store = KnownHostsStore::load(data_dir.join("known_hosts"))?;
        Ok(Self::new(Arc::new(StdMutex::new(store))))
    }

    #[must_use]
    pub fn known_hosts(&self) -> &Arc<StdMutex<KnownHostsStore>> {
        &self.known_hosts
    }

    /// 借出 known_hosts store（锁被毒化时**报错**，不 panic、也不退化成一个假答案）。
    ///
    /// 毒化只在「持锁时 panic」之后发生，而这里持锁的每一段都是纯内存操作、没有 panic
    /// 点；真发生了也绝不能答「没有钉子」—— 那会让 `RequireMatch` 下的主机看起来可以
    /// 直接信任，而这正是这个 crate 最不该出的错。
    fn known_hosts_store(
        &self,
    ) -> std::result::Result<std::sync::MutexGuard<'_, KnownHostsStore>, KnownHostsError> {
        self.known_hosts.lock().map_err(|_| {
            KnownHostsError::Io(
                "known_hosts lock poisoned".into(),
                std::io::Error::other("poisoned"),
            )
        })
    }

    /// 这台 `host:port` 当前钉住的指纹（`None` = 还没有钉子）。
    ///
    /// 界面「打开就把当前状态画出来」用的就是它，因此它**不触网**。
    pub fn host_key_pin(
        &self,
        host: &str,
        port: u16,
    ) -> std::result::Result<Option<String>, KnownHostsError> {
        Ok(self.known_hosts_store()?.pinned_fingerprint(host, port))
    }

    /// 服务器**出示**的指纹与当前钉子的关系：`unpinned` / `matches` / `mismatch`。
    ///
    /// 比较本身在 store 里（[`KnownHostsStore::check`]），这里只是借锁 —— 之所以要把它
    /// 做成后端方法，是为了让「锁怎么用」只有一处（见 [`Self::known_hosts_store`]），
    /// 而不是让每个命令各自 `lock()` 一遍再各自想一遍中毒了怎么办。
    pub fn host_key_check(
        &self,
        host: &str,
        port: u16,
        presented: &str,
    ) -> std::result::Result<Check, KnownHostsError> {
        Ok(self.known_hosts_store()?.check(host, port, presented))
    }

    /// 把用户**确认过**的指纹钉进 `known_hosts` 并落盘。
    ///
    /// 与 [`KnownHostsStore::register`] 的区别是这条路**会拒绝**：已钉着另一个指纹时返回
    /// [`TrustDecision::RefusedDifferentPin`]，且不写任何东西（ADR 0012 第 5 条 ——
    /// 变更一个钉子必须由用户先遗忘、再重新确认，没有任何「不一致时仍然继续」的捷径）。
    ///
    /// # 这个方法原来没有调用者，现在有了
    ///
    /// 它原先的说明是「供 UI 的『信任这台主机』动作使用」—— 而那个动作当时并不存在：
    /// 全仓库搜不到第二处 `trust_host`，桌面端也完全不知道 host key 或指纹。所以那段
    /// 说明描述的是一个**计划中的**调用点，而不是一个事实。
    ///
    /// 现在它是 `commands/host_key.rs` 的 `server_host_key_trust` 的实际后端：界面上的
    /// 「信任此指纹」按钮把它连起来了，而界面要的答案（新建了钉子 / 本来就是同一个 /
    /// 被拒绝）就是这里的返回值，不是 `Result<(), _>` 能表达的东西。
    pub fn trust_host(
        &self,
        host: &str,
        port: u16,
        fingerprint: &str,
    ) -> std::result::Result<TrustDecision, KnownHostsError> {
        self.known_hosts_store()?.trust(host, port, fingerprint)
    }

    /// 删除这台 `host:port` 的 pin（并落盘），下一次连接回到 TOFU。
    ///
    /// 返回值区分「删掉了一条」与「本来就没有」：重复点击「遗忘」不该报错，但界面也不该
    /// 声称刚刚删掉了一个并不存在的钉子。
    pub fn forget_host(
        &self,
        host: &str,
        port: u16,
    ) -> std::result::Result<ForgetOutcome, KnownHostsError> {
        self.known_hosts_store()?.forget(host, port)
    }

    fn next_session_id(&self) -> String {
        let n = self.token.fetch_add(1, Ordering::Relaxed);
        format!("ses_{}_{}", std::process::id(), n)
    }
}

/// 唯一一处 `SshBackend` 实现（trait impl 不能拆成多个块），每个方法只做转发，
/// 真正的逻辑在对应子模块里。
impl SshBackend for RusshBackend {
    async fn connect(&self, config: SshConfig, secrets: ConnectionSecrets) -> Result<Session> {
        let session_id = self.next_session_id();
        let conn = establish(&config, &secrets, &self.known_hosts).await?;

        Ok(Session {
            session_id: session_id.clone(),
            server_id: config.server_id.clone(),
            inner: Arc::new(SessionHandle::new(
                conn,
                config.clone(),
                secrets,
                Arc::clone(&self.known_hosts),
            )),
        })
    }

    async fn probe_host_key(&self, host: &str, port: u16) -> Result<HostKeyProbe> {
        // 与建连同一个上限：探针是一次真连接，不能比连接本身更没边界。
        tokio::time::timeout(CONNECT_TIMEOUT, hostkey::probe_server_key(host, port))
            .await
            .map_err(|_| Error::Timeout)?
    }

    async fn execute(
        &self,
        session: &Session,
        command: &str,
        timeout: Option<std::time::Duration>,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<crate::CommandResult> {
        command::execute(session, command, timeout, cancel).await
    }

    async fn open_pty(&self, session: &Session, size: (u16, u16)) -> Result<crate::PtySession> {
        pty::open_pty(self, session, size).await
    }

    async fn sftp(&self, session: &Session) -> Result<crate::SftpClient> {
        sftp::sftp(session).await
    }

    async fn pty_write(&self, pty: &crate::PtySession, data: &[u8]) -> Result<()> {
        pty::pty_write(pty, data)
    }

    async fn pty_resize(&self, pty: &crate::PtySession, cols: u16, rows: u16) -> Result<()> {
        pty::pty_resize(pty, cols, rows)
    }

    fn pty_output(&self, pty: &crate::PtySession) -> tokio::sync::mpsc::Receiver<crate::PtyEvent> {
        pty::pty_output(pty)
    }

    async fn pty_close(&self, pty: &crate::PtySession) -> Result<()> {
        pty::pty_close(pty)
    }

    async fn close(&self, session: &Session) -> Result<()> {
        session.inner.close().await
    }
}

/// 包裹一次"transport 断开 → 重连 → 重试"：只对 transport 类错误重试，认证 /
/// 校验 / 参数错误不重试。
pub(super) async fn retry_transport_async<T, F, Fut>(
    session: &Session,
    op: F,
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> Result<T>
where
    F: Fn(Arc<Handle<ConnHandler>>) -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
    T: Send,
{
    let mut attempt = 0;
    loop {
        if cancel.is_some_and(|token| token.is_cancelled()) {
            return Err(Error::Cancelled);
        }
        let conn = session.inner.conn.lock().await.clone();
        let result = op(conn).await;
        match result {
            Err(Error::Transport(_)) if attempt == 0 => {
                if cancel.is_some_and(|token| token.is_cancelled()) {
                    return Err(Error::Cancelled);
                }
                session.inner.reconnect().await?;
                attempt += 1;
            }
            other => return other,
        }
    }
}
