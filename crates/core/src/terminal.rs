//! Terminal orchestration: the ssh `Session` → PTY → `TerminalManager` bridge.
//!
//! `SshPty` adapts `yukinal_ssh::PtySession` to `yukinal_terminal::TerminalPty`
//! (the seam that keeps the manager testable). `TerminalService` owns the
//! connection cache (serverId → ssh [`Session`]) and the PTY manager, so the Tauri
//! command layer only marshals ids.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use yukinal_ssh::{
    link_count_probe_command, parse_link_count, ConnectionSecrets, PtyEvent, RusshBackend, Session,
    SftpFileStat, SftpReplaceError, SftpReplaceGuard, SftpReplacement, SshBackend, SshConfig,
};
use yukinal_terminal::{TerminalAppEvent, TerminalManager, TerminalPty};

pub type Result<T> = std::result::Result<T, TerminalServiceError>;

/// 硬链接探针的超时。它只是一次只读 `stat`；卡住不该拖住一次编辑。
const LINK_COUNT_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// 一次有守卫替换的失败分类。
///
/// 分类而不是一句话：`ConcurrentChange` 该重读后重试，`Unsupported` 与
/// `MetadataNotPreserved` 重试多少次都一样（要变的是远端或这份文件），`Failed` 是传输问题。
#[derive(Debug)]
pub enum SftpReplaceFailure {
    /// 文件在读取之后被改过，或发布之后发现不是我们写进去的那一份。
    ConcurrentChange(String),
    /// 远端不能安全替换：symlink、rename 被拒、staging 建不出来。
    Unsupported(String),
    /// metadata 保不住；`missing` 逐项点名（`mode` / `mtime` / `owner` / `group`）。
    MetadataNotPreserved {
        message: String,
        missing: Vec<String>,
    },
    /// 会话或传输失败；文案已经处理好。
    Failed(String),
}

#[derive(Debug, thiserror::Error)]
pub enum TerminalServiceError {
    #[error("{0}")]
    Ssh(#[from] yukinal_ssh::Error),
    #[error("terminal: {0}")]
    Terminal(#[from] yukinal_terminal::TerminalError),
    #[error("no session cached for server `{0}`; connect first")]
    NoSession(String),
}

/// `yukinal_ssh::PtySession` 到 manager seam 的适配器。
pub struct SshPty {
    backend: Arc<RusshBackend>,
    pty: yukinal_ssh::PtySession,
}

impl TerminalPty for SshPty {
    async fn write(&self, data: &[u8]) -> yukinal_terminal::Result<()> {
        self.backend
            .pty_write(&self.pty, data)
            .await
            .map_err(|error| yukinal_terminal::TerminalError::Channel(error.to_string()))
    }

    async fn resize(&self, cols: u16, rows: u16) -> yukinal_terminal::Result<()> {
        self.backend
            .pty_resize(&self.pty, cols, rows)
            .await
            .map_err(|error| yukinal_terminal::TerminalError::Channel(error.to_string()))
    }

    fn events(&self) -> tokio::sync::mpsc::Receiver<PtyEvent> {
        self.backend.pty_output(&self.pty)
    }

    async fn close(&self) -> yukinal_terminal::Result<()> {
        self.backend
            .pty_close(&self.pty)
            .await
            .map_err(|error| yukinal_terminal::TerminalError::Channel(error.to_string()))
    }
}

pub struct TerminalService {
    ssh: Arc<RusshBackend>,
    sessions: Mutex<HashMap<String, Session>>,
    manager: TerminalManager<SshPty>,
}

impl TerminalService {
    #[must_use]
    pub fn new(ssh: Arc<RusshBackend>) -> Self {
        Self {
            ssh,
            sessions: Mutex::new(HashMap::new()),
            manager: TerminalManager::new(),
        }
    }

    /// 取 `sessions` 的锁，**中毒时照常使用**，而不是 panic。
    ///
    /// 为什么这里可以恢复，而 `crates/database` 与 `crates/credentials` 选择报错：
    /// 中毒标记的意义是「有人握着锁 panic 了，受它保护的数据可能只改了一半」。
    /// 判据因此是**这个锁后面有没有跨字段的不变量**。
    ///
    /// 数据库那边有（一次事务要同时改多行，半途 panic 会留下不一致的账），凭证那边
    /// 也有（后端句柄与它的元数据必须一起换）。这里没有：数据就是一个
    /// `HashMap<String, Session>`，三个操作分别是 `insert` / `get().cloned()` /
    /// `remove()`，`HashMap` 自身的内部一致性在 panic 展开后依然成立，也不存在
    /// 「必须同时存在两条记录」的约束。一张缓存中毒之后仍然是可用的缓存。
    ///
    /// 反面代价才是关键：这三处原本写的是 `.expect("sessions lock")`。只要有任何
    /// 一次 panic 落在锁内，标记就永久留下，此后**每一次**终端、SFTP、断开调用都会
    /// 在这里 panic —— 一次故障被放大成该功能整体不可用。而且 `disconnect` 是
    /// `async`，在运行时工作线程上 panic 的破坏面比在普通线程上更大。
    /// `crates/ssh/src/conn.rs` 的 `PtyHandle::take_output` 出于同样的理由
    /// 选择了 `into_inner()`。
    fn sessions(&self) -> std::sync::MutexGuard<'_, HashMap<String, Session>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 存/取一条已认证的 ssh 连接。调用方（命令层）负责先 connect。
    pub fn cache_session(&self, server_id: &str, session: Session) {
        self.sessions().insert(server_id.to_string(), session);
    }

    pub fn cached_session(&self, server_id: &str) -> Result<Session> {
        self.sessions()
            .get(server_id)
            .cloned()
            .ok_or_else(|| TerminalServiceError::NoSession(server_id.to_string()))
    }

    /// Close all PTYs and remove the cached SSH session for a server.
    pub async fn disconnect(&self, server_id: &str) -> Result<bool> {
        // 锁在语句结束时释放，**不跨**下面的 `.await`：改写成
        // `let guard = self.sessions(); let session = guard.remove(..)` 就会把
        // 互斥量握过 `close_for_server` / `ssh.close` 两次网络往返。
        let session = self.sessions().remove(server_id);
        let _closed_terminals = self.manager.close_for_server(server_id).await?;
        let Some(session) = session else {
            return Ok(false);
        };
        self.ssh.close(&session).await?;
        Ok(true)
    }

    pub async fn sftp_list(
        &self,
        server_id: &str,
        path: &str,
    ) -> Result<Vec<(String, String, u64)>> {
        let session = self.cached_session(server_id)?;
        let client = self.ssh.sftp(&session).await?;
        Ok(self.ssh.sftp_list_dir_detailed(&client, path).await?)
    }

    pub async fn sftp_read(&self, server_id: &str, path: &str) -> Result<Vec<u8>> {
        self.sftp_read_bounded(server_id, path, usize::MAX).await
    }

    pub async fn sftp_read_bounded(
        &self,
        server_id: &str,
        path: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>> {
        let session = self.cached_session(server_id)?;
        let client = self.ssh.sftp(&session).await?;
        Ok(self
            .ssh
            .sftp_read_file_bounded(&client, path, max_bytes)
            .await?)
    }

    pub async fn sftp_write(&self, server_id: &str, path: &str, data: &[u8]) -> Result<()> {
        let session = self.cached_session(server_id)?;
        let client = self.ssh.sftp(&session).await?;
        Ok(self.ssh.sftp_write_file(&client, path, data).await?)
    }

    /// 一个远端路径的属性（`lstat` 语义：symlink 不会被跟随）。
    pub async fn sftp_stat(&self, server_id: &str, path: &str) -> Result<SftpFileStat> {
        let session = self.cached_session(server_id)?;
        let client = self.ssh.sftp(&session).await?;
        Ok(self.ssh.sftp_stat(&client, path).await?)
    }

    /// 文件有几个名字（硬链接数）。SFTP 属性里没有这一项，所以它是一次远端 `stat` 探针；
    /// `Ok(None)` 表示**不知道**（没有可用的 `stat`、输出看不懂、或者探针本身失败）。
    ///
    /// 探针失败在这里刻意**不**变成错误：调用方要区分的是「确认只有一个名字」与「无法确认」，
    /// 而这两种都不是一次传输失败。
    pub async fn sftp_link_count(&self, server_id: &str, path: &str) -> Result<Option<u64>> {
        let Some(command) = link_count_probe_command(path) else {
            return Ok(None);
        };
        let session = self.cached_session(server_id)?;
        let cancel = CancellationToken::new();
        match self
            .ssh
            .execute_once(&session, &command, Some(LINK_COUNT_PROBE_TIMEOUT), &cancel)
            .await
        {
            Ok(result) => Ok(parse_link_count(&result.stdout, result.exit_code)),
            Err(error) => {
                tracing::warn!(path, "link-count probe failed: {error}");
                Ok(None)
            }
        }
    }

    /// 有守卫的原子替换（ADR 0017）。失败分三类：有人改了、远端做不到、传输失败。
    pub async fn sftp_replace_guarded(
        &self,
        server_id: &str,
        path: &str,
        guard: SftpReplaceGuard,
        data: &[u8],
    ) -> std::result::Result<SftpReplacement, SftpReplaceFailure> {
        let session = self
            .cached_session(server_id)
            .map_err(|error| SftpReplaceFailure::Failed(error.to_string()))?;
        let client = self
            .ssh
            .sftp(&session)
            .await
            .map_err(|error| SftpReplaceFailure::Failed(error.to_string()))?;
        match self
            .ssh
            .sftp_replace_file_guarded(&client, path, guard, data)
            .await
        {
            Ok(replacement) => Ok(replacement),
            Err(SftpReplaceError::ConcurrentChange(detail)) => {
                Err(SftpReplaceFailure::ConcurrentChange(detail))
            }
            Err(SftpReplaceError::Unsupported(detail)) => {
                Err(SftpReplaceFailure::Unsupported(detail))
            }
            Err(SftpReplaceError::MetadataNotPreserved { message, missing }) => {
                Err(SftpReplaceFailure::MetadataNotPreserved { message, missing })
            }
            Err(SftpReplaceError::Transport(error)) => {
                Err(SftpReplaceFailure::Failed(error.to_string()))
            }
        }
    }

    #[must_use]
    pub fn manager(&self) -> &TerminalManager<SshPty> {
        &self.manager
    }

    /// 在 `serverId` 的已存连接上开一个 PTY 终端会话。
    pub async fn open(&self, server_id: &str, cols: u16, rows: u16) -> Result<String> {
        let session = self.cached_session(server_id)?;
        let pty = self.ssh.open_pty(&session, (cols, rows)).await?;
        let adopted = SshPty {
            backend: Arc::clone(&self.ssh),
            pty,
        };
        Ok(self.manager.open(server_id, cols, rows, adopted).await?)
    }

    pub async fn write(&self, terminal_session_id: &str, data: &[u8]) -> Result<()> {
        self.manager.write(terminal_session_id, data).await?;
        Ok(())
    }

    pub async fn resize(&self, terminal_session_id: &str, cols: u16, rows: u16) -> Result<()> {
        self.manager.resize(terminal_session_id, cols, rows).await?;
        Ok(())
    }

    pub async fn close(&self, terminal_session_id: &str) -> Result<()> {
        self.manager.close(terminal_session_id).await?;
        Ok(())
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<TerminalAppEvent> {
        self.manager.subscribe()
    }
}

/// 便捷构造：`SshConfig` + 已解析 `ConnectionSecrets` → 已缓存连接（供 terminal_open）。
pub async fn connect_and_cache(
    service: &TerminalService,
    ssh: &Arc<RusshBackend>,
    config: SshConfig,
    secrets: ConnectionSecrets,
) -> Result<()> {
    let session = ssh.connect(config.clone(), secrets).await?;
    service.cache_session(&config.server_id, session);
    Ok(())
}
