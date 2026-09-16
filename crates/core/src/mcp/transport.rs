//! Transport-neutral handle used by the supervisor and catalog.

use std::time::Duration;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::descriptor::{McpExitRecord, McpToolDescriptor, McpToolResult};
use super::error::McpError;
use super::handle::{McpExitWatch, McpServerInfo, McpStdioHandle, ShutdownReport};
use super::http::McpHttpHandle;
use super::wire::McpInitialize;

#[derive(Debug, Clone)]
pub enum McpServerHandle {
    Stdio(McpStdioHandle),
    Http(McpHttpHandle),
}

impl McpServerHandle {
    #[must_use]
    pub fn server_id(&self) -> &str {
        match self {
            Self::Stdio(handle) => handle.server_id(),
            Self::Http(handle) => handle.server_id(),
        }
    }

    #[must_use]
    pub fn segment(&self) -> &str {
        match self {
            Self::Stdio(handle) => handle.segment(),
            Self::Http(handle) => handle.segment(),
        }
    }

    #[must_use]
    pub fn request_timeout(&self) -> Duration {
        match self {
            Self::Stdio(handle) => handle.request_timeout(),
            Self::Http(handle) => handle.request_timeout(),
        }
    }

    #[must_use]
    pub fn is_running(&self) -> bool {
        match self {
            Self::Stdio(handle) => handle.is_running(),
            Self::Http(handle) => handle.is_running(),
        }
    }

    #[must_use]
    pub fn info(&self) -> McpServerInfo {
        match self {
            Self::Stdio(handle) => handle.info(),
            Self::Http(handle) => handle.info(),
        }
    }

    #[must_use]
    pub fn handshake(&self) -> Option<McpInitialize> {
        match self {
            Self::Stdio(handle) => handle.handshake(),
            Self::Http(handle) => handle.handshake(),
        }
    }

    #[must_use]
    pub fn tools(&self) -> Vec<McpToolDescriptor> {
        match self {
            Self::Stdio(handle) => handle.tools(),
            Self::Http(handle) => handle.tools(),
        }
    }

    #[must_use]
    pub fn last_exit(&self) -> Option<McpExitRecord> {
        match self {
            Self::Stdio(handle) => handle.last_exit(),
            Self::Http(handle) => handle.last_exit(),
        }
    }

    #[must_use]
    pub fn stderr_tail(&self) -> Vec<String> {
        match self {
            Self::Stdio(handle) => handle.stderr_tail(),
            Self::Http(handle) => handle.stderr_tail(),
        }
    }

    #[must_use]
    pub fn diagnostics(&self) -> Vec<String> {
        match self {
            Self::Stdio(handle) => handle.diagnostics(),
            Self::Http(handle) => handle.diagnostics(),
        }
    }

    pub async fn initialize(&self, timeout: Duration) -> Result<McpInitialize, McpError> {
        match self {
            Self::Stdio(handle) => handle.initialize(timeout).await,
            Self::Http(handle) => handle.initialize(timeout).await,
        }
    }

    pub async fn list_tools(&self, timeout: Duration) -> Result<Vec<McpToolDescriptor>, McpError> {
        match self {
            Self::Stdio(handle) => handle.list_tools(timeout).await,
            Self::Http(handle) => handle.list_tools(timeout).await,
        }
    }

    pub async fn call_tool_with_cancel(
        &self,
        tool: &str,
        arguments: Value,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<McpToolResult, McpError> {
        match self {
            Self::Stdio(handle) => {
                handle
                    .call_tool_with_cancel(tool, arguments, timeout, cancel)
                    .await
            }
            Self::Http(handle) => {
                handle
                    .call_tool_with_cancel(tool, arguments, timeout, cancel)
                    .await
            }
        }
    }

    pub async fn call_tool(
        &self,
        tool: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Result<McpToolResult, McpError> {
        self.call_tool_with_cancel(tool, arguments, timeout, &CancellationToken::new())
            .await
    }

    pub async fn shutdown(&self) -> ShutdownReport {
        match self {
            Self::Stdio(handle) => handle.shutdown().await,
            Self::Http(handle) => handle.shutdown().await,
        }
    }

    pub(crate) async fn force_stop(&self) -> ShutdownReport {
        match self {
            Self::Stdio(handle) => handle.force_stop().await,
            Self::Http(handle) => handle.force_stop(),
        }
    }

    pub(crate) fn exit_watch(&self) -> Option<McpExitWatch> {
        match self {
            Self::Stdio(handle) => Some(handle.exit_watch()),
            Self::Http(_) => None,
        }
    }
}
