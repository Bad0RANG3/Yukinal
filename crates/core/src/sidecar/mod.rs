//! Sidecar supervision: Rust owns the agent process (ADR 0001/0006/0008).
//!
//! The desktop never spawns processes itself. This module resolves *what* to launch,
//! launches it with piped stdio, speaks NDJSON JSON-RPC, correlates responses, forwards
//! notifications/logs, and guarantees no orphan is left behind (`kill_on_drop` +
//! explicit [`SidecarHandle::shutdown`]).
//!
//! ## 为什么拆成三个文件
//!
//! 这个模块原本是一个 858 行的 `sidecar.rs`，里面同时住着五件互不相干的事：解析启动
//! 目标、spawn 子进程并泵三个 stdio、JSON-RPC 收发与请求关联、日志脱敏、以及握手协商。
//! 全部挤在一起时没有任何东西标出边界，于是"重复的日历算法"和"被 `allow(dead_code)`
//! 罩住的死项"都在这里长出来过 —— 那不是偶然，而是没有边界的必然结果。
//!
//! 现在：
//!
//! - [`config`]：**启动什么**（路径/环境解析，纯逻辑，不碰 tokio）
//! - [`crate::redact`]：一行不可信文本 → 可以安全写进日志的东西（本 crate 的唯一安全能力，
//!   已搬到 crate 根，因为 MCP 客户端也需要它）
//! - 本文件：进程生命周期与线协议 —— spawn / 握手 / 请求关联 / 事件转发
//!
//! 公开路径完全不变：`SidecarConfig` 等仍从 `yukinal_core::sidecar::` 导出（见下面的
//! 再导出），所以 `crates/core/src/supervisor.rs` 与 `apps/desktop/src-tauri` 下那些
//! `sidecar::X` 调用一处都不用改。

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, oneshot, Mutex as AsyncMutex};

mod config;

/// 启动配置仍从这里导出 —— 路径与拆分类名前一致，调用方无需知道它换了文件。
pub use config::SidecarConfig;
// 脱敏逻辑住在 crate 根（`crate::redact`），搬出去的起因是 MCP 客户端也要用它：
// 同一个 crate 内两处处理不可信子进程文本，就必须共用一份实现，否则第二份会漂移。
use crate::redact::{redact_log_line, redact_process_log_line, truncate};

/// Must match `YUKINAL_RPC_VERSION` in `@yukinal/shared` (ADR 0006). A mismatch is
/// refused instead of treated as "probably compatible": half-speaking protocols are how
/// a permission decision ends up evaluated against the wrong payload shape.
pub const PROTOCOL_VERSION: &str = "1.0";

/// JSON-RPC request ids are local to this supervisor; the agent never allocates ids.
const EVENT_CHANNEL_CAPACITY: usize = 256;
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(200);
const NODE_VERSION_TIMEOUT: Duration = Duration::from_secs(5);
/// CREATE_NO_WINDOW, so the sidecar never flashes a console on Windows.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, thiserror::Error)]
pub enum SidecarError {
    #[error(
        "no agent bundle to launch (searched {searched}); run `pnpm --filter @yukinal/agent build`"
    )]
    NotFound { searched: String },
    #[error("failed to launch agent sidecar: {0}")]
    Launch(String),
    #[error("agent sidecar is not running")]
    NotRunning,
    #[error("{method} did not answer within {timeout:?}")]
    Timeout { method: String, timeout: Duration },
    #[error("agent sidecar answered with an error: {0}")]
    Remote(String),
    #[error("could not write to the agent sidecar: {0}")]
    Write(String),
    #[error("agent sidecar sent a frame we could not parse: {0}")]
    Frame(String),
}

/// A frame or lifecycle notice worth showing the user.
#[derive(Debug, Clone)]
pub enum SidecarEvent {
    /// JSON-RPC notification from the agent (`agent.stream`, ).
    Frame(Value),
    /// JSON-RPC request from the sidecar to the Rust host.
    Request {
        id: i64,
        method: String,
        params: Value,
    },
    /// A stderr line from the sidecar process.
    Log(String),
    Exited {
        code: Option<i32>,
        signal: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct SidecarInfo {
    pub pid: u32,
    pub entry: String,
    pub started_at: String,
}

#[derive(Debug)]
struct Inner {
    info: SidecarInfo,
    stdin: AsyncMutex<ChildStdin>,
    child: AsyncMutex<Child>,
    pending: Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>,
    events: broadcast::Sender<SidecarEvent>,
    next_id: AtomicI64,
    exited: AtomicBool,
}

/// Cheap to clone: all clones address the same process.
#[derive(Debug, Clone)]
pub struct SidecarHandle {
    inner: Arc<Inner>,
}

impl SidecarHandle {
    pub fn info(&self) -> SidecarInfo {
        self.inner.info.clone()
    }

    pub fn is_running(&self) -> bool {
        !self.inner.exited.load(Ordering::Relaxed)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SidecarEvent> {
        self.inner.events.subscribe()
    }

    /// Send a request and await the matching response frame, returning its `result`.
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, SidecarError> {
        if !self.is_running() {
            return Err(SidecarError::NotRunning);
        }

        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let frame = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let (tx, rx) = oneshot::channel();
        self.inner
            .pending
            .lock()
            .map_err(|_| SidecarError::NotRunning)?
            .insert(id, tx);

        let payload =
            serde_json::to_vec(&frame).map_err(|error| SidecarError::Frame(error.to_string()))?;
        {
            let mut stdin = self.inner.stdin.lock().await;
            let mut buffer = payload;
            buffer.push(b'\n');
            if let Err(error) = stdin.write_all(&buffer).await {
                self.forget(id);
                return Err(SidecarError::Write(error.to_string()));
            }
            if let Err(error) = stdin.flush().await {
                self.forget(id);
                return Err(SidecarError::Write(error.to_string()));
            }
        }

        match tokio::time::timeout(timeout, rx).await {
            Err(_) => {
                self.forget(id);
                Err(SidecarError::Timeout {
                    method: method.to_string(),
                    timeout,
                })
            }
            Ok(Err(_)) => {
                self.forget(id);
                Err(SidecarError::NotRunning)
            }
            Ok(Ok(Err(message))) => Err(SidecarError::Remote(redact_log_line(&message))),
            Ok(Ok(Ok(result))) => Ok(result),
        }
    }

    /// Answer a JSON-RPC request that originated in the sidecar.
    pub async fn respond(
        &self,
        id: i64,
        outcome: Result<Value, String>,
    ) -> Result<(), SidecarError> {
        if !self.is_running() {
            return Err(SidecarError::NotRunning);
        }

        let frame = match outcome {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(message) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32603, "message": message }
            }),
        };
        let mut payload =
            serde_json::to_vec(&frame).map_err(|error| SidecarError::Frame(error.to_string()))?;
        payload.push(b'\n');

        let mut stdin = self.inner.stdin.lock().await;
        stdin
            .write_all(&payload)
            .await
            .map_err(|error| SidecarError::Write(error.to_string()))?;
        stdin
            .flush()
            .await
            .map_err(|error| SidecarError::Write(error.to_string()))
    }

    /// Ask the sidecar to exit politely; the process is killed if it does not.
    pub async fn shutdown(&self) {
        // Closing our ability to send more work comes first: requests in flight fail
        // fast instead of hanging until a timeout.
        self.inner.exited.store(true, Ordering::Relaxed);
        let mut child = self.inner.child.lock().await;
        // start_kill() signals without consuming the child, so we can still reap it.
        let _ = child.start_kill();
        let _ = child.wait().await;
    }

    fn forget(&self, id: i64) {
        if let Ok(mut pending) = self.inner.pending.lock() {
            pending.remove(&id);
        }
    }

    fn resolve(&self, id: i64, outcome: Result<Value, String>) {
        let sender = match self.inner.pending.lock() {
            Ok(mut pending) => pending.remove(&id),
            Err(_) => None,
        };
        if let Some(sender) = sender {
            // A closed receiver means the caller already timed out; nothing to do.
            let _ = sender.send(outcome);
        }
    }

    fn broadcast(&self, event: SidecarEvent) {
        // No subscriber yet is normal (e.g. during startup); never an error.
        let _ = self.inner.events.send(event);
    }
}

/// Launch the sidecar. The returned handle has *not* been initialized -- callers that
/// need a handshake should call `initialize` explicitly (see `crate::commands`).
pub async fn spawn(config: &SidecarConfig) -> Result<SidecarHandle, SidecarError> {
    ensure_node_prerequisite(config).await?;
    let mut command = Command::new(&config.program);
    command
        .args(&config.args)
        .envs(config.env.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let mut child = command
        .spawn()
        .map_err(|error| config.launch_error(&error))?;
    // Rust 1.98 returns the pid as u32 already; no conversion, no silent fallback.
    let pid = child
        .id()
        .ok_or_else(|| SidecarError::Launch("child has no pid".to_string()))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| SidecarError::Launch("stdin was not piped".to_string()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| SidecarError::Launch("stdout was not piped".to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| SidecarError::Launch("stderr was not piped".to_string()))?;

    let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
    let handle = SidecarHandle {
        inner: Arc::new(Inner {
            info: SidecarInfo {
                pid,
                entry: config.entry_label.clone(),
                started_at: iso8601_utc(yukinal_time::now_epoch_seconds()),
            },
            stdin: AsyncMutex::new(stdin),
            child: AsyncMutex::new(child),
            pending: Mutex::new(HashMap::new()),
            events: events.clone(),
            next_id: AtomicI64::new(1),
            exited: AtomicBool::new(false),
        }),
    };

    // stdout: NDJSON frames -> pending responses or forwarded notifications (ADR 0006).
    let reader = handle.clone();
    let mut lines = BufReader::new(stdout).lines();
    tokio::spawn(async move {
        let mut inside_private_key = false;
        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(line) {
                Ok(frame) => reader.dispatch(frame),
                Err(error) => reader.broadcast(SidecarEvent::Log(format!(
                    "dropped non-JSON stdout line ({error}): {}",
                    redact_process_log_line(&truncate(line), &mut inside_private_key)
                ))),
            }
        }
    });

    // stderr is the sidecar's log channel; surface it, never swallow it.
    let logger = handle.clone();
    let mut err_lines = BufReader::new(stderr).lines();
    tokio::spawn(async move {
        let mut inside_private_key = false;
        while let Ok(Some(line)) = err_lines.next_line().await {
            logger.broadcast(SidecarEvent::Log(redact_process_log_line(
                &line,
                &mut inside_private_key,
            )));
        }
    });

    // Exit watcher doubles as the reaper, so the child never becomes a zombie.
    let watcher = handle.clone();
    tokio::spawn(async move {
        loop {
            let status = {
                let mut child = watcher.inner.child.lock().await;
                child.try_wait().ok().flatten()
            };
            if let Some(status) = status {
                watcher.inner.exited.store(true, Ordering::Relaxed);
                if let Ok(mut pending) = watcher.inner.pending.lock() {
                    for (_, sender) in pending.drain() {
                        let _ = sender.send(Err("agent sidecar exited".to_string()));
                    }
                }
                watcher.broadcast(SidecarEvent::Exited {
                    code: status.code(),
                    signal: exit_signal(&status),
                });
                break;
            }
            tokio::time::sleep(EXIT_POLL_INTERVAL).await;
        }
    });

    Ok(handle)
}

/// Refuse an unsupported Node before loading the ESM bundle.
///
/// The check is deliberately skipped for `YUKINAL_AGENT_COMMAND`: that path may point to
/// a wrapper or another runtime, so this crate cannot infer that `--version` has Node
/// semantics. Missing and non-executable normal Node paths still use the same actionable
/// launch error as a failed spawn.
async fn ensure_node_prerequisite(config: &SidecarConfig) -> Result<(), SidecarError> {
    if !config.requires_node {
        return Ok(());
    }
    let mut command = Command::new(&config.program);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let output = match tokio::time::timeout(NODE_VERSION_TIMEOUT, command.output()).await {
        Err(_) => {
            return Err(SidecarError::Launch(format!(
                "timed out while checking the Node.js version from `{} --version`",
                config.program.display()
            )));
        }
        Ok(Err(error)) => return Err(config.launch_error(&error)),
        Ok(Ok(output)) => output,
    };
    if !output.status.success() {
        return Err(SidecarError::Launch(format!(
            "`{} --version` exited with {}; Node.js {} or newer is required",
            config.program.display(),
            output
                .status
                .code()
                .map_or_else(|| "a signal".to_string(), |code| format!("code {code}")),
            config::REQUIRED_NODE_MAJOR
        )));
    }
    config.validate_node_version(&String::from_utf8_lossy(&output.stdout))?;
    Ok(())
}

impl SidecarHandle {
    fn dispatch(&self, frame: Value) {
        let id = frame.get("id").and_then(Value::as_i64);
        let Some(id) = id else {
            self.broadcast(SidecarEvent::Frame(frame));
            return;
        };
        if let Some(method) = frame.get("method").and_then(Value::as_str) {
            self.broadcast(SidecarEvent::Request {
                id,
                method: method.to_string(),
                params: frame.get("params").cloned().unwrap_or(Value::Null),
            });
            return;
        }
        let outcome = match frame.get("error") {
            Some(error) => Err(redact_log_line(
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("agent error"),
            )),
            None => Ok(frame.get("result").cloned().unwrap_or(Value::Null)),
        };
        self.resolve(id, outcome);
    }
}

#[cfg(unix)]
fn exit_signal(status: &std::process::ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;
    status.signal().map(|signal| format!("signal {signal}"))
}

#[cfg(not(unix))]
fn exit_signal(_status: &std::process::ExitStatus) -> Option<String> {
    None
}

/// Wall clock as ISO-8601 UTC, for supervisor-side rows (start/exit records).
///
/// 实现已移到 `yukinal-time`（见该 crate 头部：这段逻辑原本在四个地方各写了一遍）。
/// 这里保留同名的再导出，是因为 `yukinal_core::sidecar::iso8601_now()` 已被
/// `apps/desktop/src-tauri/src/commands/` 下的二十来处调用，换路径没有收益、
/// 只有噪音。
pub use yukinal_time::{iso8601_now, iso8601_utc};

/// Spawn **and** handshake. An un-initialized sidecar is not usable by the desktop
/// (`initialize` must be the first call, ADR 0006), so the two steps belong together;
/// a failed handshake kills the process instead of leaving it half-alive.
pub async fn launch(config: &SidecarConfig) -> Result<LaunchedSidecar, SidecarError> {
    let handle = spawn(config).await?;
    match handshake(&handle, config).await {
        Ok(launched) => Ok(launched),
        Err(error) => {
            handle.shutdown().await;
            Err(error)
        }
    }
}

pub async fn handshake(
    handle: &SidecarHandle,
    config: &SidecarConfig,
) -> Result<LaunchedSidecar, SidecarError> {
    let initialized = handle
        .request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "clientVersion": config.client_version,
                "dataDir": config.data_dir,
            }),
            config.request_timeout,
        )
        .await?;

    let protocol_version = required_str(&initialized, "protocolVersion")?;
    if protocol_version != PROTOCOL_VERSION {
        return Err(SidecarError::Remote(format!(
            "protocol mismatch: sidecar answers {protocol_version}, desktop speaks {PROTOCOL_VERSION}"
        )));
    }
    let agent_version =
        required_str(&initialized, "agentVersion").unwrap_or_else(|_| "unknown".to_string());

    let described = handle
        .request("system.describe", json!({}), config.request_timeout)
        .await?;
    let tool_count = described
        .get("toolCount")
        .and_then(Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or(0);
    let collisions = described
        .get("toolNameCollisions")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if collisions > 0 {
        return Err(SidecarError::Remote(format!(
            "sidecar reports {collisions} tool name collision(s); refusing to start (ADR 0004)"
        )));
    }

    Ok(LaunchedSidecar {
        handle: handle.clone(),
        protocol_version,
        agent_version,
        tool_count,
    })
}

fn required_str(value: &Value, key: &str) -> Result<String, SidecarError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| SidecarError::Frame(format!("initialize result is missing \"{key}\"")))
}

/// Result of a successful spawn + handshake.
#[derive(Debug)]
pub struct LaunchedSidecar {
    pub handle: SidecarHandle,
    pub protocol_version: String,
    pub agent_version: String,
    pub tool_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reexported_clock_is_the_shared_one_not_a_second_copy() {
        // 这里原来是一份和 `yukinal-time` 里逐字相同的向量测试。测试向量只该有一处
        // （在那个 crate 里），本文件需要守的是另一件事：`sidecar::iso8601_utc`
        // 确实是**再导出**，而不是又一次悄悄长出来的实现。函数项可以互相比较，
        // 所以这一点是可断言的，不必靠读代码。
        //
        // 一次行为抽查留在下面：万一哪天有人改了签名但保留了名字，指向同一个
        // 函数这件事仍然成立，而输出变了才是真正要拦住的。
        assert!(std::ptr::fn_addr_eq(
            iso8601_utc as fn(u64) -> String,
            yukinal_time::iso8601_utc as fn(u64) -> String,
        ));
        assert!(std::ptr::fn_addr_eq(
            iso8601_now as fn() -> String,
            yukinal_time::iso8601_now as fn() -> String,
        ));
        assert_eq!(iso8601_utc(1_700_000_000), "2023-11-14T22:13:20Z");
    }
}
