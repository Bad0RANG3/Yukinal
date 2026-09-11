//! Sidecar supervision: Rust owns the agent process (ADR 0001/0006/0008).
//!
//! The desktop never spawns processes itself. This module resolves *what* to launch,
//! launches it with piped stdio, speaks NDJSON JSON-RPC, correlates responses, forwards
//! notifications/logs, and guarantees no orphan is left behind (`kill_on_drop` +
//! explicit [`SidecarHandle::shutdown`]).

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

/// Must match `YUKINAL_RPC_VERSION` in `@yukinal/shared` (ADR 0006). A mismatch is
/// refused instead of treated as "probably compatible": half-speaking protocols are how
/// a permission decision ends up evaluated against the wrong payload shape.
pub const PROTOCOL_VERSION: &str = "1.0";

/// JSON-RPC request ids are local to this supervisor; the agent never allocates ids.
const EVENT_CHANNEL_CAPACITY: usize = 256;
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(200);
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

/// What to launch and how. Kept as data (not a magic env read inside the spawn path)
/// so tests can point it at anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarConfig {
    /// Executable, normally `node`.
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(String, String)>,
    pub request_timeout: Duration,
    /// Path reported back to the UI, so "what actually started" is visible.
    pub entry_label: String,
    /// Desktop version sent during `initialize` (audit + capability negotiation).
    pub client_version: String,
    /// Handed to the sidecar so it can find its local spool. Never a secret and never
    /// a credential.
    pub data_dir: String,
}

impl SidecarConfig {
    /// Resolution order (ADR 0008):
    /// 1. `YUKINAL_AGENT_COMMAND` (+ optional `YUKINAL_AGENT_ARGS`, `;`-separated)
    /// 2. `YUKINAL_AGENT_ENTRY` (+ optional `YUKINAL_NODE`)
    /// 3. dev fallback: nearest `apps/agent/dist/index.js` walking up from `cwd`
    ///
    /// Never a silent default: if nothing resolves, the caller gets a message naming
    /// the build step to run.
    pub fn from_env_with_cwd(cwd: &Path) -> Result<Self, SidecarError> {
        let lookup = |key: &str| {
            std::env::var(key)
                .ok()
                .filter(|value| !value.trim().is_empty())
        };

        let request_timeout = Duration::from_secs(
            lookup("YUKINAL_AGENT_TIMEOUT_SECS")
                .and_then(|raw| raw.parse::<u64>().ok())
                .unwrap_or(10),
        );

        if let Some(command) = lookup("YUKINAL_AGENT_COMMAND") {
            let args = match lookup("YUKINAL_AGENT_ARGS") {
                Some(raw) => raw.split(';').map(OsString::from).collect(),
                None => Vec::new(),
            };
            return Ok(Self {
                program: PathBuf::from(command),
                args,
                env: Vec::new(),
                request_timeout,
                entry_label: String::from("custom command"),
                client_version: default_client_version(),
                data_dir: lookup("YUKINAL_DATA_DIR").unwrap_or_default(),
            });
        }

        if let Some(entry) = lookup("YUKINAL_AGENT_ENTRY") {
            let path = PathBuf::from(&entry);
            if !path.is_file() {
                return Err(SidecarError::NotFound {
                    searched: path.display().to_string(),
                });
            }
            return Ok(Self {
                program: node_program(lookup("YUKINAL_NODE").as_deref()),
                args: vec![path.into_os_string()],
                env: Vec::new(),
                request_timeout,
                entry_label: entry,
                client_version: default_client_version(),
                data_dir: lookup("YUKINAL_DATA_DIR").unwrap_or_default(),
            });
        }

        match find_dev_bundle(cwd) {
            Some(path) => Ok(Self {
                program: node_program(None),
                args: vec![path.clone().into_os_string()],
                env: Vec::new(),
                request_timeout,
                entry_label: path.display().to_string(),
                client_version: default_client_version(),
                data_dir: lookup("YUKINAL_DATA_DIR").unwrap_or_default(),
            }),
            None => Err(SidecarError::NotFound {
                searched: ancestors(cwd)
                    .map(|dir| format!("{}/apps/agent/dist/index.js", dir.display()))
                    .collect::<Vec<_>>()
                    .join(", "),
            }),
        }
    }

    #[must_use]
    pub fn with_env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_string(), value.to_string()));
        self
    }
}

fn default_client_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn node_program(override_path: Option<&str>) -> PathBuf {
    match override_path {
        Some(explicit) if !explicit.trim().is_empty() => PathBuf::from(explicit),
        _ => PathBuf::from(if cfg!(windows) { "node.exe" } else { "node" }),
    }
}

fn find_dev_bundle(cwd: &Path) -> Option<PathBuf> {
    ancestors(cwd)
        .map(|dir| dir.join("apps").join("agent").join("dist").join("index.js"))
        .find(|candidate| candidate.is_file())
}

fn ancestors(start: &Path) -> impl Iterator<Item = PathBuf> {
    let mut current = Some(start.to_path_buf());
    std::iter::from_fn(move || {
        let path = current.take()?;
        let parent = path.parent().map(Path::to_path_buf);
        current = parent;
        Some(if path.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            path
        })
    })
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
        .map_err(|error| SidecarError::Launch(format!("{}: {error}", config.program.display())))?;
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

fn truncate(line: &str) -> String {
    const MAX: usize = 200;
    let mut out: String = line.chars().take(MAX).collect();
    if line.chars().count() > MAX {
        out.push('…');
    }
    out
}

const REDACTED: &str = "[redacted]";

/// Redact credentials before they can leave the process boundary via a diagnostic
/// error. This intentionally favors false positives: sidecar logs are diagnostic
/// only, while a leaked key cannot be recovered.
fn redact_log_line(line: &str) -> String {
    let mut redacted = line.to_string();
    for marker in [
        "authorization",
        "api_key",
        "api-key",
        "apikey",
        "access_token",
        "access-token",
        "password",
        "token",
    ] {
        redacted = redact_named_value(&redacted, marker);
    }
    for prefix in [
        "bearer ",
        "basic ",
        "sk-",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "github_pat_",
        "akia",
    ] {
        redacted = redact_token_after_prefix(&redacted, prefix);
    }
    redacted
}

/// Keep private-key blocks out of process logs even if a future sidecar writes
/// one line at a time. The delimiters themselves are not useful diagnostics here.
fn redact_process_log_line(line: &str, inside_private_key: &mut bool) -> String {
    let uppercase = line.to_ascii_uppercase();
    let begins_private_key =
        uppercase.contains("-----BEGIN") && uppercase.contains("PRIVATE KEY-----");
    let ends_private_key = uppercase.contains("-----END") && uppercase.contains("PRIVATE KEY-----");
    if *inside_private_key || begins_private_key {
        *inside_private_key = !ends_private_key;
        return String::from("[redacted private-key material]");
    }
    redact_log_line(line)
}

fn redact_named_value(line: &str, marker: &str) -> String {
    let mut output = line.to_string();
    let mut search_from = 0;
    loop {
        let lowercase = output.to_ascii_lowercase();
        let Some(relative) = lowercase[search_from..].find(marker) else {
            return output;
        };
        let marker_start = search_from + relative;
        let mut cursor = marker_start + marker.len();
        let mut found_separator = false;
        while let Some(character) = output[cursor..].chars().next() {
            match character {
                ':' | '=' => {
                    cursor += character.len_utf8();
                    found_separator = true;
                    break;
                }
                ' ' | '\t' | '"' | '\'' => cursor += character.len_utf8(),
                _ => break,
            }
        }
        if !found_separator {
            search_from = cursor.max(marker_start + marker.len());
            continue;
        }
        while let Some(character) = output[cursor..].chars().next() {
            if character.is_whitespace() {
                cursor += character.len_utf8();
            } else {
                break;
            }
        }
        let quote = output[cursor..]
            .chars()
            .next()
            .filter(|character| matches!(character, '"' | '\''));
        if let Some(quote) = quote {
            cursor += quote.len_utf8();
        }
        let value_start = cursor;
        let value_end = output[value_start..]
            .char_indices()
            .find_map(|(offset, character)| {
                (if let Some(quote) = quote {
                    character == quote
                } else {
                    character.is_whitespace() || matches!(character, '&' | ',' | ';' | '}' | ']')
                })
                .then_some(value_start + offset)
            })
            .unwrap_or(output.len());
        if value_start == value_end {
            return output;
        }
        output.replace_range(value_start..value_end, REDACTED);
        search_from = value_start + REDACTED.len();
    }
}

fn redact_token_after_prefix(line: &str, prefix: &str) -> String {
    let mut output = line.to_string();
    let mut search_from = 0;
    loop {
        let lowercase = output.to_ascii_lowercase();
        let Some(relative) = lowercase[search_from..].find(prefix) else {
            return output;
        };
        let start = search_from + relative;
        let value_start = start + prefix.len();
        let value_end = output[value_start..]
            .char_indices()
            .find_map(|(offset, character)| {
                (!character.is_ascii_alphanumeric() && !matches!(character, '-' | '_' | '.'))
                    .then_some(value_start + offset)
            })
            .unwrap_or(output.len());
        if value_end.saturating_sub(start) >= 12 {
            output.replace_range(start..value_end, REDACTED);
            search_from = start + REDACTED.len();
        } else if value_end == output.len() {
            return output;
        } else {
            search_from = value_end.max(value_start + 1);
        }
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

    fn temp_tagged_dir(tag: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let unique = format!("yukinal-sidecar-test-{tag}-{}", std::process::id());
        path.push(unique);
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn finds_the_dev_bundle_in_an_ancestor() {
        let root = temp_tagged_dir("bundle");
        let bundle = root.join("apps").join("agent").join("dist");
        std::fs::create_dir_all(&bundle).expect("create bundle dir");
        std::fs::write(bundle.join("index.js"), "console.log('x')").expect("write bundle");

        let nested = root.join("apps").join("desktop").join("src-tauri");
        std::fs::create_dir_all(&nested).expect("create nested dir");

        let found = find_dev_bundle(&nested);
        assert_eq!(found, Some(bundle.join("index.js")));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn missing_bundle_error_names_the_fix() {
        let root = temp_tagged_dir("empty");
        let error =
            SidecarConfig::from_env_with_cwd(&root).expect_err("should fail when nothing resolves");
        let message = error.to_string();
        assert!(
            message.contains("pnpm --filter @yukinal/agent build"),
            "{message}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn explicit_command_override_wins_and_splits_args_on_semicolon() {
        // Guarded: these env vars are process-global, so this test owns them.
        std::env::set_var("YUKINAL_AGENT_COMMAND", "/usr/bin/true");
        std::env::set_var("YUKINAL_AGENT_ARGS", "one;two with space");
        let config = SidecarConfig::from_env_with_cwd(Path::new(".")).expect("explicit config");
        assert_eq!(config.program, PathBuf::from("/usr/bin/true"));
        assert_eq!(config.args.len(), 2);
        assert_eq!(config.args[1], OsString::from("two with space"));
        std::env::remove_var("YUKINAL_AGENT_COMMAND");
        std::env::remove_var("YUKINAL_AGENT_ARGS");
    }

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

    #[test]
    fn truncate_keeps_long_frames_short() {
        let long = "x".repeat(400);
        assert_eq!(truncate(&long).chars().count(), 201);
        assert_eq!(truncate("short"), "short");
    }

    #[test]
    fn sidecar_diagnostics_redact_known_credential_forms() {
        let key = format!("{}{}", "sk-proj-", "abcdefghijklmnopqrstuvwxyz");
        let line = format!("authorization: Bearer {key} api_key=another-secret");
        let redacted = redact_log_line(&line);
        assert!(!redacted.contains(&key));
        assert!(!redacted.contains("another-secret"));
        assert!(redacted.contains(REDACTED));
    }

    #[test]
    fn sidecar_diagnostics_redact_multiline_private_keys() {
        let mut inside_private_key = false;
        assert_eq!(
            redact_process_log_line(
                "-----BEGIN OPENSSH PRIVATE KEY-----",
                &mut inside_private_key
            ),
            "[redacted private-key material]"
        );
        assert!(inside_private_key);
        assert_eq!(
            redact_process_log_line("base64-private-key-payload", &mut inside_private_key),
            "[redacted private-key material]"
        );
        assert_eq!(
            redact_process_log_line("-----END OPENSSH PRIVATE KEY-----", &mut inside_private_key),
            "[redacted private-key material]"
        );
        assert!(!inside_private_key);
    }
}
