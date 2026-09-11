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
    /// 这里**不能**握着 `conn` 锁去 `await` 断开过程。原来的写法是：先
    /// `let conn = self.conn.lock().await;`，紧接着 `let _ = conn.disconnect(...).await;`
    /// —— 无超时，且锁一直被持有。
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

/// PTY 输出队列的容量，单位是**事件**（每个 `PtyEvent::Output` 大致对应一个 russh
/// `ChannelMsg::Data`，通常 ≤32 KiB，所以最坏约 8 MiB 在途）。
///
/// 为什么这里必须有界，而命令方向可以无界：
///
/// 输出量由**远端**决定，不受本进程控制也不受信任。`cat bigfile`、`yes`、日志 tail
/// 都能以远超消费速度的速率产出数据。无界队列在这种情况下只会一路增长，把远端的问题
/// 变成宿主进程的内存问题 —— 而且增长是静默的，直到 OOM 才可见。有界队列则让生产端
/// 的 `send().await` 阻塞，于是那个持有 `russh::Channel` 的任务停止 `channel.wait()`，
/// russh 的接收缓冲填满，TCP 窗口关闭，背压一路传回远端。这正是终端应该有的流量控制：
/// 慢的消费端应当让远端慢下来，而不是把数据堆在本地。
///
/// 命令方向（`PtyCmd`）保持无界，是因为它的量由**用户**决定：按键与粘贴，每条都很小，
/// 速率有物理上限。若把它也改成有界，`pty_write` 就会与输出排空进度耦合 —— 远端刷屏
/// 时按键会一起卡住。输出慢只应让远端变慢，不应让键盘失灵。
pub(crate) const PTY_OUTPUT_CAPACITY: usize = 256;

/// One open PTY: commands in, output events out. The single task owning the
/// russh `Channel` lives in `backend::open_pty`; this handle carries the two ends.
pub(crate) struct PtyHandle {
    pub output_tx: tokio::sync::mpsc::Sender<PtyEvent>,
    pub commands: tokio::sync::mpsc::UnboundedSender<PtyCmd>,
    // `take_output` is intentionally synchronous because the public PTY trait is
    // synchronous at the subscription seam. A short std mutex avoids calling
    // Tokio's `blocking_lock` from inside an async forwarder (which can panic).
    receiver: StdMutex<Option<tokio::sync::mpsc::Receiver<PtyEvent>>>,
}

impl PtyHandle {
    #[must_use]
    pub fn new() -> (Self, tokio::sync::mpsc::UnboundedReceiver<PtyCmd>) {
        let (output_tx, receiver) = tokio::sync::mpsc::channel(PTY_OUTPUT_CAPACITY);
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
    pub(crate) fn take_output(&self) -> tokio::sync::mpsc::Receiver<PtyEvent> {
        let mut slot = self
            .receiver
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        slot.take().unwrap_or_else(|| {
            // Not supposed to happen twice; a fresh silent receiver keeps
            // callers from panicking on misuse.
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 输出信道必须是**有界**的，而且生产端真的会在满的时候挂起。
    ///
    /// 这条测试存在的理由：把 `channel(N)` 换回 `unbounded_channel()` 不会让任何现有测试
    /// 失败，也不会产生编译错误 —— 两者的 `send` 在「订阅者在」时都成功。也就是说，这个
    /// 约束**只靠注释是守不住的**，必须有东西在它退化时变红。
    ///
    /// 断言的是行为而不是常量：先灌满队列，再多发一条，然后确认那一条**没有**完成。
    /// 如果哪天有人改回无界，`send` 会立刻返回，`timeout` 就不会超时，这条测试失败。
    #[tokio::test]
    async fn pty_output_channel_is_bounded_and_backpressures() {
        let (pty, _commands_rx) = PtyHandle::new();
        let mut receiver = pty.take_output();

        // 灌满。容量是常量，测试跟着它走，不硬编码 256。
        for index in 0..PTY_OUTPUT_CAPACITY {
            pty.output_tx
                .send(PtyEvent::Output(vec![u8::try_from(index % 256).unwrap()]))
                .await
                .expect("filling a channel with a live receiver must succeed");
        }

        // 第 N+1 条必须阻塞：这正是背压。给它一个很短的期限，超时即为通过。
        let over_capacity = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            pty.output_tx
                .send(PtyEvent::Output(b"one too many".to_vec())),
        )
        .await;
        assert!(
            over_capacity.is_err(),
            "队列满时 send 竟然完成了：输出信道已经不是有界的，远端刷屏会无限制地堆在宿主内存里",
        );

        // 消费一条就该腾出位置 —— 证明刚才只是「等待」而不是「死锁」。
        assert!(receiver.recv().await.is_some());
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            pty.output_tx
                .send(PtyEvent::Output(b"now there is room".to_vec())),
        )
        .await
        .expect("draining one event must unblock the producer")
        .expect("receiver is still alive");
    }

    /// 订阅者消失后，生产端必须立刻拿到 `Err` 而不是永久挂起。
    ///
    /// 这是上一条的另一半：有界队列让 `send` 会等待，那么「等待一个永远不会来的消费者」
    /// 就成了新的风险。`mpsc::Sender` 在接收端被丢弃时会立刻返回 `Err`，`backend` 的 PTY
    /// 任务据此 break。这条测试把这个前提钉住 —— 它是上面那个 `.await` 能安全存在的基础。
    #[tokio::test]
    async fn send_fails_fast_once_the_subscriber_is_gone() {
        let (pty, _commands_rx) = PtyHandle::new();
        let receiver = pty.take_output();
        drop(receiver);

        let result = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            pty.output_tx
                .send(PtyEvent::Output(b"nobody is listening".to_vec())),
        )
        .await
        .expect("接收端已丢弃，send 必须立刻返回而不是挂起");

        assert!(
            result.is_err(),
            "接收端已丢弃，send 却成功了：PTY 任务将无法感知终端已关闭",
        );
    }

    /// 第二次 `take_output` 给一个「沉默」的接收端，而不是 panic。
    ///
    /// 记录的是**有意的**降级选择：公开的订阅接缝是同步的（`SshBackend::pty_output`），
    /// 所以这里用 `std::sync::Mutex` 而不是 `blocking_lock`。误用两次订阅的代价是「收不到
    /// 输出」，而不是整个进程崩掉。
    ///
    /// 具体语义（第一版这条测试写错了，这里按实际行为写清楚）：第二次返回的那个接收端，
    /// 它的发送端在 `take_output` 内部就被丢弃了，所以它是一个**已关闭**的空流 ——
    /// `recv()` 立刻返回 `None`，而不是永远挂起。真实的输出仍然只走第一次拿到的那个。
    #[tokio::test]
    async fn a_second_subscription_yields_a_silent_receiver() {
        let (pty, _commands_rx) = PtyHandle::new();
        let mut first = pty.take_output();
        let mut second = pty.take_output();

        // 第二个：空流，立刻结束。
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_millis(50), second.recv())
                .await
                .expect("沉默接收端应当立刻返回 None，而不是挂起"),
            None,
        );

        // 第一个仍然是真实的那条：发进去的事件能收到，且没有 panic。
        pty.output_tx
            .send(PtyEvent::Output(b"real".to_vec()))
            .await
            .expect("第一个接收端还活着");
        assert_eq!(
            first.recv().await,
            Some(PtyEvent::Output(b"real".to_vec())),
            "真实输出应当仍然走第一次订阅拿到的接收端",
        );
    }
}
