//! Server-id keyed MCP process supervision.
//!
//! A crash is reported and then recovered with the same bounded backoff used by the
//! sidecar. Recovery only starts a fresh process and rebuilds its tool catalog; it never
//! replays the call that was in flight when the old process exited.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

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
    start_lock: AsyncMutex<()>,
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
                start_lock: AsyncMutex::new(()),
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
        self.watch_for_exit(config.server_id(), generation, handle);
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
        let mut reports = Vec::with_capacity(handles.len());
        for (server_id, handle) in handles {
            reports.push((server_id, handle.shutdown().await));
        }
        reports
    }

    fn watch_for_exit(&self, server_id: &str, generation: u64, handle: McpServerHandle) {
        let weak = Arc::downgrade(&self.inner);
        let Some(watch) = handle.exit_watch() else {
            return;
        };
        drop(handle);
        let server_id = server_id.to_string();
        tokio::spawn(async move {
            let Some((_pid, exit)) = watch.wait().await else {
                return;
            };
            let Some(inner) = weak.upgrade() else {
                return;
            };
            McpSupervisor { inner }
                .handle_unexpected_exit(&server_id, generation, exit)
                .await;
        });
    }

    async fn handle_unexpected_exit(&self, server_id: &str, generation: u64, exit: McpExitRecord) {
        let restart = {
            let mut servers = self.inner.servers.lock().await;
            let Some(managed) = servers.get_mut(server_id) else {
                return;
            };
            if managed.manual_stop || managed.generation != generation {
                return;
            }
            managed.last_exit = Some(exit);
            managed.config.clone()
        };
        self.automatic_restart(server_id, generation, restart).await;
    }

    async fn automatic_restart(
        &self,
        server_id: &str,
        generation: u64,
        config: McpTransportConfig,
    ) {
        loop {
            let delay = {
                let mut servers = self.inner.servers.lock().await;
                let Some(managed) = servers.get_mut(server_id) else {
                    return;
                };
                if managed.manual_stop || managed.generation != generation {
                    return;
                }
                match managed.restart.decide(
                    &self.inner.restart_policy,
                    Instant::now(),
                    crate::sidecar::iso8601_now(),
                ) {
                    RestartDecision::Exhausted { .. } => return,
                    RestartDecision::Retry { delay, .. } => delay,
                }
            };

            tokio::time::sleep(delay).await;

            let _start = self.inner.start_lock.lock().await;
            {
                let servers = self.inner.servers.lock().await;
                match servers.get(server_id) {
                    Some(managed) if !managed.manual_stop && managed.generation == generation => {}
                    _ => return,
                }
            }

            let handle = match launch_ready(&config).await {
                Ok(handle) => handle,
                Err(_) => continue,
            };

            let installed = {
                let mut servers = self.inner.servers.lock().await;
                match servers.get_mut(server_id) {
                    Some(managed) if !managed.manual_stop && managed.generation == generation => {
                        managed.handle = handle.clone();
                        managed.restart.mark_started();
                        true
                    }
                    _ => false,
                }
            };

            if installed {
                self.watch_for_exit(server_id, generation, handle);
                return;
            }

            handle.shutdown().await;
            return;
        }
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
}
