//! 按 serverId 记账的 MCP 服务器管理器。

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::Mutex as AsyncMutex;

use super::config::McpStdioConfig;
use super::descriptor::{McpToolDescriptor, McpToolResult};
use super::error::McpError;
use super::handle::{McpServerHandle, McpServerStart, McpServerStatus, ShutdownReport};
use super::truncated;

/// 谁来派生、谁来回收（MCP README「进程生命周期仍然归 Rust」：
/// 本架构里唯一派生进程的地方是 Rust 宿主）。
///
/// sidecar 的 `Supervisor` 管**一个**进程；MCP 的服务器是**多个**，所以这里按 serverId
/// 记账。除此之外两者是同一套东西：串行化的启动、被监督的句柄、能解释崩溃的状态。
#[derive(Debug, Clone, Default)]
pub struct McpSupervisor {
    inner: Arc<SupervisorInner>,
}

#[derive(Debug, Default)]
struct SupervisorInner {
    /// 把「查表 → 派生 → 握手 → 登记」串起来。没有这道门，两次同时的启动都会看到空位，
    /// 于是同一个 serverId 起来两个进程 —— 单实例就从这里漏掉。
    start_lock: AsyncMutex<()>,
    servers: AsyncMutex<HashMap<String, McpServerHandle>>,
}

impl McpSupervisor {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 启动一个服务器：已经跑着就复用，否则派生 + `initialize` + `tools/list`。
    ///
    /// 握手或工具表失败时不会留下半个活着的进程。
    pub async fn start(&self, config: &McpStdioConfig) -> Result<McpServerStart, McpError> {
        let _start = self.inner.start_lock.lock().await;

        if let Some(handle) = self.handle(&config.server_id).await {
            if handle.is_running() {
                return Ok(McpServerStart {
                    info: handle.info(),
                    tool_count: handle.tools().len(),
                    already_running: true,
                });
            }
            // 一个已经死掉的句柄还留在表里是正常的（崩溃要被记住）。显式启动是唯一能让它
            // 变成「跑着的」的动作，所以这里继续往下走，用新进程盖掉它。
        }

        let handle = McpServerHandle::spawn(config).await?;
        if let Err(error) = handle.initialize(config.request_timeout).await {
            handle.shutdown().await;
            return Err(error);
        }
        if let Err(error) = handle.list_tools(config.request_timeout).await {
            handle.shutdown().await;
            return Err(error);
        }

        let start = McpServerStart {
            info: handle.info(),
            tool_count: handle.tools().len(),
            already_running: false,
        };
        self.inner
            .servers
            .lock()
            .await
            .insert(config.server_id.clone(), handle);
        Ok(start)
    }

    pub async fn handle(&self, server_id: &str) -> Option<McpServerHandle> {
        self.inner.servers.lock().await.get(server_id).cloned()
    }

    /// 这个 supervisor 正在记账的 serverId（含已经死掉、但记录还留着的）。
    pub async fn servers(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.inner.servers.lock().await.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// 一个服务器的工具表（缓存）。没管过、或者还没 `tools/list` 成功时是空表。
    pub async fn tools(&self, server_id: &str) -> Vec<McpToolDescriptor> {
        match self.handle(server_id).await {
            Some(handle) => handle.tools(),
            None => Vec::new(),
        }
    }

    /// 调一个工具。超时用这个服务器自己的配置：超时是**每个服务器**的策略，不是每次调用的
    /// 参数（需要更短的等待时，拿 [`McpSupervisor::handle`] 上的 `call_tool` 自己指定）。
    pub async fn call(
        &self,
        server_id: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<McpToolResult, McpError> {
        let handle = self
            .handle(server_id)
            .await
            .ok_or_else(|| McpError::NotRunning {
                server_id: truncated(server_id),
            })?;
        let timeout = handle.request_timeout();
        handle.call_tool(tool, arguments, timeout).await
    }

    pub async fn status(&self, server_id: &str) -> McpServerStatus {
        let Some(handle) = self.handle(server_id).await else {
            return McpServerStatus::untracked(server_id);
        };
        let info = handle.info();
        let running = handle.is_running();
        let handshake = info.handshake;
        McpServerStatus {
            server_id: info.server_id,
            running,
            pid: running.then_some(info.pid),
            program: Some(info.program),
            started_at: Some(info.started_at),
            protocol_version: handshake.as_ref().map(|it| it.protocol_version.clone()),
            server_name: handshake.as_ref().map(|it| it.server_name.clone()),
            server_version: handshake.as_ref().map(|it| it.server_version.clone()),
            tool_count: handle.tools().len(),
            last_exit: handle.last_exit(),
            stderr_tail: handle.stderr_tail(),
            diagnostics: handle.diagnostics(),
        }
    }

    /// 关掉一个服务器。`None` 表示这个 supervisor 从没管过这个 id。
    pub async fn shutdown(&self, server_id: &str) -> Option<ShutdownReport> {
        let handle = self.handle(server_id).await?;
        Some(handle.shutdown().await)
    }

    /// 关掉全部服务器，返回每个 id 的关闭结果。
    pub async fn shutdown_all(&self) -> Vec<(String, ShutdownReport)> {
        let handles: Vec<(String, McpServerHandle)> = self
            .inner
            .servers
            .lock()
            .await
            .iter()
            .map(|(id, handle)| (id.clone(), handle.clone()))
            .collect();
        let mut reports = Vec::with_capacity(handles.len());
        for (server_id, handle) in handles {
            reports.push((server_id, handle.shutdown().await));
        }
        reports
    }
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
        assert!(
            status.last_exit.is_none(),
            "a server we never ran has no exit to explain"
        );

        // 「调用一个没在跑的服务器」必须是一个说得清的失败，而不是挂住或者 panic。
        let error = supervisor
            .call("mcp-1", "echo", serde_json::json!({}))
            .await
            .expect_err("nothing to call");
        assert!(matches!(error, McpError::NotRunning { .. }), "{error:?}");
    }

    #[tokio::test]
    async fn the_status_payload_omits_the_exit_record_while_nothing_went_wrong() {
        // 状态结构会被接线那一步直接序列化给界面，所以这里钉住「没事发生时不出现空字段」，
        // 与 `SupervisorStatus::restart` 用的是同一条理由。
        let status = McpSupervisor::new().status("mcp-1").await;
        let payload = serde_json::to_value(&status).expect("serialize");
        assert!(payload.get("lastExit").is_none());
        assert_eq!(payload["running"], serde_json::json!(false));
        assert_eq!(payload["toolCount"], serde_json::json!(0));
    }
}
