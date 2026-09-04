//! Supervisor: owns *one* sidecar process and the facts the desktop must report.
//!
//! Why it lives in `yukinal-core` and not in the Tauri crate: process supervision is a
//! capability of the native core, not of a window. Keeping it free of Tauri
//! types means the whole start/stop/crash/status path is testable without a GUI, and
//! the command layer stays a thin marshaller.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::{broadcast, Mutex as AsyncMutex};

use crate::sidecar::{self, SidecarConfig, SidecarError, SidecarEvent, SidecarHandle};

/// Bounded tail of sidecar stderr, newest last. A crash must be explainable from the
/// desktop's own memory, not by re-running with a debugger.
pub const LOG_HISTORY: usize = 200;

const UI_CHANNEL_CAPACITY: usize = 512;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExitRecord {
    pub code: Option<i32>,
    pub signal: Option<String>,
    pub at: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub protocol_version: Option<String>,
    pub agent_version: Option<String>,
    /// Registered tools, captured at handshake. The live list is `tools.list` （not built yet）.
    pub tool_count: Option<usize>,
    pub entry: Option<String>,
    pub started_at: Option<String>,
    /// Survives until the next successful start, so a crash stays visible.
    pub last_exit: Option<ExitRecord>,
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
    runtime: AsyncMutex<Option<RuntimeState>>,
    last_exit: AsyncMutex<Option<ExitRecord>>,
    logs: AsyncMutex<VecDeque<String>>,
    events: broadcast::Sender<SidecarEvent>,
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
        let (events, _) = broadcast::channel(UI_CHANNEL_CAPACITY);
        Self {
            inner: Arc::new(Inner {
                runtime: AsyncMutex::new(None),
                last_exit: AsyncMutex::new(None),
                logs: AsyncMutex::new(VecDeque::new()),
                events,
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
    pub async fn start(&self, config: &SidecarConfig) -> Result<StartOutcome, SidecarError> {
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
                handle.shutdown().await;
                return Err(error);
            }
        };

        let info = RuntimeInfo {
            pid: launched.handle.info().pid,
            protocol_version: launched.protocol_version,
            agent_version: launched.agent_version,
            entry: config.entry_label.clone(),
            tool_count: launched.tool_count,
            started_at: launched.handle.info().started_at,
        };

        let watcher = Watcher {
            inner: Arc::clone(&self.inner),
            pid: info.pid,
        };

        // A stale lastExit from a previous crash must not be attributed to this run.
        *self.inner.last_exit.lock().await = None;
        *self.inner.runtime.lock().await = Some(RuntimeState {
            handle: launched.handle,
            info: info.clone(),
        });

        tokio::spawn(async move {
            loop {
                match receiver.recv().await {
                    Ok(event) => match event {
                        SidecarEvent::Log(line) => watcher.remember_log(&line).await,
                        SidecarEvent::Exited { code, signal } => {
                            watcher.record_exit(code, signal).await;
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
    pub async fn stop(&self) -> bool {
        let runtime = self.inner.runtime.lock().await.take();
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
        let mut logs = self.inner.logs.lock().await;
        if logs.len() >= LOG_HISTORY {
            logs.pop_front();
        }
        logs.push_back(line.to_string());
    }

    async fn record_exit(&self, code: Option<i32>, signal: Option<String>) {
        let record = ExitRecord {
            code,
            signal,
            at: sidecar::iso8601_now(),
        };
        *self.inner.last_exit.lock().await = Some(record);
        let mut runtime = self.inner.runtime.lock().await;
        if runtime
            .as_ref()
            .is_some_and(|state| state.handle.info().pid == self.pid)
        {
            *runtime = None;
        }
    }

    async fn publish(&self, event: SidecarEvent) {
        // No UI subscriber yet is normal; never an error path.
        let _ = self.inner.events.send(event);
    }
}
