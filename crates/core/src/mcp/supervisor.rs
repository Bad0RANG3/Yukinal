//! Server-id keyed MCP process supervision.
//!
//! A crash is reported and then recovered with the same bounded backoff used by the
//! sidecar. Recovery only starts a fresh process and rebuilds its tool catalog; it never
//! replays the call that was in flight when the old process exited.

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use crate::supervisor::{RestartDecision, RestartPolicy, RestartState};

use super::config::{McpStdioConfig, McpTransportConfig};
use super::descriptor::{McpExitRecord, McpToolDescriptor, McpToolResult};
use super::error::McpError;
use super::handle::{McpServerStart, McpServerStatus, McpStdioHandle, ShutdownReport};
use super::http::McpHttpHandle;
use super::transport::McpServerHandle;
use super::truncated;

const GLOBAL_SHUTDOWN_DEADLINE: Duration = Duration::from_secs(7);

/// One managed server. The handle can be replaced after a crash while the exit and
/// restart records remain on this entry, so a recovered process cannot erase the reason
/// it had to recover.
#[derive(Debug)]
struct ManagedServer {
    handle: McpServerHandle,
    config: McpTransportConfig,
    generation: u64,
    manual_stop: bool,
    last_exit: Option<McpExitRecord>,
    restart: RestartState,
}

/// Process ownership for every configured MCP server.
#[derive(Debug, Clone, Default)]
pub struct McpSupervisor {
    inner: Arc<SupervisorInner>,
}

#[derive(Debug, Default)]
struct SupervisorInner {
    /// Serializes check -> spawn -> handshake -> publish for all server ids.
    ///
    /// This is independently owned by restart tasks while they launch a child. Keeping the
    /// lock separate means a restart can serialize against explicit starts without holding
    /// the whole supervisor alive across spawn or handshake waits.
    start_lock: Arc<AsyncMutex<()>>,
    servers: AsyncMutex<HashMap<String, ManagedServer>>,
    restart_policy: RestartPolicy,
}

impl McpSupervisor {
    #[must_use]
    pub fn new() -> Self {
        Self::with_restart_policy(RestartPolicy::default())
    }

    #[must_use]
    pub fn with_restart_policy(restart_policy: RestartPolicy) -> Self {
        Self {
            inner: Arc::new(SupervisorInner {
                start_lock: Arc::new(AsyncMutex::new(())),
                servers: AsyncMutex::new(HashMap::new()),
                restart_policy,
            }),
        }
    }

    /// Start a server explicitly, reuse it when already running, or replace a dead
    /// handle when the user has asked for a fresh start.
    pub async fn start(&self, config: &McpStdioConfig) -> Result<McpServerStart, McpError> {
        self.start_transport(&McpTransportConfig::Stdio(config.clone()))
            .await
    }

    pub async fn start_transport(
        &self,
        config: &McpTransportConfig,
    ) -> Result<McpServerStart, McpError> {
        let _start = self.inner.start_lock.lock().await;

        {
            let servers = self.inner.servers.lock().await;
            if let Some(managed) = servers.get(config.server_id()) {
                if managed.handle.is_running() {
                    return Ok(McpServerStart {
                        info: managed.handle.info(),
                        tool_count: managed.handle.tools().len(),
                        already_running: true,
                    });
                }
            }
        }

        let handle = launch_ready(config).await?;
        let start = McpServerStart {
            info: handle.info(),
            tool_count: handle.tools().len(),
            already_running: false,
        };

        let generation = {
            let mut servers = self.inner.servers.lock().await;
            let generation = servers
                .get(config.server_id())
                .map_or(1, |managed| managed.generation.wrapping_add(1));
            let mut restart = servers
                .remove(config.server_id())
                .map(|managed| managed.restart)
                .unwrap_or_default();
            restart.reset();
            restart.mark_started();
            servers.insert(
                config.server_id().to_string(),
                ManagedServer {
                    handle: handle.clone(),
                    config: config.clone(),
                    generation,
                    manual_stop: false,
                    last_exit: None,
                    restart,
                },
            );
            generation
        };
        watch_for_exit(
            Arc::downgrade(&self.inner),
            config.server_id().to_string(),
            generation,
            handle,
        );
        Ok(start)
    }

    pub async fn handle(&self, server_id: &str) -> Option<McpServerHandle> {
        self.inner
            .servers
            .lock()
            .await
            .get(server_id)
            .map(|managed| managed.handle.clone())
    }

    /// This supervisor's server ids, including entries whose current child is dead.
    pub async fn servers(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.inner.servers.lock().await.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Cached tool descriptors for one server.
    pub async fn tools(&self, server_id: &str) -> Vec<McpToolDescriptor> {
        match self.handle(server_id).await {
            Some(handle) => handle.tools(),
            None => Vec::new(),
        }
    }

    pub async fn call(
        &self,
        server_id: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<McpToolResult, McpError> {
        self.call_with_cancel(server_id, tool, arguments, &CancellationToken::new())
            .await
    }

    /// Call a tool with a cancellation token owned by the host request.
    pub async fn call_with_cancel(
        &self,
        server_id: &str,
        tool: &str,
        arguments: Value,
        cancel: &CancellationToken,
    ) -> Result<McpToolResult, McpError> {
        let handle = self
            .handle(server_id)
            .await
            .ok_or_else(|| McpError::NotRunning {
                server_id: truncated(server_id),
            })?;
        let timeout = handle.request_timeout();
        handle
            .call_tool_with_cancel(tool, arguments, timeout, cancel)
            .await
    }

    pub async fn status(&self, server_id: &str) -> McpServerStatus {
        let servers = self.inner.servers.lock().await;
        let Some(managed) = servers.get(server_id) else {
            return McpServerStatus::untracked(server_id);
        };
        let handle = &managed.handle;
        let info = handle.info();
        let running = handle.is_running();
        let handshake = info.handshake;
        McpServerStatus {
            server_id: info.server_id,
            running,
            pid: running.then_some(info.pid).flatten(),
            program: info.program,
            started_at: Some(info.started_at),
            protocol_version: handshake.as_ref().map(|it| it.protocol_version.clone()),
            server_name: handshake.as_ref().map(|it| it.server_name.clone()),
            server_version: handshake.as_ref().map(|it| it.server_version.clone()),
            tool_count: handle.tools().len(),
            last_exit: handle.last_exit().or_else(|| managed.last_exit.clone()),
            restart: managed.restart.record.clone(),
            stderr_tail: handle.stderr_tail(),
            diagnostics: handle.diagnostics(),
        }
    }

    /// Stop a server explicitly. The generation changes before shutdown so an exit
    /// watcher can never mistake this requested stop for a crash.
    pub async fn shutdown(&self, server_id: &str) -> Option<ShutdownReport> {
        let handle = {
            let mut servers = self.inner.servers.lock().await;
            let managed = servers.get_mut(server_id)?;
            managed.manual_stop = true;
            managed.generation = managed.generation.wrapping_add(1);
            managed.handle.clone()
        };
        Some(handle.shutdown().await)
    }

    pub async fn shutdown_all(&self) -> Vec<(String, ShutdownReport)> {
        let handles: Vec<(String, McpServerHandle)> = {
            let mut servers = self.inner.servers.lock().await;
            servers
                .iter_mut()
                .map(|(id, managed)| {
                    managed.manual_stop = true;
                    managed.generation = managed.generation.wrapping_add(1);
                    (id.clone(), managed.handle.clone())
                })
                .collect()
        };
        let mut remaining: HashMap<String, McpServerHandle> = handles.iter().cloned().collect();
        let mut reports = Vec::with_capacity(handles.len());
        let mut tasks = tokio::task::JoinSet::new();
        for (server_id, handle) in handles {
            tasks.spawn(async move {
                let report = handle.shutdown().await;
                (server_id, report)
            });
        }

        let collect = async {
            while let Some(joined) = tasks.join_next().await {
                if let Ok((server_id, report)) = joined {
                    remaining.remove(&server_id);
                    reports.push((server_id, report));
                }
            }
        };
        if tokio::time::timeout(GLOBAL_SHUTDOWN_DEADLINE, collect)
            .await
            .is_err()
        {
            tasks.abort_all();
            while let Some(joined) = tasks.join_next().await {
                if let Ok((server_id, report)) = joined {
                    remaining.remove(&server_id);
                    reports.push((server_id, report));
                }
            }
        }
        for (server_id, handle) in remaining {
            reports.push((server_id, handle.force_stop().await));
        }
        reports.sort_by(|left, right| left.0.cmp(&right.0));
        reports
    }
}

fn watch_for_exit(
    inner: Weak<SupervisorInner>,
    server_id: String,
    generation: u64,
    handle: McpServerHandle,
) {
    let Some(watch) = handle.exit_watch() else {
        return;
    };
    drop(handle);
    tokio::spawn(async move {
        let Some((_pid, exit)) = watch.wait().await else {
            return;
        };
        let restart = {
            let Some(inner) = inner.upgrade() else {
                return;
            };
            let mut servers = inner.servers.lock().await;
            let Some(managed) = servers.get_mut(&server_id) else {
                return;
            };
            if managed.manual_stop || managed.generation != generation {
                return;
            }
            managed.last_exit = Some(exit);
            managed.config.clone()
        };
        automatic_restart(inner, server_id, generation, restart).await;
    });
}

async fn automatic_restart(
    inner: Weak<SupervisorInner>,
    server_id: String,
    generation: u64,
    config: McpTransportConfig,
) {
    loop {
        let delay = {
            let Some(inner) = inner.upgrade() else {
                return;
            };
            let mut servers = inner.servers.lock().await;
            let Some(managed) = servers.get_mut(&server_id) else {
                return;
            };
            if managed.manual_stop || managed.generation != generation {
                return;
            }
            match managed.restart.decide(
                &inner.restart_policy,
                Instant::now(),
                crate::sidecar::iso8601_now(),
            ) {
                RestartDecision::Exhausted { .. } => return,
                RestartDecision::Retry { delay, .. } => delay,
            }
        };

        tokio::time::sleep(delay).await;

        let start_lock = {
            let Some(inner) = inner.upgrade() else {
                return;
            };
            Arc::clone(&inner.start_lock)
        };
        let _start = start_lock.lock().await;
        {
            let Some(inner) = inner.upgrade() else {
                return;
            };
            let servers = inner.servers.lock().await;
            match servers.get(&server_id) {
                Some(managed) if !managed.manual_stop && managed.generation == generation => {}
                _ => return,
            }
        }

        let handle = match launch_ready(&config).await {
            Ok(handle) => handle,
            Err(_) => continue,
        };

        let installed = {
            let Some(inner) = inner.upgrade() else {
                handle.shutdown().await;
                return;
            };
            let mut servers = inner.servers.lock().await;
            match servers.get_mut(&server_id) {
                Some(managed) if !managed.manual_stop && managed.generation == generation => {
                    managed.handle = handle.clone();
                    managed.restart.mark_started();
                    true
                }
                _ => false,
            }
        };

        if installed {
            watch_for_exit(inner, server_id, generation, handle);
            return;
        }

        handle.shutdown().await;
        return;
    }
}

async fn launch_ready(config: &McpTransportConfig) -> Result<McpServerHandle, McpError> {
    let handle = match config {
        McpTransportConfig::Stdio(config) => {
            McpServerHandle::Stdio(McpStdioHandle::spawn(config).await?)
        }
        McpTransportConfig::Http(config) => McpServerHandle::Http(McpHttpHandle::new(config)?),
    };
    if let Err(error) = handle.initialize(config.request_timeout()).await {
        handle.shutdown().await;
        return Err(error);
    }
    if let Err(error) = handle.list_tools(config.request_timeout()).await {
        handle.shutdown().await;
        return Err(error);
    }
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::config::McpHttpConfig;

    #[tokio::test]
    async fn a_supervisor_that_never_started_anything_still_answers() {
        let supervisor = McpSupervisor::new();
        assert!(supervisor.handle("mcp-1").await.is_none());
        assert!(supervisor.tools("mcp-1").await.is_empty());
        assert!(supervisor.servers().await.is_empty());
        assert!(supervisor.shutdown("mcp-1").await.is_none());

        let status = supervisor.status("mcp-1").await;
        assert_eq!(status.server_id, "mcp-1");
        assert!(!status.running);
        assert!(status.pid.is_none());
        assert!(status.last_exit.is_none());
        assert!(status.restart.is_none());

        let error = supervisor
            .call("mcp-1", "echo", serde_json::json!({}))
            .await
            .expect_err("nothing to call");
        assert!(matches!(error, McpError::NotRunning { .. }), "{error:?}");
    }

    #[tokio::test]
    async fn the_status_payload_omits_recovery_detail_while_nothing_went_wrong() {
        let status = McpSupervisor::new().status("mcp-1").await;
        let payload = serde_json::to_value(&status).expect("serialize");
        assert!(payload.get("lastExit").is_none());
        assert!(payload.get("restart").is_none());
        assert_eq!(payload["running"], serde_json::json!(false));
        assert_eq!(payload["toolCount"], serde_json::json!(0));
    }

    #[tokio::test]
    async fn restart_backoff_does_not_keep_the_supervisor_alive() {
        let http_config = McpHttpConfig::new(
            "mcp-1",
            "restart test",
            "https://example.com/mcp",
            Duration::from_secs(1),
        )
        .expect("test HTTP config");
        let config = McpTransportConfig::Http(http_config.clone());
        let handle = McpServerHandle::Http(
            McpHttpHandle::new(&http_config).expect("the test handle does not make a request"),
        );
        let supervisor = McpSupervisor::with_restart_policy(RestartPolicy {
            enabled: true,
            max_attempts: 1,
            base_delay: Duration::from_millis(250),
            max_delay: Duration::from_millis(250),
            healthy_after: Duration::from_secs(60),
        });
        {
            let mut restart = RestartState::default();
            restart.mark_started();
            supervisor.inner.servers.lock().await.insert(
                "mcp-1".to_string(),
                ManagedServer {
                    handle,
                    config: config.clone(),
                    generation: 1,
                    manual_stop: false,
                    last_exit: None,
                    restart,
                },
            );
        }

        let weak = Arc::downgrade(&supervisor.inner);
        let observer = weak.clone();
        let restart_task = tokio::spawn(automatic_restart(weak, "mcp-1".to_string(), 1, config));

        // Let the task record the retry decision and enter its backoff sleep.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            observer.strong_count(),
            1,
            "the backoff task must not hold a strong supervisor reference"
        );

        drop(supervisor);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            observer.strong_count(),
            0,
            "dropping the last public supervisor handle must release its state immediately"
        );

        tokio::time::timeout(Duration::from_secs(1), restart_task)
            .await
            .expect("the restart task must stop after its supervisor is gone")
            .expect("the restart task must not panic");
    }
}
