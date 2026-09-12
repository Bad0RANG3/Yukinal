//! Supervisor: owns *one* sidecar process and the facts the desktop must report.
//!
//! Why it lives in `yukinal-core` and not in the Tauri crate: process supervision is a
//! capability of the native core, not of a window. Keeping it free of Tauri
//! types means the whole start/stop/crash/status path is testable without a GUI, and
//! the command layer stays a thin marshaller.
//!
//! 有界重启这一块（[`RestartPolicy`] / [`restart_delay`] / [`ExitRecord`] /
//! [`RestartRecord`] 与纯决策 `RestartState`）抽在 `restart` 子模块里并从本模块转出，
//! 所以 `crate::supervisor::RestartPolicy` 这些既有路径不变；本文件只留
//! [`Supervisor`] / `Inner` / `Watcher`。

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::{broadcast, Mutex as AsyncMutex};

use crate::sidecar::{self, SidecarConfig, SidecarError, SidecarEvent, SidecarHandle};

mod restart;

pub use restart::{restart_delay, ExitRecord, RestartPolicy, RestartRecord};

use restart::{RestartDecision, RestartState};

/// Bounded tail of sidecar stderr, newest last. A crash must be explainable from the
/// desktop's own memory, not by re-running with a debugger.
pub const LOG_HISTORY: usize = 200;

const UI_CHANNEL_CAPACITY: usize = 512;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub protocol_version: Option<String>,
    pub agent_version: Option<String>,
    /// Registered tools, captured at handshake. The live list is available through `tools.list`.
    pub tool_count: Option<usize>,
    pub entry: Option<String>,
    pub started_at: Option<String>,
    /// Survives until the next successful start, so a crash stays visible.
    pub last_exit: Option<ExitRecord>,
    /// Present only while a restart is pending, or after the budget was spent.
    ///
    /// `skip_serializing_if` keeps the field out of the payload when nothing has gone
    /// wrong: the fixture gate parses these responses strictly, and a permanent
    /// `"restart": null` would be a contract change for a state that is not news.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restart: Option<RestartRecord>,
}

#[derive(Debug, Clone)]
pub struct RuntimeInfo {
    pub pid: u32,
    pub protocol_version: String,
    pub agent_version: String,
    pub entry: String,
    pub tool_count: usize,
    pub started_at: String,
}

#[derive(Debug, Clone)]
pub struct StartOutcome {
    pub runtime: RuntimeInfo,
    /// False when this call is the one that actually launched the process.
    pub already_running: bool,
}

#[derive(Debug, Clone)]
struct RuntimeState {
    handle: SidecarHandle,
    info: RuntimeInfo,
}

#[derive(Debug)]
struct Inner {
    /// Serializes the check → spawn → handshake → publish sequence. Without this
    /// gate two simultaneous UI starts can both observe an empty slot and launch
    /// two sidecars before either one records its runtime.
    start_lock: AsyncMutex<()>,
    runtime: AsyncMutex<Option<RuntimeState>>,
    last_exit: AsyncMutex<Option<ExitRecord>>,
    logs: AsyncMutex<VecDeque<String>>,
    events: broadcast::Sender<SidecarEvent>,
    /// Read only by the restart path, which is why it lives in the shared state rather
    /// than being passed to the watcher: the watcher is created per process, the policy
    /// belongs to the supervisor.
    restart_policy: RestartPolicy,
    restart: AsyncMutex<RestartState>,
    /// The config the running (or most recently started) sidecar was launched with.
    ///
    /// The restart path needs exactly the config that worked. Re-running the resolution
    /// rules in the watcher would put a second copy of them in a task that runs *after*
    /// the failed start, which is precisely when a second, subtly different answer would
    /// go unnoticed.
    config: AsyncMutex<Option<SidecarConfig>>,
}

/// Cheap to clone; every clone addresses the same supervision state.
#[derive(Clone, Debug)]
pub struct Supervisor {
    inner: Arc<Inner>,
}

impl Default for Supervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl Supervisor {
    #[must_use]
    pub fn new() -> Self {
        Self::with_restart_policy(RestartPolicy::default())
    }

    /// Build a supervisor with an explicit restart policy. Tests use a fast one; the
    /// desktop uses the default.
    #[must_use]
    pub fn with_restart_policy(restart_policy: RestartPolicy) -> Self {
        let (events, _) = broadcast::channel(UI_CHANNEL_CAPACITY);
        Self {
            inner: Arc::new(Inner {
                start_lock: AsyncMutex::new(()),
                runtime: AsyncMutex::new(None),
                last_exit: AsyncMutex::new(None),
                logs: AsyncMutex::new(VecDeque::new()),
                events,
                restart_policy,
                restart: AsyncMutex::new(RestartState::default()),
                config: AsyncMutex::new(None),
            }),
        }
    }

    /// Sidecar notifications + logs for the UI to render (`agent.*`).
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<SidecarEvent> {
        self.inner.events.subscribe()
    }

    pub async fn status(&self) -> SupervisorStatus {
        let runtime = self.inner.runtime.lock().await.clone();
        let last_exit = self.inner.last_exit.lock().await.clone();
        let restart = self.inner.restart.lock().await.record.clone();
        match runtime.filter(|state| state.handle.is_running()) {
            Some(state) => SupervisorStatus {
                running: true,
                pid: Some(state.info.pid),
                protocol_version: Some(state.info.protocol_version),
                agent_version: Some(state.info.agent_version),
                tool_count: Some(state.info.tool_count),
                entry: Some(state.info.entry),
                started_at: Some(state.info.started_at),
                last_exit,
                restart,
            },
            None => SupervisorStatus {
                running: false,
                pid: None,
                protocol_version: None,
                agent_version: None,
                tool_count: None,
                entry: None,
                started_at: None,
                last_exit,
                restart,
            },
        }
    }

    pub async fn handle(&self) -> Option<SidecarHandle> {
        let runtime = self.inner.runtime.lock().await;
        runtime.as_ref().map(|state| state.handle.clone())
    }

    #[must_use]
    pub async fn logs(&self) -> Vec<String> {
        self.inner.logs.lock().await.iter().cloned().collect()
    }

    /// Launch (or reuse) the sidecar and handshake with it. Never leaves a half-alive
    /// child behind: `sidecar::launch` kills on handshake failure.
    ///
    /// This is the *asked-for* start, so it also clears the two records that describe an
    /// outage: a fresh start by the user is not the continuation of a crash loop.
    pub async fn start(&self, config: &SidecarConfig) -> Result<StartOutcome, SidecarError> {
        self.start_with_history(config, false).await
    }

    /// Start the agent again after an exit nobody asked for.
    ///
    /// Identical to [`Supervisor::start`] except that the crash and the restart stay
    /// visible: an automatic restart that erases its own cause is indistinguishable from
    /// "nothing happened", which is exactly the report a user must not get.
    async fn restart(&self, config: &SidecarConfig) -> Result<StartOutcome, SidecarError> {
        self.start_with_history(config, true).await
    }

    async fn start_with_history(
        &self,
        config: &SidecarConfig,
        keep_history: bool,
    ) -> Result<StartOutcome, SidecarError> {
        let _start_guard = self.inner.start_lock.lock().await;
        if let Some(state) = self.inner.runtime.lock().await.as_ref() {
            if state.handle.is_running() {
                return Ok(StartOutcome {
                    runtime: state.info.clone(),
                    already_running: true,
                });
            }
        }

        // Spawn first, subscribe second, handshake third: `initialize` must be the first
        // frame the agent sees (ADR 0006), but the agent's startup log lands before the
        // handshake returns -- subscribing last would drop those lines forever.
        let handle = sidecar::spawn(config).await?;
        let mut receiver = handle.subscribe();
        let launched = match sidecar::handshake(&handle, config).await {
            Ok(launched) => launched,
            Err(error) => {
                // 握手失败时，`receiver` 里已经躺着这个进程说过的最后几句话（`ready`、
                // 掉帧警告、Node 自己的解析错误），但把日志写进保留尾巴的那个循环还没
                // 起来 —— 它要等握手成功后的 `RuntimeState`。于是「为什么起不来」的证据
                // 恰好在这一条路径上被丢掉，界面与日志里只剩一句「agent sidecar exited」。
                // 先把已经到达的行落进尾巴，再关进程。
                // 子进程死了不等于它的话已经到手：读 stderr 的任务还在排空操作系统里的那段
                // 管道，所以抽一次是不够的。给两轮、每轮之间让出一点时间。
                for _ in 0..2 {
                    while let Ok(event) = receiver.try_recv() {
                        if let SidecarEvent::Log(line) = event {
                            self.inner.remember_log(&line).await;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                handle.shutdown().await;
                return Err(error);
            }
        };

        // `SidecarInfo` already carries `pid`, `entry` and `started_at`, populated by
        // `sidecar::spawn` from this same config (`sidecar/mod.rs:292-296`).
        //
        // This is a simplification, not a bug fix, and the distinction is worth being
        // precise about: it previously read `config.entry_label.clone()` and called
        // `info()` twice. Those cannot *currently* disagree, because spawn and this
        // function both read the same `&config` and nothing mutates `SidecarInfo.entry`
        // afterwards. What it removes is the redundant second source — the value that
        // `status()` reports is now unambiguously the handle's, and there is one
        // `SidecarInfo` clone instead of three.
        let handle_info = launched.handle.info();
        let info = RuntimeInfo {
            pid: handle_info.pid,
            protocol_version: launched.protocol_version,
            agent_version: launched.agent_version,
            entry: handle_info.entry,
            tool_count: launched.tool_count,
            started_at: handle_info.started_at,
        };

        let watcher = Watcher {
            inner: Arc::clone(&self.inner),
            pid: info.pid,
        };

        // A stale lastExit from a previous crash must not be attributed to this run — but a
        // crash we are restarting *from* is exactly the context of this run, so it stays.
        if !keep_history {
            *self.inner.last_exit.lock().await = None;
        }
        *self.inner.runtime.lock().await = Some(RuntimeState {
            handle: launched.handle,
            info: info.clone(),
        });

        // The restart path reuses this exact config, and the "was this process healthy?"
        // clock starts here rather than at spawn: the uptime that matters is uptime of a
        // *handshaked* agent, not of a process that may still die during `initialize`.
        *self.inner.config.lock().await = Some(config.clone());
        {
            let mut state = self.inner.restart.lock().await;
            state.started_at = Some(Instant::now());
            if !keep_history {
                state.record = None;
            }
        }
        // Mark the generation boundary in the tail. Without it the previous process's last
        // words and the new one's first words run together, and "which line came from the
        // process that died?" is unanswerable exactly when it matters.
        self.inner
            .remember_log(&format!("[supervisor] sidecar started (pid {})", info.pid))
            .await;

        tokio::spawn(async move {
            loop {
                match receiver.recv().await {
                    Ok(event) => match event {
                        SidecarEvent::Log(line) => watcher.remember_log(&line).await,
                        SidecarEvent::Exited { code, signal } => {
                            Arc::clone(&watcher.inner)
                                .handle_exit(watcher.pid, code, signal)
                                .await;
                            break;
                        }
                        frame @ SidecarEvent::Frame(_) => watcher.publish(frame).await,
                        request @ SidecarEvent::Request { .. } => watcher.publish(request).await,
                    },
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        watcher
                            .remember_log(&format!("dropped {missed} sidecar event(s)"))
                            .await;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        Ok(StartOutcome {
            runtime: info,
            already_running: false,
        })
    }

    /// Stop the sidecar. Returns whether a process was actually running.
    ///
    /// This is the *asked-for* exit: it takes the runtime slot before shutting the child
    /// down, which is what tells the watcher that the exit it is about to observe was not a
    /// crash. The restart policy is reset with it — a stop followed by a start is a fresh
    /// session, not attempt N of an outage.
    pub async fn stop(&self) -> bool {
        // Coordinate with start so a stop racing the spawn/handshake/publish sequence
        // cannot observe an empty runtime slot and leave the newly launched child alive.
        let _start_guard = self.inner.start_lock.lock().await;
        let runtime = self.inner.runtime.lock().await.take();
        self.inner.restart.lock().await.reset();
        match runtime {
            Some(state) => {
                state.handle.shutdown().await;
                true
            }
            None => false,
        }
    }

    /// Send a request to the running sidecar, or report that it is not running.
    pub async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, SidecarError> {
        let handle = self.handle().await.ok_or(SidecarError::NotRunning)?;
        handle.request(method, params, timeout).await
    }
}

/// Task-side view of the supervisor. It holds `Arc<Inner>` rather than a `Supervisor`
/// clone: the watcher must not keep the runtime slot alive by itself.
struct Watcher {
    inner: Arc<Inner>,
    pid: u32,
}

impl Watcher {
    async fn remember_log(&self, line: &str) {
        self.inner.remember_log(line).await;
    }

    async fn publish(&self, event: SidecarEvent) {
        self.inner.publish(event).await;
    }
}

impl Inner {
    /// Bounded stderr tail: evict the oldest line so a chatty agent cannot grow it forever.
    async fn remember_log(&self, line: &str) {
        let mut logs = self.logs.lock().await;
        if logs.len() >= LOG_HISTORY {
            logs.pop_front();
        }
        logs.push_back(line.to_string());
    }

    async fn publish(&self, event: SidecarEvent) {
        // No UI subscriber yet is normal; never an error path.
        let _ = self.events.send(event);
    }

    /// Fold one observed exit into: the record the UI reads, the event the forwarder logs,
    /// and — only for an exit nobody asked for — the restart decision.
    ///
    /// "Nobody asked for it" is decided by pid, not by a separate flag: `stop()` takes the
    /// runtime slot *before* shutting the child down, so an exit whose pid is no longer the
    /// recorded one was either requested by the user or already superseded by a newer
    /// process. Concluding that from a flag would mean trusting every present and future
    /// caller to set it.
    async fn handle_exit(self: &Arc<Self>, pid: u32, code: Option<i32>, signal: Option<String>) {
        *self.last_exit.lock().await = Some(ExitRecord {
            code,
            signal: signal.clone(),
            at: sidecar::iso8601_now(),
        });
        // Published rather than swallowed: the forwarder is created once and has to survive
        // a restart, and "the agent died" is the one line a reader of the desktop console
        // needs in order to make sense of the lines that follow it.
        self.publish(SidecarEvent::Exited {
            code,
            signal: signal.clone(),
        })
        .await;

        let was_current = {
            let mut runtime = self.runtime.lock().await;
            if runtime
                .as_ref()
                .is_some_and(|state| state.handle.info().pid == pid)
            {
                *runtime = None;
                true
            } else {
                false
            }
        };
        if !was_current {
            return;
        }

        let exit_note = match (code, signal.as_deref()) {
            (Some(code), _) => format!("exit code {code}"),
            (None, Some(signal)) => format!("signal {signal}"),
            (None, None) => "no exit status".to_string(),
        };
        let decision = {
            let mut state = self.restart.lock().await;
            state.decide(&self.restart_policy, Instant::now(), sidecar::iso8601_now())
        };
        match decision {
            RestartDecision::Exhausted { attempts } => {
                if self.restart_policy.enabled {
                    self.remember_log(&format!(
                        "[supervisor] sidecar exited ({exit_note}); restart budget of {attempts} attempt(s) is spent, not restarting"
                    ))
                    .await;
                }
            }
            RestartDecision::Retry { attempt, delay } => {
                self.remember_log(&format!(
                    "[supervisor] sidecar exited ({exit_note}); restart {attempt}/{} in {} ms",
                    self.restart_policy.max_attempts,
                    delay.as_millis()
                ))
                .await;
                let config = self.config.lock().await.clone();
                spawn_restart(Arc::clone(self), config, delay, attempt);
            }
        }
    }
}

/// Start the restart attempt in a fresh task.
///
/// The future is boxed on purpose. `start` spawns the watcher, the watcher handles an exit,
/// the exit path restarts through `start` again — a genuinely cyclic call graph, which the
/// compiler refuses to give an opaque type to ("cycle detected when computing type of opaque
/// `start`"). Erasing the future here is what breaks that cycle; it is not a style choice.
fn spawn_restart(inner: Arc<Inner>, config: Option<SidecarConfig>, delay: Duration, attempt: u32) {
    let future: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
        Box::pin(async move { restart_loop(inner, config, delay, attempt).await });
    tokio::spawn(future);
}

/// Wait, then try to bring the agent back, and keep trying within the same budget.
///
/// A restart that itself fails (`node` removed while the app was open, a bundle that no
/// longer parses) produces no `Exited` event, so nothing else would notice it: without this
/// loop the supervisor would simply stop trying after the first failure and the UI would
/// show a dead agent with attempts left unspent.
async fn restart_loop(
    inner: Arc<Inner>,
    config: Option<SidecarConfig>,
    mut delay: Duration,
    attempt: u32,
) {
    let Some(config) = config else {
        inner
            .remember_log("[supervisor] no remembered sidecar config; not restarting")
            .await;
        return;
    };

    let mut attempt = attempt;
    loop {
        tokio::time::sleep(delay).await;
        let supervisor = Supervisor {
            inner: Arc::clone(&inner),
        };
        match supervisor.restart(&config).await {
            Ok(outcome) => {
                inner
                    .remember_log(&format!(
                        "[supervisor] restart {attempt} succeeded (pid {}, reusing a running process: {})",
                        outcome.runtime.pid, outcome.already_running
                    ))
                    .await;
                return;
            }
            Err(error) => {
                inner
                    .remember_log(&format!("[supervisor] restart {attempt} failed: {error}"))
                    .await;
                let decision = {
                    let mut state = inner.restart.lock().await;
                    state.decide(
                        &inner.restart_policy,
                        Instant::now(),
                        sidecar::iso8601_now(),
                    )
                };
                match decision {
                    RestartDecision::Retry {
                        attempt: next,
                        delay: next_delay,
                    } => {
                        attempt = next;
                        delay = next_delay;
                    }
                    RestartDecision::Exhausted { attempts } => {
                        inner
                            .remember_log(&format!(
                                "[supervisor] restart budget of {attempts} attempt(s) is spent, giving up"
                            ))
                            .await;
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_status_without_trouble_omits_the_restart_field_entirely() {
        let status = Supervisor::new().status().await;
        let payload = serde_json::to_value(&status).expect("serialize");
        assert!(
            payload.get("restart").is_none(),
            "a permanent \"restart\": null would be a contract change for a non-event"
        );
        assert_eq!(
            payload.get("running").and_then(serde_json::Value::as_bool),
            Some(false)
        );
    }
}
