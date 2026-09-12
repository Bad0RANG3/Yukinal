//! `McpStdioConfig`：**启动什么**，以及「哪种配置根本不该被启动」。
//!
//! 与 `sidecar::config` 同一个位置：这里全是纯逻辑（读一行数据库记录、校验它、拼出启动
//! 参数），没有 IO、不碰 tokio、也不需要 `McpServerHandle`。判断「这个服务器要怎么起」
//! 时不必翻过进程生命周期那几百行。
//!
//! 存储形状直接照单全收：`McpServerConfig`（`crates/database/src/models.rs`，表
//! `mcp_servers`）是**唯一**的配置来源，这里只做校验与翻译，不重新声明一套字段 ——
//! 否则表加了一列、启动器不知道，是两边各自演化才会有的问题。数据库与迁移不在本任务
//! 的改动范围内，所以这里接受已经存在的列（含 `url` / `allowed_tools` / `trust_level`），
//! 只对「能不能启动」负责。

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use yukinal_database::models::McpServerConfig as StoredMcpServer;

use super::descriptor::{self, NameRejection, SEGMENT_PATTERN};
use super::{truncated, McpError};

/// 单次请求的默认超时。
///
/// sidecar 的默认是 10s（它启动的是我们自己构建的 bundle）。MCP 服务是第三方进程：
/// `npx -y @modelcontextprotocol/server-x` 冷启动要下载并解包，几十秒并不罕见。30s 比
/// sidecar 宽，但仍然是一个有限的数 —— 「等多久算卡住」必须有答案。
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// 允许的最小超时。
///
/// 注册表要求 `timeoutMs > 0`（README.md 的「输入、输出与超时由本地强制」），而零超时在这里等于「每次调用立刻失败」：
/// 那不是一种配置，是一个错误。
pub const MIN_REQUEST_TIMEOUT: Duration = Duration::from_millis(50);

/// 允许的最大超时。
///
/// 超过五分钟的单次工具调用，与「卡死」在界面上无法区分；需要更长等待的外部工具应该自己
/// 分片，而不是让宿主一直挂着。
pub const MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// 要启动的 stdio MCP 服务进程。
///
/// 类型本身是一道闸门：**它无法表示 http 服务器**。`transport: "http"` 在
/// [`McpStdioConfig::from_server_config`] 那里就被拒绝了，所以后面任何一条代码路径都不可能
/// 「顺手」把 http 配置当成 stdio 起起来（README.md 的「进程生命周期仍然归 Rust」：出站网络策略还不存在）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpStdioConfig {
    /// 数据库里的服务器 id。它是**身份**：状态查询、事件、`ToolOrigin { kind: "mcp",
    /// serverId }`（README.md 的「目标必须在本地解析」）用的都是这个值，而不是下面那个规范化后的段。
    pub server_id: String,
    /// 内部名段：`mcp.<segment>.<tool>` 里的那一段（ADR 0004），由 `server_id` 规范化而来。
    ///
    /// 两个字段分开，是因为「用户看到的 id」与「内部名里的拼写」是两件事：`mcp_1` 这种
    /// 已经存在的 id 必须能继续当身份用，而它作为内部名段必须先变成 `mcp-1`。
    pub segment: String,
    pub label: String,
    pub program: PathBuf,
    /// 原样传给子进程的参数（数据库里是 JSON 数组，顺序即调用顺序）。
    pub args: Vec<OsString>,
    /// 额外环境变量。数据库没有这一列，所以只可能来自调用方；子进程默认继承宿主环境。
    pub env: Vec<(String, String)>,
    /// 每一次请求（`initialize` / `tools/list` / `tools/call`）的超时。
    pub request_timeout: Duration,
}

impl McpStdioConfig {
    /// 从「一个 id + 一个程序」拼出配置，并顺手校验。
    ///
    /// `clientInfo.version` 不在这里：它是客户端的常量（crate 版本），不是每个服务器各自的
    /// 配置，多一个字段就多一处可以填错的地方。
    pub fn new(
        server_id: &str,
        label: &str,
        program: impl Into<PathBuf>,
        request_timeout: Duration,
    ) -> Result<Self, McpError> {
        let segment = descriptor::normalize_segment(server_id).map_err(|rejection| {
            McpError::InvalidConfig {
                server_id: truncated(server_id),
                reason: format!("the server id {}", rejection.reason),
            }
        })?;
        let config = Self {
            server_id: server_id.to_string(),
            segment,
            label: label.to_string(),
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            request_timeout,
        };
        config.validate()?;
        Ok(config)
    }

    #[must_use]
    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    #[must_use]
    pub fn with_env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_string(), value.to_string()));
        self
    }

    /// 数据库记录 → 启动配置。
    ///
    /// 检查顺序是有意的：**传输方式先判**。`http` 无论这行记录还说了什么（禁用、没写
    /// command）都必须以 [`McpError::TransportNotImplemented`] 结束，因为那是「这个架构
    /// 现在做不到」，而其余几种都是「这行记录还不完整」。
    pub fn from_server_config(
        server: &StoredMcpServer,
        request_timeout: Duration,
    ) -> Result<Self, McpError> {
        // 这里的 id 直接来自一行用户数据，而错误消息会进日志、界面甚至审计：和 `new`/`validate`
        // 一样先截断，别让一个几兆字节的 id 跟着错误一起流出去。
        let server_id = truncated(&server.id);
        match server.transport.as_str() {
            "stdio" => {}
            "http" => {
                return Err(McpError::TransportNotImplemented {
                    server_id,
                    transport: server.transport.clone(),
                })
            }
            other => {
                return Err(McpError::UnsupportedTransport {
                    server_id,
                    transport: truncated(other),
                })
            }
        }

        if !server.enabled {
            // 禁用是一道状态闸门，不是一个可以忽略的字段：把它变成错误，意味着没有哪个
            // 调用方可以「忘了过滤」就把一个被禁用的服务器拉起来。
            return Err(McpError::Disabled { server_id });
        }

        let command = server
            .command
            .as_deref()
            .map(str::trim)
            .filter(|command| !command.is_empty())
            .ok_or(McpError::MissingCommand { server_id })?;

        let mut config = Self::new(
            &server.id,
            &server.label,
            PathBuf::from(command),
            request_timeout,
        )?;
        if let Some(args) = &server.args {
            config = config.with_args(args.iter().cloned());
        }
        // `url` 在 stdio 下没有意义。表允许两个字段同时存在，这里既不报错也不拿它去连任何
        // 东西：http 传输在函数开头就已经被拒绝了，所以一行「url 写了但 transport 是 stdio」
        // 的记录最多是一条无用的记录，而不是一条会被悄悄尊重的配置。
        Ok(config)
    }

    /// 启动前的最后一次校验。
    ///
    /// [`McpStdioConfig::new`] 已经校验过一次，但结构体字段是公开的：一个手写的字面量可以
    /// 绕过它。真正不能被骗的是**派生进程的那一处**，所以 `start` 会再走一遍。
    pub fn validate(&self) -> Result<(), McpError> {
        let invalid = |reason: String| McpError::InvalidConfig {
            server_id: truncated(&self.server_id),
            reason,
        };
        if self.server_id.trim().is_empty() {
            return Err(invalid(
                "the server id is empty; it is the identity every status and event reports"
                    .to_string(),
            ));
        }
        if !descriptor::is_segment(&self.segment) {
            return Err(invalid(format!(
                "the internal name segment \"{}\" does not match `{SEGMENT_PATTERN}`",
                truncated(&self.segment)
            )));
        }
        if self.program.as_os_str().is_empty() {
            return Err(invalid("there is no program to launch".to_string()));
        }
        if self.request_timeout < MIN_REQUEST_TIMEOUT || self.request_timeout > MAX_REQUEST_TIMEOUT
        {
            return Err(invalid(format!(
                "the request timeout must be between {MIN_REQUEST_TIMEOUT:?} and {MAX_REQUEST_TIMEOUT:?}; \
                 a zero or unbounded timeout cannot be told apart from a hang"
            )));
        }
        Ok(())
    }

    /// 内部工具名（ADR 0004）：`mcp.<segment>.<tool>`。
    ///
    /// 适配器（README.md 的「外部工具必须先变成 Yukinal 的工具声明」）把远端工具变成
    /// `ToolDeclaration` 时需要它，所以这个函数必须和
    /// 名字规则一起被校验，而不是让调用方自己 `format!` 一个点号名字出来。
    pub fn internal_tool_name(&self, tool_segment: &str) -> Result<String, NameRejection> {
        descriptor::internal_tool_name(&self.segment, tool_segment)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(transport: &str) -> StoredMcpServer {
        StoredMcpServer {
            id: "mcp_1".to_string(),
            label: "local fs".to_string(),
            transport: transport.to_string(),
            command: Some("npx".to_string()),
            args: Some(vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-filesystem".to_string(),
            ]),
            url: None,
            enabled: true,
            // 这两个字段本模块不读（allowed_tools 为空表示什么都不自动信任，trust_level
            // 决定要不要注册 —— 都是注册期的事，README.md 的「风险等级由本地决定」），
            // 但记录里有，就直接收下。
            allowed_tools: Vec::new(),
            trust_level: "unreviewed".to_string(),
        }
    }

    fn timeout() -> Duration {
        DEFAULT_REQUEST_TIMEOUT
    }

    #[test]
    fn an_http_server_is_refused_by_name_and_reason() {
        let mut server = stored("http");
        server.command = None;
        server.url = Some("https://example.invalid/mcp".to_string());
        // 即使记录同时被禁用、也没有 command，第一判定仍然是传输方式：那是「架构做不到」，
        // 不是「这行记录还不完整」。
        server.enabled = false;

        let error = McpStdioConfig::from_server_config(&server, timeout())
            .expect_err("http transport must be refused, never silently ignored");
        match &error {
            McpError::TransportNotImplemented {
                server_id,
                transport,
            } => {
                assert_eq!(server_id, "mcp_1");
                assert_eq!(transport, "http");
            }
            other => panic!("unexpected error: {other:?}"),
        }
        let message = error.to_string();
        assert!(message.contains("http"), "{message}");
        assert!(
            message.contains("not implemented"),
            "the error must say the transport is missing, not that the config is odd: {message}"
        );
        assert!(
            message.contains("outbound network policy"),
            "the error must name the reason (README.md: the process lifecycle stays with Rust): {message}"
        );
    }

    #[test]
    fn an_unknown_transport_is_refused_too() {
        let error = McpStdioConfig::from_server_config(&stored("sse"), timeout())
            .expect_err("only stdio exists");
        assert!(
            matches!(error, McpError::UnsupportedTransport { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn a_stdio_server_without_a_command_is_a_config_error_not_a_panic() {
        let mut server = stored("stdio");
        server.command = None;
        let error =
            McpStdioConfig::from_server_config(&server, timeout()).expect_err("nothing to launch");
        assert!(
            matches!(error, McpError::MissingCommand { .. }),
            "{error:?}"
        );

        // 空白 command 与缺失是同一件事：一个只写空格的可执行文件路径不是可执行文件。
        server.command = Some("   ".to_string());
        assert!(matches!(
            McpStdioConfig::from_server_config(&server, timeout()).expect_err("blank"),
            McpError::MissingCommand { .. }
        ));
    }

    #[test]
    fn a_disabled_row_must_not_become_a_running_process() {
        let mut server = stored("stdio");
        server.enabled = false;
        let error = McpStdioConfig::from_server_config(&server, timeout())
            .expect_err("a disabled server is not a launch candidate");
        assert!(matches!(error, McpError::Disabled { .. }), "{error:?}");
    }

    #[test]
    fn a_stored_row_carries_its_identity_its_segment_and_its_arguments() {
        let config = McpStdioConfig::from_server_config(&stored("stdio"), timeout())
            .expect("a complete stdio row");
        // 身份保持原样（`mcp_1` 仍然是 id），内部名用的是规范化后的段。
        assert_eq!(config.server_id, "mcp_1");
        assert_eq!(config.segment, "mcp-1");
        assert_eq!(config.program, PathBuf::from("npx"));
        assert_eq!(config.args.len(), 2);
        assert_eq!(
            config.args[1],
            OsString::from("@modelcontextprotocol/server-filesystem")
        );
        assert_eq!(
            config.internal_tool_name("read-file").expect("legal"),
            "mcp.mcp-1.read-file"
        );
    }

    #[test]
    fn a_stdio_row_keeps_a_url_out_of_the_launch_path() {
        let mut server = stored("stdio");
        server.url = Some("https://example.invalid".to_string());
        let config = McpStdioConfig::from_server_config(&server, timeout())
            .expect("a stray url is noise, not a second transport");
        assert_eq!(config.program, PathBuf::from("npx"));
    }

    #[test]
    fn an_id_that_cannot_be_a_segment_is_refused_at_construction() {
        for id in ["", "  ", "MCP-1", "mcp..1", "_mcp"] {
            let error = McpStdioConfig::new(id, "label", "node", timeout())
                .expect_err("this id cannot become an internal name segment");
            assert!(
                matches!(error, McpError::InvalidConfig { .. }),
                "{id:?}: {error:?}"
            );
        }
        // 单下划线是唯一被翻译的写法，所以它必须能通过。
        assert_eq!(
            McpStdioConfig::new("mcp_1", "label", "node", timeout())
                .expect("translated")
                .segment,
            "mcp-1"
        );
    }

    #[test]
    fn validate_catches_what_a_hand_built_config_can_get_wrong() {
        // 字段是公开的，所以字面量能绕过 `new`；`validate` 是派生进程那一处依赖的最后一道。
        let mut config = McpStdioConfig::new("mcp-1", "label", "node", timeout()).expect("valid");

        config.segment = "Not A Segment".to_string();
        assert!(matches!(
            config.validate().expect_err("illegal segment"),
            McpError::InvalidConfig { .. }
        ));

        config.segment = "mcp-1".to_string();
        config.program = PathBuf::new();
        assert!(config.validate().is_err(), "there is nothing to launch");

        config.program = PathBuf::from("node");
        config.request_timeout = Duration::ZERO;
        assert!(
            config.validate().is_err(),
            "a zero timeout is a failure every time, not a configuration"
        );

        config.request_timeout = MAX_REQUEST_TIMEOUT + Duration::from_secs(1);
        assert!(
            config.validate().is_err(),
            "an unbounded wait cannot be told apart from a hang"
        );

        config.request_timeout = timeout();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn environment_entries_are_additive_and_ordered() {
        let config = McpStdioConfig::new("mcp-1", "label", "node", timeout())
            .expect("valid")
            .with_env("MCP_FIXTURE", "1")
            .with_env("PATH", "/usr/bin");
        assert_eq!(
            config.env,
            vec![
                ("MCP_FIXTURE".to_string(), "1".to_string()),
                ("PATH".to_string(), "/usr/bin".to_string())
            ]
        );
    }
}
