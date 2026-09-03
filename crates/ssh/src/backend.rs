//! russh implementation of `SshBackend`（ADR 0002）。
//!
//! russh 类型严格封闭在本模块：trait 边界之上只有 `crates/ssh` 自己的类型。
//! 连接前的 known_hosts 预检 + 认证期密钥校验双重把关：主机指纹未知（RequireMatch）
//! 或与已存指纹不一致时拒绝连接；TOFU 策略下首次连接成功后才落盘记录。

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use russh::client::{self, Handle};
use russh::keys::{ssh_key, HashAlg, PublicKeyOrCertificate};
use russh::{Channel, ChannelMsg, Pty};

use crate::conn::{PtyHandle, SessionHandle, SftpHandle};
use crate::known_hosts::{Check, KnownHostsStore};
use crate::{
    Authentication, CommandResult, ConnectionSecrets, Error, PtyEvent, PtySession, Result, Session,
    SftpClient, SshBackend, SshConfig,
};

/// 建连 + 认证整体超时（硬性兜底，不让 UI 卡在握手）。
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

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

    /// 供 UI 的"信任这台主机"动作使用：把当前指纹钉进 known_hosts 并落盘。
    pub fn trust_host(
        &self,
        host: &str,
        port: u16,
        fingerprint: &str,
    ) -> std::result::Result<(), crate::known_hosts::KnownHostsError> {
        let mut store = self.known_hosts.lock().map_err(|_| {
            crate::known_hosts::KnownHostsError::Io(
                "poisoned lock".into(),
                std::io::Error::other("poisoned"),
            )
        })?;
        store.register(host, port, fingerprint)
    }

    /// SFTP 冒烟操作（S13 之前证明子系统真实可用）：远端目录清单。
    pub async fn sftp_list_dir(&self, client: &SftpClient, path: &str) -> Result<Vec<String>> {
        let sftp = lock_sftp(client).await?;
        let dir = sftp
            .read_dir(path)
            .await
            .map_err(|error| Error::Channel(error.to_string()))?;
        Ok(dir.map(|entry| entry.file_name()).collect())
    }

    /// SFTP 读整文件（S13 filesystem.read 的基础操作）。
    pub async fn sftp_read_file(&self, client: &SftpClient, path: &str) -> Result<Vec<u8>> {
        use russh_sftp::protocol::OpenFlags;
        use tokio::io::AsyncReadExt;
        let sftp = lock_sftp(client).await?;
        let mut file = sftp
            .open_with_flags(path, OpenFlags::READ)
            .await
            .map_err(|error| Error::Channel(error.to_string()))?;
        let mut out = Vec::new();
        file.read_to_end(&mut out)
            .await
            .map_err(|error| Error::Channel(error.to_string()))?;
        Ok(out)
    }

    fn next_session_id(&self) -> String {
        let n = self.token.fetch_add(1, Ordering::Relaxed);
        format!("ses_{}_{}", std::process::id(), n)
    }

    fn next_pty_id(&self) -> String {
        static PTY_TOKEN: AtomicU64 = AtomicU64::new(1);
        format!(
            "pty_{}_{}",
            std::process::id(),
            PTY_TOKEN.fetch_add(1, Ordering::Relaxed)
        )
    }
}

impl SshBackend for RusshBackend {
    async fn connect(&self, config: SshConfig, secrets: ConnectionSecrets) -> Result<Session> {
        let session_id = self.next_session_id();
        let conn = tokio::time::timeout(
            CONNECT_TIMEOUT,
            establish(&config, &secrets, &self.known_hosts),
        )
        .await
        .map_err(|_| Error::Timeout)??;

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

    async fn execute(
        &self,
        session: &Session,
        command: &str,
        timeout: Option<std::time::Duration>,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<CommandResult> {
        retry_transport_async(session, |conn| run_command(conn, command, timeout, cancel)).await
    }

    async fn open_pty(&self, session: &Session, size: (u16, u16)) -> Result<PtySession> {
        let (cols, rows) = size;
        let channel =
            retry_transport_async(session, |conn| open_pty_channel(conn, cols, rows)).await?;
        let pty_id = self.next_pty_id();

        let (pty, mut commands_rx) = PtyHandle::new();
        let output_tx = pty.output_tx.clone();

        // 单一任务持有完整 `Channel`：对外转发远端输出，对内消费写入/改尺寸命令。
        tokio::spawn(async move {
            let mut channel = channel;
            loop {
                tokio::select! {
                    command = commands_rx.recv() => {
                        let Some(command) = command else { break; };
                        match command {
                            crate::conn::PtyCmd::Write(data) => {
                                if channel.data_bytes(data).await.is_err() {
                                    break;
                                }
                            }
                            crate::conn::PtyCmd::Resize(cols, rows) => {
                                let _ = channel
                                    .window_change(u32::from(cols), u32::from(rows), 0, 0)
                                    .await;
                            }
                        }
                    }
                    message = channel.wait() => {
                        match message {
                            None => break,
                            Some(ChannelMsg::Data { data }) => {
                                if output_tx.send(PtyEvent::Output(data.to_vec())).is_err() {
                                    break; // 订阅者退出 = 终端已关
                                }
                            }
                            Some(ChannelMsg::ExtendedData { data, ext: 1 }) => {
                                if output_tx.send(PtyEvent::Output(data.to_vec())).is_err() {
                                    break;
                                }
                            }
                            Some(ChannelMsg::ExitStatus { exit_status }) => {
                                let _ = output_tx.send(PtyEvent::Closed { code: Some(exit_status) });
                                break;
                            }
                            Some(ChannelMsg::Close | ChannelMsg::Eof) => {
                                let _ = output_tx.send(PtyEvent::Closed { code: None });
                                break;
                            }
                            Some(_) => {}
                        }
                    }
                }
            }
        });

        Ok(PtySession {
            pty_id,
            server_id: session.server_id.clone(),
            cols,
            rows,
            inner: Arc::new(pty),
        })
    }

    async fn sftp(&self, session: &Session) -> Result<SftpClient> {
        let sftp = retry_transport_async(session, |conn| async move {
            let channel = conn.channel_open_session().await.map_err(map_send_err)?;
            channel
                .request_subsystem(true, "sftp")
                .await
                .map_err(map_send_err)?;
            let stream = channel.into_stream();
            russh_sftp::client::SftpSession::new(stream)
                .await
                .map_err(|error| Error::Channel(format!("sftp handshake failed: {error}")))
        })
        .await?;
        Ok(SftpClient {
            session_id: session.session_id.clone(),
            server_id: session.server_id.clone(),
            inner: Arc::new(SftpHandle::new_some(Arc::new(sftp))),
        })
    }

    async fn pty_write(&self, pty: &PtySession, data: &[u8]) -> Result<()> {
        pty.inner
            .commands
            .send(crate::conn::PtyCmd::Write(data.to_vec()))
            .map_err(|_| Error::Channel("pty is closed".into()))?;
        Ok(())
    }

    async fn pty_resize(&self, pty: &PtySession, cols: u16, rows: u16) -> Result<()> {
        pty.inner
            .commands
            .send(crate::conn::PtyCmd::Resize(cols, rows))
            .map_err(|_| Error::Channel("pty is closed".into()))?;
        Ok(())
    }

    fn pty_output(&self, pty: &PtySession) -> tokio::sync::mpsc::UnboundedReceiver<PtyEvent> {
        pty.inner.take_output()
    }

    async fn close(&self, session: &Session) -> Result<()> {
        session.inner.close().await
    }
}

async fn lock_sftp(client: &SftpClient) -> Result<Arc<russh_sftp::client::SftpSession>> {
    client
        .inner
        .sftp
        .lock()
        .await
        .clone()
        .ok_or_else(|| Error::Channel("sftp session is not established".into()))
}

impl SftpHandle {
    fn new_some(session: Arc<russh_sftp::client::SftpSession>) -> Self {
        Self {
            sftp: tokio::sync::Mutex::new(Some(session)),
        }
    }
}

// ---------------------------------------------------------------------------
// establish

/// 一次完整建连：预检 host key → TCP+握手 → 认证 → （TOFU）记录指纹。
pub(crate) async fn establish(
    config: &SshConfig,
    secrets: &ConnectionSecrets,
    known_hosts: &Arc<StdMutex<KnownHostsStore>>,
) -> Result<Arc<Handle<ConnHandler>>> {
    if config.port == 0 {
        return Err(Error::Configuration("port must be 1..=65535".into()));
    }

    let pinned = known_hosts
        .lock()
        .map_err(|_| Error::Transport("known_hosts lock poisoned".into()))?
        .pinned(&config.host, config.port);
    let (expected, accept_unknown) = match pinned {
        Some(pinned_fp) => (Some(pinned_fp), false),
        None => match config.known_hosts_policy {
            crate::KnownHostsPolicy::RequireMatch => {
                return Err(Error::HostKeyVerification {
                    host: config.host.clone(),
                    fingerprint: "not pinned (first connect must be explicitly trusted)".into(),
                });
            }
            crate::KnownHostsPolicy::TrustOnFirstUse => (None, true),
        },
    };

    let presented = Arc::new(StdMutex::new(None::<String>));
    let handler = ConnHandler {
        expected,
        accept_unknown,
        presented: Arc::clone(&presented),
    };
    let ssh_config = client::Config {
        inactivity_timeout: Some(std::time::Duration::from_secs(60)),
        ..<_>::default()
    };

    let mut handle = client::connect(
        Arc::new(ssh_config),
        (config.host.as_str(), config.port),
        handler,
    )
    .await
    .map_err(map_send_err)?;

    authenticate(&mut handle, config, secrets).await?;

    // TOFU：认证通过后再钉指纹，认证失败不留下记录。
    if accept_unknown {
        if let Some(fp) = presented
            .lock()
            .map_err(|_| Error::Transport("lock poisoned".into()))?
            .clone()
        {
            known_hosts
                .lock()
                .map_err(|_| Error::Transport("known_hosts lock poisoned".into()))?
                .register(&config.host, config.port, &fp)
                .map_err(|error| Error::Transport(error.to_string()))?;
        }
    }

    Ok(Arc::new(handle))
}

async fn authenticate(
    handle: &mut Handle<ConnHandler>,
    config: &SshConfig,
    secrets: &ConnectionSecrets,
) -> Result<()> {
    let user = config.username.as_str();
    match &config.authentication {
        Authentication::Password { .. } => {
            let password = secrets.password.as_deref().ok_or_else(|| {
                Error::Authentication("no password resolved at the call site".into())
            })?;
            let result = handle
                .authenticate_password(user, password)
                .await
                .map_err(map_send_err)?;
            if !result.success() {
                return Err(Error::Authentication("server rejected the password".into()));
            }
        }
        Authentication::PrivateKey { .. } => {
            let pem = secrets.private_key_pem.as_deref().ok_or_else(|| {
                Error::Authentication("no private key resolved at the call site".into())
            })?;
            let key = ssh_key::PrivateKey::from_openssh(pem).map_err(|error| {
                Error::Authentication(format!(
                    "cannot parse the private key (password-protected keys are not supported yet): {error}"
                ))
            })?;
            let hash = handle
                .best_supported_rsa_hash()
                .await
                .map_err(map_send_err)?
                .flatten();
            let result = handle
                .authenticate_publickey(
                    user,
                    russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                )
                .await
                .map_err(map_send_err)?;
            if !result.success() {
                return Err(Error::Authentication(
                    "server rejected the public key".into(),
                ));
            }
        }
        Authentication::Agent => {
            return Err(Error::Authentication(
                "ssh-agent auth is not implemented yet".into(),
            ));
        }
    }
    Ok(())
}

/// 包裹一次"transport 断开 → 重连 → 重试"：只对 transport 类错误重试，认证 /
/// 校验 / 参数错误不重试。
async fn retry_transport_async<T, F, Fut>(session: &Session, op: F) -> Result<T>
where
    F: Fn(Arc<Handle<ConnHandler>>) -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
    T: Send,
{
    let mut attempt = 0;
    loop {
        let conn = session.inner.conn.lock().await.clone();
        let result = op(conn).await;
        match result {
            Err(Error::Transport(_)) if attempt == 0 => {
                session.inner.reconnect().await?;
                attempt += 1;
            }
            other => return other,
        }
    }
}

// ---------------------------------------------------------------------------
// command execution

async fn run_command(
    conn: Arc<Handle<ConnHandler>>,
    command: &str,
    timeout: Option<std::time::Duration>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<CommandResult> {
    let mut channel = conn.channel_open_session().await.map_err(map_send_err)?;
    channel.exec(true, command).await.map_err(map_send_err)?;

    let body = async {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exit_code = None;
        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
                ChannelMsg::ExtendedData { data, ext: 1 } => stderr.extend_from_slice(&data),
                ChannelMsg::ExitStatus { exit_status } => exit_code = Some(exit_status as i32),
                ChannelMsg::Close | ChannelMsg::Eof => break,
                _ => {}
            }
        }
        Ok(CommandResult {
            exit_code: exit_code.unwrap_or(-1),
            stdout,
            stderr,
        })
    };

    match timeout {
        Some(limit) => tokio::select! {
            _ = cancel.cancelled() => Err(Error::Cancelled),
            result = tokio::time::timeout(limit, body) => result.map_err(|_| Error::Timeout)?,
        },
        None => tokio::select! {
            _ = cancel.cancelled() => Err(Error::Cancelled),
            result = body => result,
        },
    }
}

// ---------------------------------------------------------------------------
// pty channels

async fn open_pty_channel(
    conn: Arc<Handle<ConnHandler>>,
    cols: u16,
    rows: u16,
) -> Result<Channel<russh::client::Msg>> {
    let channel = conn.channel_open_session().await.map_err(map_send_err)?;
    channel
        .request_pty(
            true,
            "xterm-256color",
            u32::from(cols),
            u32::from(rows),
            0,
            0,
            &[(Pty::TTY_OP_END, 0), (Pty::ONLCR, 0)],
        )
        .await
        .map_err(map_send_err)?;
    channel.request_shell(true).await.map_err(map_send_err)?;
    Ok(channel)
}

// ---------------------------------------------------------------------------
// error mapping

fn map_send_err(error: russh::Error) -> Error {
    Error::Transport(error.to_string())
}

// ---------------------------------------------------------------------------
// handler

/// 认证期 host key 校验：核对 against 已钉指纹；TOFU 下放行（记录在 establish）。
pub(crate) struct ConnHandler {
    expected: Option<String>,
    accept_unknown: bool,
    presented: Arc<StdMutex<Option<String>>>,
}

impl client::Handler for ConnHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        let fingerprint = match server_public_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => {
                key.fingerprint(HashAlg::Sha256).to_string()
            }
            PublicKeyOrCertificate::Certificate(_) => {
                // 证书认证在 MVP 之外：不"先信再查"。
                return Ok(false);
            }
        };
        if let Ok(mut slot) = self.presented.lock() {
            *slot = Some(fingerprint.clone());
        }

        Ok(match &self.expected {
            Some(pinned) => *pinned == fingerprint,
            None => self.accept_unknown,
        })
    }
}

impl KnownHostsStore {
    /// 只看是否已钉过、钉子是什么（不比较 presented）。
    fn pinned(&self, host: &str, port: u16) -> Option<String> {
        match self.check(host, port, "") {
            Check::Matches { pinned } | Check::Mismatch { pinned, .. } => Some(pinned),
            Check::Unknown => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KnownHostsPolicy;

    #[test]
    fn pinned_reports_unknown_for_new_hosts() {
        let store = KnownHostsStore::in_memory();
        assert_eq!(store.pinned("example.com", 22), None);
    }

    #[tokio::test]
    async fn connect_refuses_untrusted_host_under_require_match() {
        let backend = RusshBackend::new(Arc::new(StdMutex::new(KnownHostsStore::in_memory())));
        let config = SshConfig {
            server_id: "srv_t".into(),
            host: "10.255.255.1".into(),
            port: 2222,
            username: "root".into(),
            authentication: Authentication::Password {
                credential_ref: "keychain://ssh/t".into(),
            },
            known_hosts_policy: KnownHostsPolicy::RequireMatch,
            keepalive_interval_secs: 0,
        };
        let result = backend.connect(config, ConnectionSecrets::empty()).await;
        assert!(matches!(
            result,
            Err(Error::HostKeyVerification { host, .. }) if host == "10.255.255.1"
        ));
        // 不触网：RequireMatch 下未知主机在 TCP 之前就被拒绝。
    }

    #[tokio::test]
    async fn connect_to_unreachable_host_maps_to_transport() {
        let backend = RusshBackend::new(Arc::new(StdMutex::new(KnownHostsStore::in_memory())));
        let config = SshConfig {
            server_id: "srv_t".into(),
            host: "127.0.0.1".into(),
            port: 1, // nothing listens here
            username: "root".into(),
            authentication: Authentication::Password {
                credential_ref: "keychain://ssh/t".into(),
            },
            known_hosts_policy: KnownHostsPolicy::TrustOnFirstUse,
            keepalive_interval_secs: 0,
        };
        let result = backend.connect(config, ConnectionSecrets::empty()).await;
        assert!(matches!(result, Err(Error::Transport(_))));
    }
}
