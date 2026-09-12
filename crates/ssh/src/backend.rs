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

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use russh::client::{self, AuthResult, Handle};
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::agent::AgentIdentity;
use russh::keys::{ssh_key, HashAlg, PublicKeyOrCertificate};
use russh::{Channel, ChannelMsg, Pty};

use crate::conn::{PtyHandle, SessionHandle, SftpHandle};
use crate::known_hosts::KnownHostsStore;
use crate::{
    AgentError, Authentication, CertificateError, CommandResult, ConnectionSecrets, Error,
    PrivateKeyError, PtyEvent, PtySession, Result, Session, SftpClient, SshBackend, SshConfig,
};

/// 建连 + 认证整体超时（硬性兜底，不让 UI 卡在握手）。
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
/// Remote commands must not be able to grow an unbounded Rust `Vec` from a
/// hostile or unexpectedly noisy process. The host layer applies tighter
/// semantic limits when it parses a result; this is the transport-level cap.
const MAX_COMMAND_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// agent 连接 / 问候的上限。
///
/// 必须有：russh 的 `AgentClient::connect_named_pipe` 在管道忙（`ERROR_PIPE_BUSY`）
/// 时每 50ms 重试一次、且**没有次数上限**，而「agent 忙」恰恰是常见状态（另一个
/// `ssh-add` 正在写）。没有这层超时，这个循环会一直转下去 —— 首次连接被
/// `CONNECT_TIMEOUT` 兜住，但 `SessionHandle::reconnect` 没有外层超时，那条路径
/// 会永久挂起。
const AGENT_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// OpenSSH for Windows 的 agent 固定监听这个命名管道（Windows 上没有 Unix socket，
/// 所以「agent 在哪」在那里是一个常量）。
#[cfg(windows)]
const OPENSSH_AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

/// OpenSSH 的证书配对约定：私钥 `/path/id_ed25519` 的证书是同目录的
/// `/path/id_ed25519-cert.pub`。
///
/// 这是**约定**而不是协议要求：服务器只认「证书 + 与之对应的私钥」这一对，文件名
/// 与配对无关。`ssh-keygen -s`、`ssh-add`、`ssh -i` 都按这个后缀自动配对，所以它是
/// 本 crate 推导证书位置的默认方案；推导不出来时必须显式报
/// [`CertificateError::PathUndetermined`]，不能当成「这台服务器不用证书」而退回裸
/// key 认证。
const OPENSSH_CERT_SUFFIX: &str = "-cert.pub";

/// agent 客户端统一装箱：Unix socket、Windows 命名管道、Pageant 的 stream 类型
/// 各不相同，装箱之后上面的认证代码只有一条路径。
type DynamicAgent = AgentClient<Box<dyn AgentStream + Send + Unpin>>;

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

    /// 把当前指纹钉进 `known_hosts` 并落盘。
    ///
    /// # 这个方法目前没有任何调用者
    ///
    /// 它原先的说明是「供 UI 的『信任这台主机』动作使用」—— 而那个动作并不存在：
    /// 全仓库搜不到第二处 `trust_host`，桌面端也完全不知道 host key 或指纹
    /// （`apps/desktop/src` 里没有任何 `fingerprint` / `hostKey` / `known_hosts`
    /// 的引用）。所以现在的实际行为是：
    ///
    /// - 指纹不匹配时 `establish` 返回 [`Error::HostKeyVerification`]（见下方 ~397 行），
    ///   这条错误经由 IPC 落到界面上时只是**一句普通错误文本**
    ///   （`Display` 在 `lib.rs:62` 拼成 `host key verification failed for {host}
    ///   (fingerprint {fingerprint})`），用户看不到可操作的「信任」入口；
    /// - ADR 0002 描述的严格模式（「直接拒绝并提示需要先显式信任」）因此缺少界面侧
    ///   的补救手段：拒绝是对的，提示是有的，但提示里那个动作没接上。
    ///
    /// 保留而不删除，是因为它是那条补救路径唯一已实现的机制，删掉会让严格模式在
    /// 结构上变成死路。把它接上属于功能改动（要定 IPC 形状、要在界面上摆出指纹
    /// 与确认动作），不在「重构不改变行为」的范围内 —— 所以这里只把事实写清楚，
    /// 不留一句会让人以为它已经接好的旧注释。
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

    /// Execute a command exactly once. Read-only commands use the trait method,
    /// which may reconnect and retry a transport failure; callers with side
    /// effects must use this method so a lost response cannot repeat the action.
    pub async fn execute_once(
        &self,
        session: &Session,
        command: &str,
        timeout: Option<std::time::Duration>,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<CommandResult> {
        run_command(
            session.inner.conn.lock().await.clone(),
            command,
            timeout,
            cancel,
            MAX_COMMAND_OUTPUT_BYTES,
        )
        .await
    }

    /// SFTP 冒烟操作（文件工具落地前证明子系统真实可用）：远端目录清单。
    pub async fn sftp_list_dir(&self, client: &SftpClient, path: &str) -> Result<Vec<String>> {
        let sftp = lock_sftp(client).await?;
        let dir = sftp
            .read_dir(path)
            .await
            .map_err(|error| Error::Channel(error.to_string()))?;
        Ok(dir.map(|entry| entry.file_name()).collect())
    }

    pub async fn sftp_list_dir_detailed(
        &self,
        client: &SftpClient,
        path: &str,
    ) -> Result<Vec<(String, String, u64)>> {
        let sftp = lock_sftp(client).await?;
        let dir = sftp
            .read_dir(path)
            .await
            .map_err(|error| Error::Channel(error.to_string()))?;
        Ok(dir
            .map(|entry| {
                let file_type = match entry.file_type() {
                    russh_sftp::protocol::FileType::Dir => "directory",
                    russh_sftp::protocol::FileType::File => "file",
                    russh_sftp::protocol::FileType::Symlink => "symlink",
                    russh_sftp::protocol::FileType::Other => "other",
                };
                (
                    entry.file_name(),
                    file_type.to_string(),
                    entry.metadata().len(),
                )
            })
            .collect())
    }

    /// SFTP 读整文件（filesystem read 工具的基础操作）。
    pub async fn sftp_read_file(&self, client: &SftpClient, path: &str) -> Result<Vec<u8>> {
        self.sftp_read_file_bounded(client, path, usize::MAX).await
    }

    /// SFTP 读取至多 `max_bytes + 1` 字节，以便调用方可靠判断是否截断。
    pub async fn sftp_read_file_bounded(
        &self,
        client: &SftpClient,
        path: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>> {
        use russh_sftp::protocol::OpenFlags;
        use tokio::io::AsyncReadExt;
        let sftp = lock_sftp(client).await?;
        let file = sftp
            .open_with_flags(path, OpenFlags::READ)
            .await
            .map_err(|error| Error::Channel(error.to_string()))?;
        let mut out = Vec::new();
        file.take(max_bytes.saturating_add(1) as u64)
            .read_to_end(&mut out)
            .await
            .map_err(|error| Error::Channel(error.to_string()))?;
        Ok(out)
    }

    /// SFTP 覆盖写入一个文件；close 会等待远端确认所有 pending writes。
    pub async fn sftp_write_file(
        &self,
        client: &SftpClient,
        path: &str,
        data: &[u8],
    ) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        let sftp = lock_sftp(client).await?;
        let mut file = sftp
            .create(path)
            .await
            .map_err(|error| Error::Channel(error.to_string()))?;
        file.write_all(data)
            .await
            .map_err(|error| Error::Channel(error.to_string()))?;
        file.close()
            .await
            .map_err(|error| Error::Channel(error.to_string()))?;
        Ok(())
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
        retry_transport_async(
            session,
            |conn| run_command(conn, command, timeout, cancel, MAX_COMMAND_OUTPUT_BYTES),
            Some(cancel),
        )
        .await
    }

    async fn open_pty(&self, session: &Session, size: (u16, u16)) -> Result<PtySession> {
        let (cols, rows) = size;
        let channel =
            retry_transport_async(session, |conn| open_pty_channel(conn, cols, rows), None).await?;
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
                            crate::conn::PtyCmd::Close => {
                                let _ = channel.close().await;
                                let _ = output_tx.send(PtyEvent::Closed { code: None }).await;
                                break;
                            }
                        }
                    }
                    message = channel.wait() => {
                        match message {
                            None => break,
                            // `.await` 是有意的：输出队列有界（`PTY_OUTPUT_CAPACITY`），
                            // 满了就在这里挂起，于是本任务不再 `channel.wait()`，russh 的
                            // 接收缓冲填满、TCP 窗口关闭，背压传回远端。远端刷屏时应当让
                            // 远端慢下来，而不是把无界数据堆在宿主内存里。
                            // 订阅者退出（终端已关）时 `send` 立刻返回 Err，照样 break。
                            Some(ChannelMsg::Data { data }) => {
                                if output_tx.send(PtyEvent::Output(data.to_vec())).await.is_err() {
                                    break; // 订阅者退出 = 终端已关
                                }
                            }
                            Some(ChannelMsg::ExtendedData { data, ext: 1 }) => {
                                if output_tx.send(PtyEvent::Output(data.to_vec())).await.is_err() {
                                    break;
                                }
                            }
                            Some(ChannelMsg::ExitStatus { exit_status }) => {
                                let _ = output_tx
                                    .send(PtyEvent::Closed {
                                        code: Some(exit_status),
                                    })
                                    .await;
                                break;
                            }
                            Some(ChannelMsg::Close | ChannelMsg::Eof) => {
                                let _ = output_tx.send(PtyEvent::Closed { code: None }).await;
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
        let sftp = retry_transport_async(
            session,
            |conn| async move {
                let channel = conn.channel_open_session().await.map_err(map_send_err)?;
                channel
                    .request_subsystem(true, "sftp")
                    .await
                    .map_err(map_send_err)?;
                let stream = channel.into_stream();
                russh_sftp::client::SftpSession::new(stream)
                    .await
                    .map_err(|error| Error::Channel(format!("sftp handshake failed: {error}")))
            },
            None,
        )
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

    async fn pty_close(&self, pty: &PtySession) -> Result<()> {
        pty.inner
            .commands
            .send(crate::conn::PtyCmd::Close)
            .map_err(|_| Error::Channel("pty is closed".into()))?;
        Ok(())
    }

    fn pty_output(&self, pty: &PtySession) -> tokio::sync::mpsc::Receiver<PtyEvent> {
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
            let key = load_private_key(secrets)?;
            authenticate_with_key(handle, user, key).await?;
        }
        Authentication::Certificate {
            certificate_path,
            private_key_path,
            ..
        } => {
            let key = load_private_key(secrets)?;
            let path =
                derive_certificate_path(certificate_path.as_deref(), private_key_path.as_deref())?;
            let certificate = load_certificate(&path, &key)?;
            let result = handle
                .authenticate_openssh_cert(user, Arc::new(key), certificate)
                .await
                .map_err(map_send_err)?;
            if !result.success() {
                return Err(Error::Authentication(
                    "server rejected the certificate".into(),
                ));
            }
        }
        Authentication::Agent { socket_path } => {
            authenticate_with_agent(handle, user, socket_path.as_deref()).await?;
        }
    }
    Ok(())
}

/// 单次 publickey 认证。
///
/// `key` 的所有权在这里结束：解密后的私钥**不缓存**，重连时会拿
/// `ConnectionSecrets` 重新解一次。这正是 `SessionHandle` 保存 `ConnectionSecrets`
/// 而不保存明文 key 的原因 —— 口令与明文私钥的生命周期跟着调用点，不跟着会话。
async fn authenticate_with_key(
    handle: &mut Handle<ConnHandler>,
    user: &str,
    key: ssh_key::PrivateKey,
) -> Result<()> {
    let hash = best_supported_rsa_hash(handle).await?;
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
    Ok(())
}

/// RSA 的签名哈希由**服务器**的 `server-sig-algs` 决定（`ssh-rsa` / `rsa-sha2-*`
/// 是三把不同的「钥匙」）；非 RSA 的 key 忽略这个值。
async fn best_supported_rsa_hash(handle: &Handle<ConnHandler>) -> Result<Option<HashAlg>> {
    Ok(handle
        .best_supported_rsa_hash()
        .await
        .map_err(map_send_err)?
        .flatten())
}

/// 把 `ConnectionSecrets` 里的私钥材料变成可用的 `ssh_key::PrivateKey`。
///
/// 四种失败各自成套报出（见 [`PrivateKeyError`]）。口令只在函数体内被借用一次：
/// 返回的错误里不含任何口令材料，只说明「需要口令 / 口令不对 / 本就没加密」这些事实。
///
/// 空口令按「没给口令」处理：界面上的口令框留空就是这个形状，把它算成一次「错误的
/// 口令」只会让本来能连的明文 key 连不上。非空口令配**未加密**的 key 则显式报
/// [`PrivateKeyError::NotEncrypted`] —— 那说明调用点把口令配到了别的 key 上，静默
/// 忽略等于把配置错误藏起来。
fn load_private_key(secrets: &ConnectionSecrets) -> Result<ssh_key::PrivateKey> {
    let pem = secrets
        .private_key_pem
        .as_deref()
        .ok_or_else(|| Error::Authentication("no private key resolved at the call site".into()))?;
    let key = ssh_key::PrivateKey::from_openssh(pem).map_err(|error| {
        Error::PrivateKey(PrivateKeyError::Unreadable {
            reason: error.to_string(),
        })
    })?;
    let passphrase = secrets
        .private_key_passphrase
        .as_deref()
        .filter(|value| !value.is_empty());

    // 加密的 key 在 OpenSSH 格式里是可解析的（密文原样保留），所以要靠
    // `is_encrypted()` 判断，而不是指望 `from_openssh` 报错 —— 后者对加密 key 是成功的。
    if !key.is_encrypted() {
        return match passphrase {
            Some(_) => Err(Error::PrivateKey(PrivateKeyError::NotEncrypted)),
            None => Ok(key),
        };
    }
    let passphrase = passphrase.ok_or(Error::PrivateKey(PrivateKeyError::PassphraseRequired))?;
    key.decrypt(passphrase)
        .map_err(|_| Error::PrivateKey(PrivateKeyError::PassphraseRejected))
}

/// 证书位置：显式路径优先，否则按 OpenSSH 的 `-cert.pub` 约定从私钥路径推导。
///
/// 抽成纯函数是为了让「推导」这件事能被单独测：sibling 命名是**约定**而不是协议，
/// 只靠注释守不住。两者都没有时返回 [`CertificateError::PathUndetermined`] ——
/// 缺配置要报出来，不能当成「这台服务器不用证书」而退回裸 key 认证。
fn derive_certificate_path(
    certificate_path: Option<&str>,
    private_key_path: Option<&str>,
) -> Result<std::path::PathBuf> {
    if let Some(explicit) = certificate_path {
        return Ok(std::path::PathBuf::from(explicit));
    }
    private_key_path
        .map(|base| std::path::PathBuf::from(format!("{base}{OPENSSH_CERT_SUFFIX}")))
        .ok_or(Error::Certificate(CertificateError::PathUndetermined))
}

/// 读取 OpenSSH 用户证书，并确认它签的正是同时提供的那把私钥。
fn load_certificate(
    path: &std::path::Path,
    key: &ssh_key::PrivateKey,
) -> Result<ssh_key::Certificate> {
    let displayed = path.display().to_string();
    let text = std::fs::read_to_string(path).map_err(|error| {
        Error::Certificate(CertificateError::Read {
            path: displayed.clone(),
            detail: error.to_string(),
        })
    })?;
    let certificate = ssh_key::Certificate::from_openssh(&text).map_err(|error| {
        Error::Certificate(CertificateError::Parse {
            path: displayed,
            detail: error.to_string(),
        })
    })?;
    // 证书里带公钥，所以「配错文件」在本地就能查出来。发出去再让服务器回一句
    // 认证失败，用户拿到的信息会少一大截。
    if certificate.public_key() != key.public_key().key_data() {
        return Err(Error::Certificate(CertificateError::KeyMismatch));
    }
    Ok(certificate)
}

/// agent 认证：连上 agent，把它持有的身份**逐个**交给服务器。
///
/// 逐个而不是只试第一个：真实 agent 里通常躺着好几把 key（旧的、别的用途的、别的
/// 机器的），只试第一个会把「right key 不在第一位」变成一次认证失败。agent 持有的
/// 证书身份走 `authenticate_certificate_with`（服务器要看的是证书本身，而不是证书
/// 里那把公钥），普通身份走 `authenticate_publickey_with`。
async fn authenticate_with_agent(
    handle: &mut Handle<ConnHandler>,
    user: &str,
    socket_path: Option<&str>,
) -> Result<()> {
    let mut agent = connect_agent(socket_path).await?;
    let identities = agent
        .request_identities()
        .await
        .map_err(|error| Error::Agent(classify_agent_error(&error)))?;
    if identities.is_empty() {
        return Err(Error::Agent(AgentError::NoIdentities));
    }

    let hash = best_supported_rsa_hash(handle).await?;
    let mut identities_tried = 0usize;
    let mut partial_success = false;
    for identity in &identities {
        identities_tried += 1;
        let result = match identity {
            AgentIdentity::PublicKey { key, .. } => handle
                .authenticate_publickey_with(user, key.clone(), hash, &mut agent)
                .await
                .map_err(map_agent_sign_error)?,
            AgentIdentity::Certificate { certificate, .. } => handle
                .authenticate_certificate_with(user, certificate.clone(), hash, &mut agent)
                .await
                .map_err(map_agent_sign_error)?,
        };
        match result {
            AuthResult::Success => return Ok(()),
            AuthResult::Failure {
                partial_success: partial,
                ..
            } => partial_success |= partial,
        }
    }
    Err(Error::Agent(AgentError::Rejected {
        identities_tried,
        partial_success,
    }))
}

/// 连接 agent（平台差异收在这一层），并给整次连接加超时。
///
/// 所有失败都变成 [`AgentError::Unavailable`] 且带上 `agent_label`：这条要求是有意的
/// —— 「agent 连不上」和「密码不对」在界面上必须是两句话，否则用户会去改密码，而
/// 真正要做的是把 agent（或 `ssh-add`）跑起来。
async fn connect_agent(socket_path: Option<&str>) -> Result<DynamicAgent> {
    let label = agent_label(socket_path);
    match tokio::time::timeout(AGENT_CONNECT_TIMEOUT, connect_agent_inner(socket_path)).await {
        Ok(Ok(agent)) => Ok(agent),
        Ok(Err(error)) => Err(Error::Agent(AgentError::Unavailable {
            detail: format!("{label}: {error}"),
        })),
        Err(_) => Err(Error::Agent(AgentError::Unavailable {
            detail: format!(
                "{label}: no answer within {}s",
                AGENT_CONNECT_TIMEOUT.as_secs()
            ),
        })),
    }
}

/// 「我们连的是什么」的人类可读名字，只用于错误消息。
fn agent_label(socket_path: Option<&str>) -> String {
    match socket_path {
        Some(path) => format!("the ssh-agent at {path}"),
        #[cfg(unix)]
        None => "the ssh-agent named by SSH_AUTH_SOCK".to_string(),
        #[cfg(windows)]
        None => format!("the ssh-agent (OpenSSH pipe {OPENSSH_AGENT_PIPE}, then Pageant)"),
        #[cfg(not(any(unix, windows)))]
        None => "the ssh-agent".to_string(),
    }
}

/// Unix：显式路径优先，否则按 `SSH_AUTH_SOCK` 发现（russh 的 `connect_env` 顺带把
/// 「变量指着一个不存在的路径」与「变量根本没设」分成两个不同的错误）。
#[cfg(unix)]
async fn connect_agent_inner(
    socket_path: Option<&str>,
) -> std::result::Result<DynamicAgent, russh::keys::Error> {
    let client = match socket_path {
        Some(path) => AgentClient::connect_uds(path).await?,
        None => AgentClient::connect_env().await?,
    };
    Ok(client.dynamic())
}

/// Windows：显式路径当作命名管道；否则先试 OpenSSH for Windows 的固定管道，再退到
/// Pageant（PuTTY 的 agent 走窗口消息而不是管道，russh 用 `pageant` crate 实现；
/// 它是 russh 在 Windows 上的**非可选**依赖，所以不需要额外 cargo feature）。
#[cfg(windows)]
async fn connect_agent_inner(
    socket_path: Option<&str>,
) -> std::result::Result<DynamicAgent, russh::keys::Error> {
    if let Some(path) = socket_path {
        return Ok(AgentClient::connect_named_pipe(path).await?.dynamic());
    }
    let pipe_error = match AgentClient::connect_named_pipe(OPENSSH_AGENT_PIPE).await {
        Ok(client) => return Ok(client.dynamic()),
        Err(error) => error,
    };
    let pageant_error = match AgentClient::connect_pageant().await {
        Ok(client) => return Ok(client.dynamic()),
        Err(error) => error,
    };
    Err(russh::keys::Error::IO(std::io::Error::other(format!(
        "OpenSSH pipe unavailable ({pipe_error}); Pageant unavailable ({pageant_error})"
    ))))
}

#[cfg(not(any(unix, windows)))]
async fn connect_agent_inner(
    _socket_path: Option<&str>,
) -> std::result::Result<DynamicAgent, russh::keys::Error> {
    Err(russh::keys::Error::IO(std::io::Error::other(
        "no ssh-agent transport is known for this platform",
    )))
}

/// agent 通讯错误 → 类型化错误。
///
/// 这里不再细分「连不上」与「连上后断了」：对调用点而言两者都是「agent 现在不可用」，
/// 底层措辞（连接被拒 / 管道不在 / IO 错误）留在 `detail` 里。
fn classify_agent_error(error: &russh::keys::Error) -> AgentError {
    match error {
        russh::keys::Error::EnvVar(name) => AgentError::Unavailable {
            detail: format!("{name} is not set"),
        },
        russh::keys::Error::BadAuthSock => AgentError::Unavailable {
            detail: "SSH_AUTH_SOCK points at a path that does not exist".into(),
        },
        russh::keys::Error::AgentFailure => AgentError::SigningRejected {
            detail: "the agent answered with a failure message".into(),
        },
        russh::keys::Error::AgentProtocolError => AgentError::Protocol {
            detail: "unexpected reply frame".into(),
        },
        other => AgentError::Unavailable {
            detail: other.to_string(),
        },
    }
}

/// agent 在签名阶段失败时的映射。
///
/// 这里只拿得到 `Display`：russh 0.63 的 `AgentAuthError` 定义在**私有**模块
/// `russh::auth` 里，类型不可命名、`Send` / `Key` 两个分支也不可匹配（它的
/// `source()` 因为 `#[error(transparent)]` 直接透传到更底层，所以 downcast 也拿不到
/// 真正的 `keys::Error`）。于是「agent 拒绝签名」与「签名途中这条 SSH 连接断了」在
/// 类型上合成 [`AgentError::SigningFailed`] 一条，底层措辞留在 `detail` 里 —— 这比
/// 按消息文本猜类型诚实。
fn map_agent_sign_error<E: std::fmt::Display>(error: E) -> Error {
    Error::Agent(AgentError::SigningFailed {
        detail: error.to_string(),
    })
}

/// 包裹一次"transport 断开 → 重连 → 重试"：只对 transport 类错误重试，认证 /
/// 校验 / 参数错误不重试。
async fn retry_transport_async<T, F, Fut>(
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

// ---------------------------------------------------------------------------
// command execution

async fn run_command(
    conn: Arc<Handle<ConnHandler>>,
    command: &str,
    timeout: Option<std::time::Duration>,
    cancel: &tokio_util::sync::CancellationToken,
    max_output_bytes: usize,
) -> Result<CommandResult> {
    let mut channel = conn.channel_open_session().await.map_err(map_send_err)?;
    channel.exec(true, command).await.map_err(map_send_err)?;

    let body = async {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exit_code = None;
        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::Data { data } => append_bounded(&mut stdout, &data, max_output_bytes),
                ChannelMsg::ExtendedData { data, ext: 1 } => {
                    append_bounded(&mut stderr, &data, max_output_bytes)
                }
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

fn append_bounded(output: &mut Vec<u8>, chunk: &[u8], max_bytes: usize) {
    let remaining = max_bytes.saturating_sub(output.len());
    output.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
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
                // 这是**服务器**把 host key 作为证书出示（host certificate），和用户
                // 证书认证（`authenticate` 里的 `Authentication::Certificate`）是两件
                // 不同的事：后者已经支持。
                //
                // 这里仍然拒绝，因为本 crate 没有 host CA 信任库。要接受一张 host
                // 证书，得先知道「哪把 CA key 被信任、签名是否出自它、主机名是否在
                // principals 里」；而我们能做的只有「记住它」—— 那正是这里禁止的
                // 「先信再查」，known_hosts 的钉子会因此变成一句空话。
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
    ///
    /// 这里直接查表，不再借道 `check(host, port, "")` —— 那是以「和空串比较」的形式
    /// 表达一次查找，读起来像在做校验，实际只是取值，而且顺带掩盖了
    /// `Check::Mismatch` 在生产路径上从不触发这件事（真正的比对在
    /// `ConnHandler::check_server_key`，指纹只在该回调里才存在）。
    fn pinned(&self, host: &str, port: u16) -> Option<String> {
        self.pinned_fingerprint(host, port)
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

    // -----------------------------------------------------------------------
    // 认证材料：私钥 / 口令 / 证书 / agent
    //
    // 下面这些测试都不需要服务器：材料在测试里现生成、写进一次性目录，用完删掉。
    // 需要真机的部分（真的把 key 交给服务器、agent 真的签名）留在 `tests/live.rs`，
    // 那些由环境变量门控。

    /// 一次性目录：`std::env::temp_dir()` 下的唯一子目录，`Drop` 时删掉。
    /// 不为此引入 tempfile —— 本 crate 只有测试需要临时文件，仓库既有测试
    /// （`tests/known_hosts.rs`）也是手写临时路径。
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "yukinal-ssh-{tag}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const TEST_PASSPHRASE: &str = "yukinal-test-passphrase";

    fn generated_key() -> ssh_key::PrivateKey {
        ssh_key::PrivateKey::random(&mut rand::rng(), ssh_key::Algorithm::Ed25519)
            .expect("generate ed25519 key")
    }

    fn pem(key: &ssh_key::PrivateKey) -> String {
        key.to_openssh(ssh_key::LineEnding::LF)
            .expect("encode as openssh pem")
            .as_str()
            .to_owned()
    }

    fn secrets_with(key_pem: String, passphrase: Option<&str>) -> ConnectionSecrets {
        ConnectionSecrets {
            password: None,
            private_key_pem: Some(key_pem),
            private_key_passphrase: passphrase.map(str::to_owned),
        }
    }

    /// 用一把 CA key 给一把用户 key 签一张用户证书（测试用，参数固定）。
    fn test_certificate(
        ca: &ssh_key::PrivateKey,
        user: &ssh_key::PrivateKey,
    ) -> ssh_key::Certificate {
        use ssh_key::certificate::{Builder, CertType};
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after the epoch")
            .as_secs();
        let mut builder = Builder::new_with_random_nonce(
            &mut rand::rng(),
            user.public_key(),
            now.saturating_sub(3600),
            now.saturating_add(86_400),
        )
        .expect("certificate builder");
        builder.serial(1).expect("serial");
        builder.key_id("yukinal-test").expect("key id");
        builder.cert_type(CertType::User).expect("cert type");
        builder.valid_principal("testuser").expect("principal");
        builder.sign(ca).expect("sign certificate")
    }

    /// 明文 key：不带口令就能用，且解出来的就是同一把 key。
    #[test]
    fn plain_private_key_loads_without_a_passphrase() {
        let key = generated_key();
        let loaded = load_private_key(&secrets_with(pem(&key), None)).expect("plain key loads");
        assert_eq!(loaded.public_key(), key.public_key());
        assert!(!loaded.is_encrypted());
    }

    /// 加密 key 的三种结局必须彼此可分：没给口令 / 口令不对 / 口令对了。
    ///
    /// 这条测试存在的理由：旧实现把前两种合并成一句「认证失败」并按「不支持」拒绝，
    /// 而用户要做的事完全不同 —— 一个是去拿口令，一个是重新输，还有一个本来就能连。
    #[test]
    fn encrypted_private_key_reports_which_passphrase_problem_it_is() {
        let key = generated_key();
        let encrypted = key
            .encrypt(&mut rand::rng(), TEST_PASSPHRASE)
            .expect("encrypt key");
        assert!(encrypted.is_encrypted(), "加密后的 key 必须自报加密");
        let encoded = pem(&encrypted);

        assert!(
            matches!(
                load_private_key(&secrets_with(encoded.clone(), None)),
                Err(Error::PrivateKey(PrivateKeyError::PassphraseRequired))
            ),
            "加密 key + 没有口令 → PassphraseRequired",
        );
        assert!(
            matches!(
                load_private_key(&secrets_with(encoded.clone(), Some("not-the-passphrase"))),
                Err(Error::PrivateKey(PrivateKeyError::PassphraseRejected))
            ),
            "加密 key + 错误口令 → PassphraseRejected",
        );

        let decrypted = load_private_key(&secrets_with(encoded, Some(TEST_PASSPHRASE)))
            .expect("right passphrase");
        assert!(!decrypted.is_encrypted());
        assert_eq!(
            decrypted.public_key(),
            key.public_key(),
            "解开之后必须是同一把 key",
        );
    }

    /// 空口令按「没给」处理：界面上的口令框留空就是这个形状。
    #[test]
    fn an_empty_passphrase_counts_as_no_passphrase() {
        let key = generated_key();
        assert!(
            load_private_key(&secrets_with(pem(&key), Some(""))).is_ok(),
            "明文 key + 空口令仍然是明文 key",
        );

        let encrypted = pem(&key
            .encrypt(&mut rand::rng(), TEST_PASSPHRASE)
            .expect("encrypt"));
        assert!(matches!(
            load_private_key(&secrets_with(encrypted, Some(""))),
            Err(Error::PrivateKey(PrivateKeyError::PassphraseRequired))
        ));
    }

    /// 口令配到了没加密的 key 上：显式报错，不静默忽略（那是配置错误，不是口令错误）。
    #[test]
    fn a_passphrase_for_a_plain_key_is_reported_not_ignored() {
        let secrets = secrets_with(pem(&generated_key()), Some(TEST_PASSPHRASE));
        assert!(matches!(
            load_private_key(&secrets),
            Err(Error::PrivateKey(PrivateKeyError::NotEncrypted))
        ));
    }

    /// 解析不出来的材料与「需要口令」是两件事，不能混为一谈。
    #[test]
    fn unreadable_key_material_is_not_a_passphrase_problem() {
        let truncated =
            "-----BEGIN OPENSSH PRIVATE KEY-----\nnope\n-----END OPENSSH PRIVATE KEY-----\n";
        assert!(matches!(
            load_private_key(&secrets_with(truncated.into(), Some(TEST_PASSPHRASE))),
            Err(Error::PrivateKey(PrivateKeyError::Unreadable { .. }))
        ));
    }

    /// 调用点根本没解析出私钥时保持既有的 `Authentication` 错误
    /// （其他 crate 已经在看这条路径的文本，不跟着这次改动变）。
    #[test]
    fn absent_key_material_keeps_the_existing_authentication_error() {
        assert!(matches!(
            load_private_key(&ConnectionSecrets::empty()),
            Err(Error::Authentication(_))
        ));
    }

    /// 口令不进 `Debug`、不进错误文本。这是「绝不泄漏」这条要求的可执行部分：
    /// 拿着口令的容器与每一个相关错误变体都会被检查。
    #[test]
    fn the_passphrase_never_reaches_debug_or_display() {
        let secrets = secrets_with(pem(&generated_key()), Some(TEST_PASSPHRASE));
        let debug = format!("{secrets:?}");
        assert!(
            !debug.contains(TEST_PASSPHRASE),
            "ConnectionSecrets 的 Debug 泄漏了口令：{debug}",
        );

        let errors = [
            Error::PrivateKey(PrivateKeyError::PassphraseRequired),
            Error::PrivateKey(PrivateKeyError::PassphraseRejected),
            Error::PrivateKey(PrivateKeyError::NotEncrypted),
        ];
        for error in errors {
            let rendered = format!("{error} {error:?}");
            assert!(
                !rendered.contains(TEST_PASSPHRASE),
                "错误文本泄漏了口令：{rendered}",
            );
        }
    }

    /// sibling 推导：`<private-key-path>` → `<private-key-path>-cert.pub`；
    /// 显式路径优先；两者都没有时是显式错误，而不是「没有证书」。
    #[test]
    fn certificate_path_follows_the_openssh_sibling_convention() {
        assert_eq!(
            derive_certificate_path(None, Some("/home/u/.ssh/id_ed25519")).expect("derive sibling"),
            std::path::PathBuf::from("/home/u/.ssh/id_ed25519-cert.pub"),
        );
        assert_eq!(
            derive_certificate_path(Some("/tmp/explicit.pub"), Some("/home/u/.ssh/id_ed25519"))
                .expect("explicit wins"),
            std::path::PathBuf::from("/tmp/explicit.pub"),
        );
        assert!(matches!(
            derive_certificate_path(None, None),
            Err(Error::Certificate(CertificateError::PathUndetermined))
        ));
    }

    /// 认证路径上的 sibling 查找：磁盘上只有 `<key>` 与 `<key>-cert.pub` 时找到证书。
    #[test]
    fn certificate_is_found_next_to_the_private_key_file() {
        let dir = TempDir::new("cert-sibling");
        let ca = generated_key();
        let user = generated_key();
        let key_path = dir.path().join("id_ed25519");
        let cert_path = dir.path().join("id_ed25519-cert.pub");
        std::fs::write(&key_path, pem(&user)).expect("write key file");
        std::fs::write(
            &cert_path,
            test_certificate(&ca, &user)
                .to_openssh()
                .expect("encode cert"),
        )
        .expect("write certificate");

        let resolved = derive_certificate_path(None, Some(&key_path.display().to_string()))
            .expect("derive sibling");
        assert_eq!(resolved, cert_path);
        let certificate = load_certificate(&resolved, &user).expect("load sibling certificate");
        assert_eq!(certificate.key_id(), "yukinal-test");
    }

    /// 证书必须签的就是同时提供的那把私钥；配错文件要在本地拦下。
    #[test]
    fn certificate_must_certify_the_offered_private_key() {
        let dir = TempDir::new("cert-mismatch");
        let ca = generated_key();
        let user = generated_key();
        let other = generated_key();
        let cert_path = dir.path().join("id_ed25519-cert.pub");
        std::fs::write(
            &cert_path,
            test_certificate(&ca, &user)
                .to_openssh()
                .expect("encode cert"),
        )
        .expect("write certificate");

        let loaded = load_certificate(&cert_path, &user).expect("matching pair loads");
        assert_eq!(loaded.public_key(), user.public_key().key_data());
        assert!(matches!(
            load_certificate(&cert_path, &other),
            Err(Error::Certificate(CertificateError::KeyMismatch))
        ));
    }

    /// 证书读不出 / 根本不是证书：`Read` 与 `Parse` 分开，且都带上路径。
    #[test]
    fn unreadable_and_unparsable_certificates_are_distinguishable() {
        let dir = TempDir::new("cert-errors");
        let key = generated_key();
        let missing = dir.path().join("absent-cert.pub");
        match load_certificate(&missing, &key) {
            Err(Error::Certificate(CertificateError::Read { path, .. })) => {
                assert_eq!(path, missing.display().to_string());
            }
            other => panic!("expected a read error, got {other:?}"),
        }

        let garbage = dir.path().join("garbage-cert.pub");
        std::fs::write(&garbage, "this is not a certificate\n").expect("write garbage");
        assert!(matches!(
            load_certificate(&garbage, &key),
            Err(Error::Certificate(CertificateError::Parse { .. }))
        ));
    }

    /// agent 连不上必须是**类型化的 agent 错误**，且消息里点名是哪个 agent：
    /// 「认证失败」对用户没有任何可操作性。
    #[tokio::test]
    async fn unreachable_agent_is_reported_as_a_named_agent_error() {
        // 两边的「路径」形状不同（Windows 命名管道 / Unix UDS），但都保证不存在。
        let path = if cfg!(windows) {
            format!(r"\\.\pipe\yukinal-no-such-agent-{}", std::process::id())
        } else {
            format!("/tmp/yukinal-no-such-agent-{}", std::process::id())
        };
        match connect_agent(Some(&path)).await {
            Err(Error::Agent(AgentError::Unavailable { detail })) => {
                assert!(
                    detail.contains(&path),
                    "agent 错误必须点名是在连哪个 agent：{detail}",
                );
            }
            Err(other) => panic!("expected a typed unavailable-agent error, got {other:?}"),
            Ok(_) => panic!("there is no agent at {path}, the connection must not succeed"),
        }
    }

    /// agent 通讯错误的分类：变量没设 / socket 不在 / agent 说失败 / 应答不合协议。
    #[test]
    fn agent_errors_are_classified() {
        match classify_agent_error(&russh::keys::Error::EnvVar("SSH_AUTH_SOCK")) {
            AgentError::Unavailable { detail } => {
                assert!(
                    detail.contains("SSH_AUTH_SOCK"),
                    "细节里要有变量名：{detail}"
                );
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            classify_agent_error(&russh::keys::Error::BadAuthSock),
            AgentError::Unavailable { .. }
        ));
        assert!(matches!(
            classify_agent_error(&russh::keys::Error::IO(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "gone",
            ))),
            AgentError::Unavailable { .. }
        ));
        assert!(matches!(
            classify_agent_error(&russh::keys::Error::AgentFailure),
            AgentError::SigningRejected { .. }
        ));
        assert!(matches!(
            classify_agent_error(&russh::keys::Error::AgentProtocolError),
            AgentError::Protocol { .. }
        ));
    }

    /// 签名阶段的失败统一映射成 `SigningFailed`，并保留底层措辞
    /// （russh 的 `AgentAuthError` 在私有模块里，类型上细分不了 —— 见实现处的注释）。
    #[test]
    fn agent_signing_failures_keep_their_detail() {
        match map_agent_sign_error("the agent is locked") {
            Error::Agent(AgentError::SigningFailed { detail }) => {
                assert_eq!(detail, "the agent is locked");
            }
            other => panic!("{other:?}"),
        }
    }
}
