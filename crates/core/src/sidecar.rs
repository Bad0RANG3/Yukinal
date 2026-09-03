//! Sidecar supervision: Rust owns the agent process (ADR 0001/0006/0009).
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
    /// Resolution order (ADR 0009):
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
            Ok(Ok(Err(message))) => Err(SidecarError::Remote(message)),
            Ok(Ok(Ok(result))) => Ok(result),
        }
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
                started_at: iso8601_utc(now_epoch_seconds()),
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
        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(line) {
                Ok(frame) => reader.dispatch(frame),
                Err(error) => reader.broadcast(SidecarEvent::Log(format!(
                    "dropped non-JSON stdout line ({error}): {}",
                    truncate(line)
                ))),
            }
        }
    });

    // stderr is the sidecar's log channel; surface it, never swallow it.
    let logger = handle.clone();
    let mut err_lines = BufReader::new(stderr).lines();
    tokio::spawn(async move {
        while let Ok(Some(line)) = err_lines.next_line().await {
            logger.broadcast(SidecarEvent::Log(line));
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
        if frame.get("method").is_some() {
            // A request from the sidecar to us is not part of ADR 0006; treat it as
            // data so it stays visible instead of being dropped.
            self.broadcast(SidecarEvent::Frame(frame));
            return;
        }
        let outcome = match frame.get("error") {
            Some(error) => Err(error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("agent error")
                .to_string()),
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
#[must_use]
pub fn iso8601_now() -> String {
    iso8601_utc(now_epoch_seconds())
}

fn now_epoch_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|delta| delta.as_secs())
        .unwrap_or(0)
}

/// ISO-8601 UTC without adding a date crate for one display string.
/// Gregorian civil-date conversion after Howard Hinnant's `civil_from_days`.
/// Exposed for status rows; the sidecar's own clock stays authoritative for audit
///, this is only for "when did we start this process".
#[must_use]
pub fn iso8601_utc(epoch: u64) -> String {
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
    let day_of_era = (shifted - era * 146_097) as u64; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365; // [0, 399]
                                                                                              // One era is 400 years (146_097 days), not 146_097 years.
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100); // [0, 365]
    let month_prime = (5 * day_of_year + 2) / 153; // [0, 11]
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32; // [1, 31]
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

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
    fn iso8601_matches_reference_timestamps() {
        // Vectors generated with `new Date(epoch * 1000).toISOString()`.
        let cases = [
            (0_u64, "1970-01-01T00:00:00Z"),
            (59, "1970-01-01T00:00:59Z"),
            (3_661, "1970-01-01T01:01:01Z"),
            (1_582_934_400, "2020-02-29T00:00:00Z"), // leap day
            (1_700_000_000, "2023-11-14T22:13:20Z"),
            (1_893_456_000, "2030-01-01T00:00:00Z"),
            (2_147_483_647, "2038-01-19T03:14:07Z"), // y2k38 boundary
        ];
        for (epoch, expected) in cases {
            assert_eq!(iso8601_utc(epoch), expected, "epoch {epoch}");
        }
    }

    #[test]
    fn truncate_keeps_long_frames_short() {
        let long = "x".repeat(400);
        assert_eq!(truncate(&long).chars().count(), 201);
        assert_eq!(truncate("short"), "short");
    }
}
