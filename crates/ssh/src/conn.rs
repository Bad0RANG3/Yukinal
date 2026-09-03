//! Live-connection handles shared by sessions, PTYs and SFTP clients.
//!
//! russh types stay inside this module + `backend`; `Session`/`PtySession`/
//! `SftpClient` in the crate root only hold `Arc`s to the handles defined here.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::{watch, Mutex};

use crate::backend::{establish, ConnHandler};
use crate::known_hosts::KnownHostsStore;
use crate::{ConnectionSecrets, Error, PtyEvent, Result, SshConfig};

/// One established connection, shared by every clone of a `Session`.
pub(crate) struct SessionHandle {
    pub conn: Mutex<Arc<russh::client::Handle<ConnHandler>>>,
    pub known_hosts: Arc<StdMutex<KnownHostsStore>>,
    pub config: SshConfig,
    secrets: ConnectionSecrets,
    reconnecting: AtomicBool,
    shutdown: watch::Sender<bool>,
    keepalive_task: tokio::task::JoinHandle<()>,
}

impl SessionHandle {
    /// Build a session from an already-authenticated connection. Starts the
    /// keepalive loop (when configured) in the background.
    pub(crate) fn new(
        conn: Arc<russh::client::Handle<ConnHandler>>,
        config: SshConfig,
        secrets: ConnectionSecrets,
        known_hosts: Arc<StdMutex<KnownHostsStore>>,
    ) -> Self {
        let (shutdown, mut shutdown_rx) = watch::channel(false);
        let keepalive_task = if config.keepalive_interval_secs > 0 {
            let interval =
                std::time::Duration::from_secs(u64::from(config.keepalive_interval_secs));
            let ka_conn = Arc::clone(&conn);
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(interval);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        _ = shutdown_rx.changed() => break,
                        _ = tick.tick() => {
                            // Ping keeps NAT/proxy sessions alive; a failure here is
                            // surfaced on the next operation (ops reconnect), not raced
                            // from a background task that would clobber session state.
                            if ka_conn.send_ping().await.is_err() {
                                tracing::debug!("ssh keepalive ping failed; ops will reconnect");
                            }
                        }
                    }
                }
            })
        } else {
            tokio::spawn(async move {
                let _ = shutdown_rx.changed().await;
            })
        };

        Self {
            conn: Mutex::new(conn),
            known_hosts,
            config,
            secrets,
            reconnecting: AtomicBool::new(false),
            shutdown,
            keepalive_task,
        }
    }

    /// Re-establish the connection using the stored config + resolved secrets.
    /// Single-flight: concurrent callers wait for the in-flight reconnect.
    pub(crate) async fn reconnect(&self) -> Result<()> {
        if self.reconnecting.swap(true, Ordering::SeqCst) {
            for _ in 0..20 {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                if !self.reconnecting.load(Ordering::SeqCst) {
                    return Ok(());
                }
            }
            return Err(Error::Transport("reconnect already in progress".into()));
        }

        let result = establish(&self.config, &self.secrets, &self.known_hosts).await;
        self.reconnecting.store(false, Ordering::SeqCst);
        let new_conn = result?;
        *self.conn.lock().await = new_conn;
        Ok(())
    }

    /// Polite shutdown: stop keepalive, tell the peer, drop the connection.
    pub(crate) async fn close(&self) -> Result<()> {
        let _ = self.shutdown.send(true);
        let conn = self.conn.lock().await;
        let _ = conn
            .disconnect(russh::Disconnect::ByApplication, "session closed", "en")
            .await;
        Ok(())
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

/// 终端方向命令：由持有完整 `russh::Channel` 的 PTY 任务消费。
pub(crate) enum PtyCmd {
    Write(Vec<u8>),
    Resize(u16, u16),
}

/// One open PTY: commands in, output events out. The single task owning the
/// russh `Channel` lives in `backend::open_pty`; this handle carries the two ends.
pub(crate) struct PtyHandle {
    pub output_tx: tokio::sync::mpsc::UnboundedSender<PtyEvent>,
    pub commands: tokio::sync::mpsc::UnboundedSender<PtyCmd>,
    receiver: tokio::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<PtyEvent>>>,
}

impl PtyHandle {
    #[must_use]
    pub fn new() -> (Self, tokio::sync::mpsc::UnboundedReceiver<PtyCmd>) {
        let (output_tx, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (commands, commands_rx) = tokio::sync::mpsc::unbounded_channel();
        (
            Self {
                output_tx,
                commands,
                receiver: tokio::sync::Mutex::new(Some(receiver)),
            },
            commands_rx,
        )
    }

    /// The one output stream of this PTY (terminal owns exactly one subscriber).
    pub(crate) fn take_output(&self) -> tokio::sync::mpsc::UnboundedReceiver<PtyEvent> {
        let mut slot = self.receiver.blocking_lock();
        slot.take().unwrap_or_else(|| {
            // Not supposed to happen twice; a fresh silent receiver keeps
            // callers from panicking on misuse.
            let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
            rx
        })
    }
}

/// One SFTP subsystem handle. Established lazily on first use of the client.
pub(crate) struct SftpHandle {
    pub sftp: tokio::sync::Mutex<Option<Arc<russh_sftp::client::SftpSession>>>,
}

impl SftpHandle {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            sftp: tokio::sync::Mutex::new(None),
        }
    }
}
