//! PTY 通道：打开 shell、转发远端输出、写入与改尺寸。
//!
//! 这里只有自由函数与 [`RusshBackend`] 的固有方法；[`crate::SshBackend`] 的
//! 唯一一份实现留在 `backend::mod`，它把每个方法转发到下面这些函数。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use russh::client::Handle;
use russh::{Channel, ChannelMsg, Pty};

use super::error::map_send_err;
use super::hostkey::ConnHandler;
use super::{retry_transport_async, RusshBackend};
use crate::conn::PtyHandle;
use crate::{Error, PtyEvent, PtySession, Result, Session};

const PTY_CLOSE_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const PTY_FINAL_EVENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

impl RusshBackend {
    fn next_pty_id(&self) -> String {
        static PTY_TOKEN: AtomicU64 = AtomicU64::new(1);
        format!(
            "pty_{}_{}",
            std::process::id(),
            PTY_TOKEN.fetch_add(1, Ordering::Relaxed)
        )
    }
}

/// [`crate::SshBackend::open_pty`] 的实现体。
pub(super) async fn open_pty(
    backend: &RusshBackend,
    session: &Session,
    size: (u16, u16),
) -> Result<PtySession> {
    let (cols, rows) = size;
    let channel =
        retry_transport_async(session, |conn| open_pty_channel(conn, cols, rows), None).await?;
    let pty_id = backend.next_pty_id();

    let (pty, mut commands_rx) = PtyHandle::new();
    let output_tx = pty.output_tx.clone();

    // 单一任务持有完整 `Channel`：对外转发远端输出，对内消费写入/改尺寸命令。
    tokio::spawn(async move {
        let mut channel = channel;
        let mut exit_code = None;
        let (final_code, acknowledge, notify_closed) = loop {
            tokio::select! {
                command = commands_rx.recv() => {
                    let Some(command) = command else {
                        break (exit_code, None, false);
                    };
                    match command {
                        crate::conn::PtyCmd::Write(data) => {
                            if channel.data_bytes(data).await.is_err() {
                                break (exit_code, None, true);
                            }
                        }
                        crate::conn::PtyCmd::Resize(cols, rows) => {
                            let _ = channel
                                .window_change(u32::from(cols), u32::from(rows), 0, 0)
                                .await;
                        }
                        crate::conn::PtyCmd::Close(acknowledge) => {
                            break (exit_code, Some(acknowledge), true);
                        }
                    }
                }
                message = channel.wait() => {
                    match message {
                        None => break (exit_code, None, true),
                        // `.await` 是有意的：输出队列有界（`PTY_OUTPUT_CAPACITY`），
                        // 满了就在这里挂起，于是本任务不再 `channel.wait()`，russh 的
                        // 接收缓冲填满、TCP 窗口关闭，背压传回远端。远端刷屏时应当让
                        // 远端慢下来，而不是把无界数据堆在宿主内存里。
                        // 订阅者退出（终端已关）时 `send` 立刻返回 Err，照样 break。
                        Some(ChannelMsg::Data { data }) => {
                            if output_tx.send(PtyEvent::Output(data.to_vec())).await.is_err() {
                                break (exit_code, None, false); // 订阅者退出 = 终端已关
                            }
                        }
                        Some(ChannelMsg::ExtendedData { data, ext: 1 }) => {
                            if output_tx.send(PtyEvent::Output(data.to_vec())).await.is_err() {
                                break (exit_code, None, false);
                            }
                        }
                        Some(ChannelMsg::ExitStatus { exit_status }) => {
                            exit_code = Some(exit_status);
                        }
                        Some(ChannelMsg::Close | ChannelMsg::Eof) => {
                            break (exit_code, None, true);
                        }
                        Some(_) => {}
                    }
                }
            }
        };

        let _ = channel.close().await;
        if notify_closed {
            let _ = tokio::time::timeout(
                PTY_FINAL_EVENT_TIMEOUT,
                output_tx.send(PtyEvent::Closed { code: final_code }),
            )
            .await;
        }
        if let Some(acknowledge) = acknowledge {
            let _ = acknowledge.send(());
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

/// [`crate::SshBackend::pty_write`] 的实现体。
pub(super) fn pty_write(pty: &PtySession, data: &[u8]) -> Result<()> {
    pty.inner
        .commands
        .send(crate::conn::PtyCmd::Write(data.to_vec()))
        .map_err(|_| Error::Channel("pty is closed".into()))?;
    Ok(())
}

/// [`crate::SshBackend::pty_resize`] 的实现体。
pub(super) fn pty_resize(pty: &PtySession, cols: u16, rows: u16) -> Result<()> {
    pty.inner
        .commands
        .send(crate::conn::PtyCmd::Resize(cols, rows))
        .map_err(|_| Error::Channel("pty is closed".into()))?;
    Ok(())
}

/// [`crate::SshBackend::pty_close`] 的实现体。
pub(super) async fn pty_close(pty: &PtySession) -> Result<()> {
    let (acknowledge, closed) = tokio::sync::oneshot::channel();
    pty.inner
        .commands
        .send(crate::conn::PtyCmd::Close(acknowledge))
        .map_err(|_| Error::Channel("pty is closed".into()))?;
    // The PTY task may be blocked behind a full output queue or a dead transport.
    // Waiting is useful for observing completion, but it must not turn close into
    // an unbounded operation.
    let _ = tokio::time::timeout(PTY_CLOSE_ACK_TIMEOUT, closed).await;
    Ok(())
}

/// [`crate::SshBackend::pty_output`] 的实现体。
pub(super) fn pty_output(pty: &PtySession) -> tokio::sync::mpsc::Receiver<PtyEvent> {
    pty.inner.take_output()
}

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
