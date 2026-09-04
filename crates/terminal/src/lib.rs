//! yukinal-terminal — PTY Manager（数据流：xterm.js → Tauri IPC → 本 crate → SSH Channel → Remote PTY）。
//!
//! 职责：
//! - 每个会话一个稳定 `terminal_session_id`，多会话互不干扰；
//! - 所有会话的路由 / 事件统一成 [`TerminalAppEvent`]（桌面层只负责转发到 Tauri events）；
//! - 会话生命周期：open → write/resize 流式 → close；输出即来即推（broadcast，不积压）；
//! - reconnect：同一会话 id 下用新 pty 替换（`reopen`），xterm.js 缓冲区不丢。
//!
//! 本 crate 不接触凭据 / 服务器模型：pty 由上层（Rust core）经 [`TerminalPty`]
//! 注入。测试用内存 pty 覆盖全部路由逻辑，无需真机。

#![allow(dead_code)] // 待 server 工具落地后全面使用

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{broadcast, Mutex};
use yukinal_ssh::PtyEvent;

pub type Result<T> = std::result::Result<T, TerminalError>;

#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    #[error("terminal session `{0}` not found")]
    NotFound(String),
    #[error("terminal session `{0}` already exists")]
    Exists(String),
    #[error("terminal operation failed: {0}")]
    Channel(String),
}

/// 终端对 pty 的最小需求。`SshPty`（基于 `yukinal-ssh`）与测试用 `MemoryPty` 都实现它。
pub trait TerminalPty: Send + Sync {
    fn write(&self, data: &[u8]) -> impl std::future::Future<Output = Result<()>> + Send;
    fn resize(&self, cols: u16, rows: u16) -> impl std::future::Future<Output = Result<()>> + Send;
    /// 输出事件流（远端字节 / 关闭）。reopen 后新 pty 的新任务重新订阅。
    fn events(&self) -> tokio::sync::mpsc::UnboundedReceiver<PtyEvent>;
    fn close(&self) -> impl std::future::Future<Output = Result<()>> + Send;
}

/// 会话信息（列表 / 标题）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSessionInfo {
    pub terminal_session_id: String,
    pub server_id: String,
    pub cols: u16,
    pub rows: u16,
    pub opened_at: String,
}

/// 统一上抛事件；桌面层 1:1 转发为 Tauri events（`terminal.data` / `.opened` / `.closed`）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "name")]
pub enum TerminalAppEvent {
    Opened {
        payload: TerminalSessionInfo,
    },
    Data {
        terminal_session_id: String,
        /// 按 UTF-8 传输（交互式 shell 场景足够；非 UTF-8 程序后续换 base64）。
        data: String,
    },
    Closed {
        terminal_session_id: String,
    },
}

struct Session<P: TerminalPty> {
    info: TerminalSessionInfo,
    pty: Arc<P>,
}

/// 多会话 terminal 管理器。`subscribe` 的 broadcast 是 UI 唯一事件入口。
pub struct TerminalManager<P: TerminalPty> {
    sessions: Arc<Mutex<HashMap<String, Session<P>>>>,
    next_id: AtomicU64,
    events: broadcast::Sender<TerminalAppEvent>,
}

impl<P: TerminalPty + 'static> Default for TerminalManager<P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P: TerminalPty + 'static> TerminalManager<P> {
    #[must_use]
    pub fn new() -> Self {
        let (events, _) = broadcast::channel(1024);
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            events,
        }
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<TerminalAppEvent> {
        self.events.subscribe()
    }

    /// 注册新会话并启动输出转发任务。`pty` 由上层建好传入。
    pub async fn open(&self, server_id: &str, cols: u16, rows: u16, pty: P) -> Result<String> {
        let session_id = self.next_id();
        let info = TerminalSessionInfo {
            terminal_session_id: session_id.clone(),
            server_id: server_id.to_string(),
            cols,
            rows,
            opened_at: iso8601_now(),
        };

        let mut sessions = self.sessions.lock().await;
        if sessions.contains_key(&session_id) {
            return Err(TerminalError::Exists(session_id));
        }
        sessions.insert(
            session_id.clone(),
            Session {
                info,
                pty: Arc::new(pty),
            },
        );

        let _ = self.events.send(TerminalAppEvent::Opened {
            payload: sessions
                .get(&session_id)
                .expect("just inserted")
                .info
                .clone(),
        });
        self.spawn_forwarder(session_id.clone());
        Ok(session_id)
    }

    /// 同一会话 id 下替换 pty（断线重连后 xterm 缓冲区保留）。
    pub async fn reopen(
        &self,
        terminal_session_id: &str,
        cols: u16,
        rows: u16,
        pty: P,
    ) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        let session = sessions
            .get_mut(terminal_session_id)
            .ok_or_else(|| TerminalError::NotFound(terminal_session_id.to_string()))?;
        session.pty = Arc::new(pty);
        session.info.cols = cols;
        session.info.rows = rows;

        let session_id = terminal_session_id.to_string();
        let info = session.info.clone();
        drop(sessions);
        let _ = self.events.send(TerminalAppEvent::Opened {
            payload: info.clone(),
        });
        let _ = info;
        self.spawn_forwarder(session_id);

        // 传输上的 re-opened 与 opened 事件同形；UI 用同一逻辑重挂数据流。
        Ok(())
    }

    pub async fn write(&self, terminal_session_id: &str, data: &[u8]) -> Result<()> {
        let pty = self.pty_for(terminal_session_id).await?;
        pty.write(data).await
    }

    pub async fn resize(&self, terminal_session_id: &str, cols: u16, rows: u16) -> Result<()> {
        let pty = self.pty_for(terminal_session_id).await?;
        pty.resize(cols, rows).await
    }

    /// 锁内只取 `Arc`，锁外执行，避免把会话锁带过 await。
    async fn pty_for(&self, terminal_session_id: &str) -> Result<Arc<P>> {
        self.sessions
            .lock()
            .await
            .get(terminal_session_id)
            .map(|session| session.pty.clone())
            .ok_or_else(|| TerminalError::NotFound(terminal_session_id.to_string()))
    }

    /// 关闭会话（唯一删除路径）：`pty.close()` + 移除路由 + 上抛 Closed。
    pub async fn close(&self, terminal_session_id: &str) -> Result<()> {
        let pty = {
            let mut sessions = self.sessions.lock().await;
            sessions
                .remove(terminal_session_id)
                .map(|session| session.pty)
                .ok_or_else(|| TerminalError::NotFound(terminal_session_id.to_string()))?
        };
        pty.close().await?;
        let _ = self.events.send(TerminalAppEvent::Closed {
            terminal_session_id: terminal_session_id.to_string(),
        });
        Ok(())
    }

    /// Close every terminal belonging to one server. Disconnecting a server
    /// must not leave PTYs alive against a session that the user believes is
    /// closed.
    pub async fn close_for_server(&self, server_id: &str) -> Result<usize> {
        let ids: Vec<String> = self
            .list()
            .await
            .into_iter()
            .filter(|info| info.server_id == server_id)
            .map(|info| info.terminal_session_id)
            .collect();
        let count = ids.len();
        for id in ids {
            self.close(&id).await?;
        }
        Ok(count)
    }

    pub async fn list(&self) -> Vec<TerminalSessionInfo> {
        let sessions = self.sessions.lock().await;
        let mut infos: Vec<_> = sessions
            .values()
            .map(|session| session.info.clone())
            .collect();
        infos.sort_by(|a, b| a.terminal_session_id.cmp(&b.terminal_session_id));
        infos
    }

    pub async fn get(&self, terminal_session_id: &str) -> Option<TerminalSessionInfo> {
        self.sessions
            .lock()
            .await
            .get(terminal_session_id)
            .map(|session| session.info.clone())
    }

    fn next_id(&self) -> String {
        format!(
            "t_{}_{}",
            std::process::id(),
            self.next_id.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// 每会话一个转发任务：pty 输出 → Data；Closed → 上抛（不做删除，
    /// 删除权归 `close()`，避免 reopen 与任务移除互踩）。
    fn spawn_forwarder(&self, terminal_session_id: String) {
        let sessions = Arc::clone(&self.sessions);
        let events = self.events.clone();
        tokio::spawn(async move {
            let receiver = {
                let guard = sessions.lock().await;
                let Some(session) = guard.get(&terminal_session_id) else {
                    return;
                };
                session.pty.events()
            };
            let mut receiver = receiver;
            loop {
                match receiver.recv().await {
                    Some(PtyEvent::Output(bytes)) => {
                        let data = String::from_utf8_lossy(&bytes).into_owned();
                        if events
                            .send(TerminalAppEvent::Data {
                                terminal_session_id: terminal_session_id.clone(),
                                data,
                            })
                            .is_err()
                        {
                            break; // 无订阅者（UI 已关），任务结束
                        }
                    }
                    Some(PtyEvent::Closed { .. }) | None => {
                        let _ = events.send(TerminalAppEvent::Closed {
                            terminal_session_id: terminal_session_id.clone(),
                        });
                        break;
                    }
                }
            }
        });
    }
}

impl<P: TerminalPty> fmt::Debug for TerminalManager<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalManager").finish_non_exhaustive()
    }
}

/// ISO-8601 UTC（显示用）。与 core 侧同一套日历算法（civil_from_days），
/// 保持所有时间戳格式一致。
fn iso8601_now() -> String {
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|delta| delta.as_secs())
        .unwrap_or(0);
    iso8601_utc(epoch)
}

fn iso8601_utc(epoch: u64) -> String {
    let days = (epoch / 86_400) as i64;
    let time_of_day = epoch % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time_of_day / 3_600,
        (time_of_day % 3_600) / 60,
        time_of_day % 60
    )
}

fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let shifted = days_since_epoch + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = (shifted - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// 内存 pty：记录写入/改尺寸，输出可脚本化注入。
    struct MemoryPty {
        written: StdMutex<Vec<Vec<u8>>>,
        resizes: StdMutex<Vec<(u16, u16)>>,
        output_tx: tokio::sync::mpsc::UnboundedSender<PtyEvent>,
        closed: StdMutex<bool>,
    }

    impl MemoryPty {
        fn new() -> Self {
            let (output_tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            Self {
                written: StdMutex::new(Vec::new()),
                resizes: StdMutex::new(Vec::new()),
                output_tx,
                closed: StdMutex::new(false),
            }
        }
    }

    impl TerminalPty for MemoryPty {
        async fn write(&self, data: &[u8]) -> Result<()> {
            self.written.lock().expect("lock").push(data.to_vec());
            Ok(())
        }
        async fn resize(&self, cols: u16, rows: u16) -> Result<()> {
            self.resizes.lock().expect("lock").push((cols, rows));
            Ok(())
        }
        fn events(&self) -> tokio::sync::mpsc::UnboundedReceiver<PtyEvent> {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let _ = tx; // 第二次订阅是空的（与 SshPty 的 take_output 语义一致）
            rx
        }
        async fn close(&self) -> Result<()> {
            *self.closed.lock().expect("lock") = true;
            Ok(())
        }
    }

    /// 带"可注入脚本"的 pty：events() 给出与 emit() 同一信道（模拟 SshPty 的一次性订阅）。
    struct ScriptedMemoryPty {
        output_tx: tokio::sync::mpsc::UnboundedSender<PtyEvent>,
        receiver: StdMutex<Option<tokio::sync::mpsc::UnboundedReceiver<PtyEvent>>>,
        written: StdMutex<Vec<Vec<u8>>>,
    }

    impl ScriptedMemoryPty {
        fn new() -> Self {
            let (output_tx, rx) = tokio::sync::mpsc::unbounded_channel();
            Self {
                output_tx,
                receiver: StdMutex::new(Some(rx)),
                written: StdMutex::new(Vec::new()),
            }
        }
    }

    impl TerminalPty for ScriptedMemoryPty {
        async fn write(&self, data: &[u8]) -> Result<()> {
            self.written.lock().expect("lock").push(data.to_vec());
            Ok(())
        }
        async fn resize(&self, _cols: u16, _rows: u16) -> Result<()> {
            Ok(())
        }
        fn events(&self) -> tokio::sync::mpsc::UnboundedReceiver<PtyEvent> {
            self.receiver
                .lock()
                .expect("lock")
                .take()
                .unwrap_or_else(|| {
                    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
                    rx
                })
        }
        async fn close(&self) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn opens_multiple_sessions_and_routes_commands() {
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let manager = TerminalManager::<MemoryPty>::new();

        rt.block_on(async {
            let id_a = manager
                .open("srv_1", 120, 30, MemoryPty::new())
                .await
                .expect("open a");
            let id_b = manager
                .open("srv_1", 80, 24, MemoryPty::new())
                .await
                .expect("open b");
            assert_ne!(id_a, id_b);

            manager.write(&id_a, b"ls\r").await.expect("write a");
            manager.resize(&id_b, 100, 40).await.expect("resize b");
            assert_eq!(manager.list().await.len(), 2);

            // 写/改尺寸按会话隔离。
            let info_a = manager.get(&id_a).await.expect("get a");
            assert_eq!((info_a.cols, info_a.rows), (120, 30));

            manager.close(&id_a).await.expect("close a");
            assert!(matches!(
                manager.write(&id_a, b"x").await,
                Err(TerminalError::NotFound(_))
            ));
            assert_eq!(manager.list().await.len(), 1);
        });
    }

    #[test]
    fn reopen_keeps_session_id_and_buffer() {
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let manager = TerminalManager::<MemoryPty>::new();

        rt.block_on(async {
            let id = manager
                .open("srv_1", 120, 30, MemoryPty::new())
                .await
                .expect("open");
            manager
                .reopen(&id, 100, 40, MemoryPty::new())
                .await
                .expect("reopen");
            let info = manager.get(&id).await.expect("still exists");
            assert_eq!((info.cols, info.rows), (100, 40));
        });
    }

    #[test]
    fn unknown_session_reports_not_found() {
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let manager = TerminalManager::<MemoryPty>::new();
        rt.block_on(async {
            assert!(matches!(
                manager.write("t_missing", b"x").await,
                Err(TerminalError::NotFound(_))
            ));
            assert!(matches!(
                manager.resize("t_missing", 1, 1).await,
                Err(TerminalError::NotFound(_))
            ));
            assert!(matches!(
                manager.close("t_missing").await,
                Err(TerminalError::NotFound(_))
            ));
        });
    }

    #[test]
    fn close_for_server_only_closes_matching_sessions() {
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let manager = TerminalManager::<MemoryPty>::new();
        rt.block_on(async {
            let matching = manager
                .open("srv_a", 80, 24, MemoryPty::new())
                .await
                .expect("open");
            let other = manager
                .open("srv_b", 80, 24, MemoryPty::new())
                .await
                .expect("open");
            assert_eq!(manager.close_for_server("srv_a").await.expect("close"), 1);
            assert!(manager.get(&matching).await.is_none());
            assert!(manager.get(&other).await.is_some());
        });
    }

    #[test]
    fn broadcast_carries_opened_data_closed() {
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let manager = TerminalManager::<MemoryPty>::new();
        let mut rx = manager.subscribe();

        let id = rt.block_on(async {
            manager
                .open("srv_1", 120, 30, MemoryPty::new())
                .await
                .expect("open")
        });
        rt.block_on(async {
            manager.close(&id).await.expect("close");
        });

        let events: Vec<_> = (0..2).filter_map(|_| rx.blocking_recv().ok()).collect();
        assert!(
            matches!(&events[0], TerminalAppEvent::Opened { payload } if payload.server_id == "srv_1")
        );
        assert!(
            matches!(&events[1], TerminalAppEvent::Closed { terminal_session_id } if terminal_session_id == &id)
        );
    }

    #[test]
    fn output_events_are_forwarded_to_subscribers() {
        let rt = tokio::runtime::Runtime::new().expect("rt");
        rt.block_on(async {
            let manager = TerminalManager::<ScriptedMemoryPty>::new();
            let mut rx = manager.subscribe();
            let pty = ScriptedMemoryPty::new();
            let emitter = pty.output_tx.clone();
            let id = manager.open("srv_1", 120, 30, pty).await.expect("open");

            let _ = emitter.send(PtyEvent::Output(b"hello\r\n".to_vec()));

            match rx.recv().await.expect("event 1") {
                TerminalAppEvent::Opened { .. } => {}
                other => panic!("expected Opened, got {other:?}"),
            }
            match rx.recv().await.expect("event 2") {
                TerminalAppEvent::Data {
                    terminal_session_id,
                    data,
                } => {
                    assert_eq!(id, terminal_session_id);
                    assert_eq!(data, "hello\r\n");
                }
                other => panic!("expected Data, got {other:?}"),
            }

            // 远端关闭 → Closed 上抛。
            let _ = emitter.send(PtyEvent::Closed { code: Some(0) });
            match rx.recv().await.expect("event 3") {
                TerminalAppEvent::Closed {
                    terminal_session_id,
                } => assert_eq!(id, terminal_session_id),
                other => panic!("expected Closed, got {other:?}"),
            }
        });
    }

    #[test]
    fn iso8601_matches_reference() {
        assert_eq!(iso8601_utc(1_700_000_000), "2023-11-14T22:13:20Z");
    }
}
