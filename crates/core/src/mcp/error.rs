//! MCP 客户端能报出来的失败。

use std::time::Duration;

/// MCP 客户端能报出来的失败。
///
/// 每一个变体都对应一件调用方**能分别处理**的事：换传输方式、补配置、换服务器、重试、
/// 还是就此罢手。这就是为什么「超时」和「进程死了」是两个变体，而不是一个带消息的
/// `Failed`。
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error(
        "mcp server \"{server_id}\" is configured with transport \"{transport}\", which is not \
         implemented: an http server means outbound network requests, and no outbound network \
         policy exists yet (docs/boundaries/mcp.md: 传输方式只有 stdio，http 要等出站网络策略先定下来)"
    )]
    TransportNotImplemented {
        server_id: String,
        transport: String,
    },

    #[error("mcp server \"{server_id}\" has an unsupported transport \"{transport}\"; only \"stdio\" exists")]
    UnsupportedTransport {
        server_id: String,
        transport: String,
    },

    #[error(
        "mcp server \"{server_id}\" is disabled; a disabled row must not become a running process"
    )]
    Disabled { server_id: String },

    #[error(
        "mcp server \"{server_id}\" is configured for the stdio transport but has no command; \
         there is nothing to launch"
    )]
    MissingCommand { server_id: String },

    #[error("mcp configuration for \"{server_id}\" is invalid: {reason}")]
    InvalidConfig { server_id: String, reason: String },

    #[error(
        "mcp server \"{server_id}\" declared tool \"{name}\", which cannot become an internal tool \
         name segment: {reason}"
    )]
    InvalidToolName {
        server_id: String,
        name: String,
        reason: String,
    },

    #[error(
        "mcp server \"{server_id}\" declared two tools whose internal name segments collide on \
         \"{name}\"; refusing the whole list rather than registering an ambiguous name (ADR 0004)"
    )]
    ToolNameCollision { server_id: String, name: String },

    #[error(
        "mcp server \"{server_id}\" negotiated protocol version \"{version}\", which this client \
         does not speak; supported versions are {supported}"
    )]
    UnsupportedProtocolVersion {
        server_id: String,
        version: String,
        supported: String,
    },

    #[error("mcp server \"{server_id}\" broke the protocol: {reason}")]
    Protocol { server_id: String, reason: String },

    #[error("failed to launch mcp server \"{server_id}\" ({program}): {reason}")]
    Launch {
        server_id: String,
        program: String,
        reason: String,
    },

    #[error("mcp server \"{server_id}\" is not running")]
    NotRunning { server_id: String },

    #[error("{method} on mcp server \"{server_id}\" did not answer within {timeout:?}")]
    Timeout {
        server_id: String,
        method: String,
        timeout: Duration,
    },

    #[error("mcp server \"{server_id}\" answered with a JSON-RPC error {code}: {message}")]
    Remote {
        server_id: String,
        code: i64,
        message: String,
    },

    /// 进程死了。`reason` 是给人看的那一行，`code`/`signal` 是给调用方判定的同一件事
    /// （断言「退出码是 7」比断言消息里有某个子串硬）。
    #[error(
        "mcp server \"{server_id}\" exited ({reason}); the failure is not retried automatically"
    )]
    Exited {
        server_id: String,
        code: Option<i32>,
        signal: Option<String>,
        reason: String,
    },

    #[error("could not write to mcp server \"{server_id}\": {reason}")]
    Write { server_id: String, reason: String },

    #[error("mcp server \"{server_id}\" never advertised a tool named \"{tool}\"")]
    UnknownTool { server_id: String, tool: String },

    #[error(
        "the arguments for mcp tool \"{tool}\" on server \"{server_id}\" must be a JSON object"
    )]
    InvalidArguments { server_id: String, tool: String },
}
