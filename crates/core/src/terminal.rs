//! Terminal orchestration: the ssh `Session` → PTY → `TerminalManager` bridge.
//!
//! `SshPty` adapts `yukinal_ssh::PtySession` to `yukinal_terminal::TerminalPty`
//! (the seam that keeps the manager testable). `TerminalService` owns the
//! connection cache (serverId → ssh [`Session`]) and the PTY manager, so the Tauri
//! command layer only marshals ids.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use yukinal_ssh::{ConnectionSecrets, PtyEvent, RusshBackend, Session, SshBackend, SshConfig};
use yukinal_terminal::{TerminalAppEvent, TerminalManager, TerminalPty};

pub type Result<T> = std::result::Result<T, TerminalServiceError>;

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
    session: Session,
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

    fn events(&self) -> tokio::sync::mpsc::UnboundedReceiver<PtyEvent> {
        self.backend.pty_output(&self.pty)
    }

    async fn close(&self) -> yukinal_terminal::Result<()> {
        self.backend
            .close(&self.session)
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

    /// 存/取一条已认证的 ssh 连接。调用方（命令层）负责先 connect。
    pub fn cache_session(&self, server_id: &str, session: Session) {
        self.sessions
            .lock()
            .expect("sessions lock")
            .insert(server_id.to_string(), session);
    }

    pub fn cached_session(&self, server_id: &str) -> Result<Session> {
        self.sessions
            .lock()
            .expect("sessions lock")
            .get(server_id)
            .cloned()
            .ok_or_else(|| TerminalServiceError::NoSession(server_id.to_string()))
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
            session: session.clone(),
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
