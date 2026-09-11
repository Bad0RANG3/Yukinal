//! Live-connection handles shared by sessions, PTYs and SFTP clients.
//!
//! russh types stay inside this module + `backend`; `Session`/`PtySession`/
//! `SftpClient` in the crate root only hold `Arc`s to the handles defined here.

use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::{watch, Mutex};

use crate::backend::{establish, ConnHandler};
use crate::known_hosts::KnownHostsStore;
use crate::{ConnectionSecrets, PtyEvent, Result, SshConfig};

/// 断开握手的上限。对端失联时 `disconnect()` 不会自己返回，而 `close()` 的
/// 调用方在关窗/切服务器，不该被一台已经失联的主机拖住。
const DISCONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// One established connection, shared by every clone of a `Session`.
pub(crate) struct SessionHandle {
    pub conn: Mutex<Arc<russh::client::Handle<ConnHandler>>>,
    pub known_hosts: Arc<StdMutex<KnownHostsStore>>,
    pub config: SshConfig,
    secrets: ConnectionSecrets,
    reconnect_lock: Mutex<()>,
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
            reconnect_lock: Mutex::new(()),
            shutdown,
            keepalive_task,
        }
    }

    /// Re-establish the connection using the stored config + resolved secrets.
    /// A mutex is used instead of a polling flag so a waiter cannot mistake a
    /// failed reconnect for a successful one.
    pub(crate) async fn reconnect(&self) -> Result<()> {
        let _guard = self.reconnect_lock.lock().await;
        let new_conn = establish(&self.config, &self.secrets, &self.known_hosts).await?;
        *self.conn.lock().await = new_conn;
        Ok(())
    }

    /// Polite shutdown: stop keepalive, tell the peer, drop the connection.
    ///
    /// 这里**不能**握着 `conn` 锁去 `await` 断开过程，原来的写法就是这样：
    ///
    /// ```ignore
    /// let conn = self.conn.lock().await;
    /// let _ = conn.disconnect(...).await;   // 无超时，且锁一直被持有
    /// ```
    ///
    /// `disconnect` 要向对端发 SSH_MSG_DISCONNECT 并等它收尾。对端没响应时它
    /// 会一直等 —— 而「对端没响应」恰恰是用户点断开键的原因，所以这不是理论
    /// 情形，是最常见的情形。锁被无限期持有之后，`reconnect()` 会永久卡在
    /// `self.conn.lock().await`（第 79 行），所有依赖 `conn` 的操作一起卡住。
    ///
    /// 修法是利用 `conn` 本身是 `Arc`：先把 Arc 克隆出来、**放掉锁**，再在锁外
    /// 断开。于是锁的持有时间与网络无关。附带的好处是断开针对的是此刻的那个
    /// 连接对象：若同时有 `reconnect()` 换上了新连接，被断掉的是旧的那个，
    /// 新连接不会被误伤（原写法会把 reconnect 一起关在门外）。
    ///
    /// 断开本身仍然加一个上限：`close()` 的调用方在关窗，不该被一个失联主机
    /// 拖到无法退出。超时后直接返回 —— 连接对象随后随 `Arc` 一起释放。
    pub(crate) async fn close(&self) -> Result<()> {
        let _ = self.shutdown.send(true);
        // 作用域刻意收窄：`guard` 在这一行结束时就被释放，下面的 `await` 是
        // 在锁外进行的。写成 `Arc::clone(&self.conn.lock().await)` 会得到一个
        // 临时的 guard，其生命周期延续到整条语句结束 —— 也就是把锁又带进了
        // 下面那次 `await`，正是这次修改要消灭的东西。
        let conn = {
            let guard = self.conn.lock().await;
            Arc::clone(&guard)
        };
        let _ = tokio::time::timeout(
            DISCONNECT_TIMEOUT,
            conn.disconnect(russh::Disconnect::ByApplication, "session closed", "en"),
        )
        .await;
        Ok(())
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        // `keepalive_task` 原先是个**只写不读**的字段，而 `tokio::task::JoinHandle`
        // 被丢弃时只是 detach —— 任务继续跑。所以「握着句柄」这件事本身什么都没做，
        // 唯一真正停掉 keepalive 的是上面那个 shutdown 信号：任务在 `select!` 里
        // 等 `shutdown_rx.changed()`，信号到达即退出，连正在进行的 `send_ping()`
        // 也会被那个 select 一起取消。
        //
        // 那为什么不干脆删掉字段：`Drop` 是同步的，只能做 `abort()`。留着句柄并在
        // 这里 abort，把「会话没了 → keepalive 一定不再跑」变成一个不依赖任务恰好
        // 走到 select 点的硬保证。字段因此从摆设变成承重的，dead_code 警告也随之
        // 消失，而不是靠一行 allow 压下去。
        self.keepalive_task.abort();
    }
}

/// 终端方向命令：由持有完整 `russh::Channel` 的 PTY 任务消费。
pub(crate) enum PtyCmd {
    Write(Vec<u8>),
    Resize(u16, u16),
    Close,
}

/// One open PTY: commands in, output events out. The single task owning the
/// russh `Channel` lives in `backend::open_pty`; this handle carries the two ends.
pub(crate) struct PtyHandle {
    pub output_tx: tokio::sync::mpsc::UnboundedSender<PtyEvent>,
    pub commands: tokio::sync::mpsc::UnboundedSender<PtyCmd>,
    // `take_output` is intentionally synchronous because the public PTY trait is
    // synchronous at the subscription seam. A short std mutex avoids calling
    // Tokio's `blocking_lock` from inside an async forwarder (which can panic).
    receiver: StdMutex<Option<tokio::sync::mpsc::UnboundedReceiver<PtyEvent>>>,
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
                receiver: StdMutex::new(Some(receiver)),
            },
            commands_rx,
        )
    }

    /// The one output stream of this PTY (terminal owns exactly one subscriber).
    pub(crate) fn take_output(&self) -> tokio::sync::mpsc::UnboundedReceiver<PtyEvent> {
        let mut slot = self
            .receiver
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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

// `SftpHandle` 的构造器是 `backend.rs:368` 的 `new_some`，不在本模块 —— 因为
// `russh_sftp::SftpSession` 是 russh 类型，按 crate 头部约定「russh 类型绝不跨出
// `backend` 模块」，所以只有 `backend` 能造出它。本模块只定义形状。
//
// 这里原本另有一个 `SftpHandle::new()`（造一个 `sftp: None` 的句柄），从无调用者：
// 需要 SFTP 句柄的地方都已经有一个会话，必须用 `new_some`。空构造器是纯残骸，
// 此前被 crate 级的 `#![allow(dead_code)]` 罩着。已删除。
