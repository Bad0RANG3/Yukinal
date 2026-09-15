//! MCP 传输配置：**连接什么**，以及「哪种配置根本不该被连接」。
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

use std::collections::HashSet;
use std::ffi::OsString;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use reqwest::Url;
use yukinal_database::models::McpServerConfig as StoredMcpServer;
use yukinal_net::{NetworkProxy, ProxyCredential};

use super::descriptor::{self, NameRejection, SEGMENT_PATTERN};
use super::oauth::McpOAuthTokenSource;
use super::{truncated, McpError};

/// 单次请求的默认超时。
///
/// sidecar 的默认是 10s（它启动的是我们自己构建的 bundle）。MCP 服务是第三方进程：
/// `npx -y @modelcontextprotocol/server-x` 冷启动要下载并解包，几十秒并不罕见。30s 比
/// sidecar 宽，但仍然是一个有限的数 —— 「等多久算卡住」必须有答案。
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// 允许的最小超时。
///
/// 注册表要求 `timeoutMs > 0`（docs/boundaries/mcp.md 的「输入、输出与超时由本地强制」），而零超时在这里等于「每次调用立刻失败」：
/// 那不是一种配置，是一个错误。
pub const MIN_REQUEST_TIMEOUT: Duration = Duration::from_millis(50);

/// 允许的最大超时。
///
/// 超过五分钟的单次工具调用，与「卡死」在界面上无法区分；需要更长等待的外部工具应该自己
/// 分片，而不是让宿主一直挂着。
pub const MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// A bounded number of static headers per HTTP endpoint. More than this is not
/// useful authentication configuration; it is an accidental data dump.
pub const MAX_HTTP_AUTH_HEADERS: usize = 16;

/// Validate an OAuth issuer, authorization, or token endpoint with the same
/// origin rules as the MCP endpoint itself.
pub fn validate_oauth_url(server_id: &str, raw: &str) -> Result<String, McpError> {
    validate_http_url(server_id, raw)
}

/// 要启动的 stdio MCP 服务进程。
///
/// 类型本身是一道闸门：它只能表示 stdio。HTTP 由 [`McpHttpConfig`] 表示，两者由
/// [`McpTransportConfig`] 在配置边界明确分流。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpStdioConfig {
    /// 数据库里的服务器 id。它是**身份**：状态查询、事件、`ToolOrigin { kind: "mcp",
    /// serverId }`（docs/boundaries/mcp.md 的「目标必须在本地解析」）用的都是这个值，而不是下面那个规范化后的段。
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
    /// 检查顺序是有意的：**传输方式先判**。调用方如果拿错解析器，`http` 行会在这里得到
    /// [`McpError::UnsupportedTransport`]，而不会被当成 stdio 启动。
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
                return Err(McpError::UnsupportedTransport {
                    server_id,
                    transport: "http".to_string(),
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
        // `url` 在 stdio 下没有意义。表允许两个字段同时存在，这里不拿它去连任何东西：
        // 一行「url 写了但 transport 是 stdio」的记录最多是一条无用记录。
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
    /// 适配器（docs/boundaries/mcp.md 的「外部工具必须先变成 Yukinal 的工具声明」）把远端工具变成
    /// `ToolDeclaration` 时需要它，所以这个函数必须和
    /// 名字规则一起被校验，而不是让调用方自己 `format!` 一个点号名字出来。
    pub fn internal_tool_name(&self, tool_segment: &str) -> Result<String, NameRejection> {
        descriptor::internal_tool_name(&self.segment, tool_segment)
    }
}

/// 一个 Streamable HTTP MCP 端点。
///
/// 只允许显式 URL，远程端点必须使用 HTTPS；明文 HTTP 仅允许回环地址。请求不跟随重定向，
/// 因而服务器不能用 3xx 把请求（以及认证头）带到另一个来源。
#[derive(Clone)]
pub struct McpHttpConfig {
    pub server_id: String,
    pub segment: String,
    pub label: String,
    /// 已解析并重新序列化的绝对 URL。
    pub url: String,
    pub request_timeout: Duration,
    /// Ordered static authentication headers. Protocol-owned headers are rejected
    /// by [`McpHttpAuthHeader::new`], so the transport remains in control of framing.
    pub auth_headers: Vec<McpHttpAuthHeader>,
    /// Dynamic bearer authentication. The source owns all secret material and
    /// refresh policy; the transport only asks for the token to put on one request.
    pub oauth: Option<Arc<dyn McpOAuthTokenSource>>,
    /// How this endpoint is reached on the network（ADR 0022）。
    ///
    /// 默认直连，而且直连是**显式**的（`no_proxy()`）：只有用户选了「系统代理」才会变。
    pub proxy: NetworkProxy,
    /// 代理凭据。手写 `Debug` 的类型，只进系统凭据库。
    pub proxy_credential: Option<ProxyCredential>,
}

impl std::fmt::Debug for McpHttpConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHttpConfig")
            .field("server_id", &self.server_id)
            .field("segment", &self.segment)
            .field("label", &self.label)
            .field("url", &self.url)
            .field("request_timeout", &self.request_timeout)
            .field("auth_headers", &self.auth_headers)
            .field("oauth", &self.oauth.as_ref().map(|_| "<configured>"))
            .field("proxy", &self.proxy)
            .field(
                "proxy_credential",
                &self.proxy_credential.as_ref().map(|_| "<configured>"),
            )
            .finish()
    }
}

/// One explicit authentication header. The value is intentionally private and has a
/// redacting `Debug` implementation so it cannot be logged by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct McpHttpAuthHeader {
    name: String,
    value: String,
}

impl std::fmt::Debug for McpHttpAuthHeader {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHttpAuthHeader")
            .field("name", &self.name)
            .field("value", &"<redacted>")
            .finish()
    }
}

impl McpHttpAuthHeader {
    pub fn new(name: &str, value: &str) -> Result<Self, String> {
        let name = name.trim();
        if name.is_empty()
            || name.len() > 128
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
        {
            return Err("HTTP authentication header name is not a valid header token".to_string());
        }
        let normalized = name.to_ascii_lowercase();
        if matches!(
            normalized.as_str(),
            "accept"
                | "connection"
                | "content-length"
                | "content-type"
                | "host"
                | "mcp-protocol-version"
                | "mcp-session-id"
                | "proxy-authorization"
                | "te"
                | "trailer"
                | "transfer-encoding"
                | "upgrade"
        ) {
            return Err(format!(
                "HTTP authentication header `{name}` is reserved by the MCP transport"
            ));
        }
        if value.is_empty()
            || value.len() > 8 * 1024
            || value
                .bytes()
                .any(|byte| byte == b'\r' || byte == b'\n' || byte == 0)
        {
            return Err(
                "HTTP authentication header value must be 1 to 8192 bytes without controls"
                    .to_string(),
            );
        }
        Ok(Self {
            name: name.to_string(),
            value: value.to_string(),
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl McpHttpConfig {
    pub fn new(
        server_id: &str,
        label: &str,
        url: &str,
        request_timeout: Duration,
    ) -> Result<Self, McpError> {
        let segment = descriptor::normalize_segment(server_id).map_err(|rejection| {
            McpError::InvalidConfig {
                server_id: truncated(server_id),
                reason: format!("the server id {}", rejection.reason),
            }
        })?;
        let url = validate_http_url(server_id, url)?;
        let config = Self {
            server_id: server_id.to_string(),
            segment,
            label: label.to_string(),
            url,
            request_timeout,
            auth_headers: Vec::new(),
            oauth: None,
            proxy: NetworkProxy::Direct,
            proxy_credential: None,
        };
        config.validate()?;
        Ok(config)
    }

    /// 数据库记录 → HTTP 配置。
    pub fn from_server_config(
        server: &StoredMcpServer,
        request_timeout: Duration,
    ) -> Result<Self, McpError> {
        let server_id = truncated(&server.id);
        match server.transport.as_str() {
            "http" => {}
            "stdio" => {
                return Err(McpError::UnsupportedTransport {
                    server_id,
                    transport: "stdio".to_string(),
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
            return Err(McpError::Disabled { server_id });
        }
        let raw_url = server
            .url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .ok_or_else(|| McpError::MissingUrl {
                server_id: server_id.clone(),
            })?;
        Self::new(&server.id, &server.label, raw_url, request_timeout)
    }

    /// 连接前的最后一次校验；公开字段可以绕过 `new`，所以这里必须再检查一遍。
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
        if self.url != validate_http_url(&self.server_id, &self.url)? {
            return Err(invalid(
                "the HTTP endpoint is not in normalized absolute form".to_string(),
            ));
        }
        validate_request_timeout(self.request_timeout, &self.server_id)?;
        if self.auth_headers.len() > MAX_HTTP_AUTH_HEADERS {
            return Err(invalid(format!(
                "an HTTP endpoint may carry at most {MAX_HTTP_AUTH_HEADERS} authentication headers"
            )));
        }
        let mut names = HashSet::with_capacity(self.auth_headers.len());
        for auth in &self.auth_headers {
            if self.oauth.is_some() && auth.name().eq_ignore_ascii_case("authorization") {
                return Err(invalid(
                    "OAuth and a static Authorization header are mutually exclusive".to_string(),
                ));
            }
            McpHttpAuthHeader::new(auth.name(), auth.value()).map_err(|reason| {
                McpError::InvalidConfig {
                    server_id: truncated(&self.server_id),
                    reason,
                }
            })?;
            if !names.insert(auth.name().to_ascii_lowercase()) {
                return Err(invalid(format!(
                    "HTTP authentication header `{}` is configured more than once",
                    auth.name()
                )));
            }
        }
        Ok(())
    }

    pub fn with_auth_header(mut self, name: &str, value: &str) -> Result<Self, McpError> {
        if self.auth_headers.len() >= MAX_HTTP_AUTH_HEADERS {
            return Err(McpError::InvalidConfig {
                server_id: truncated(&self.server_id),
                reason: format!(
                    "an HTTP endpoint may carry at most {MAX_HTTP_AUTH_HEADERS} authentication headers"
                ),
            });
        }
        if self
            .auth_headers
            .iter()
            .any(|header| header.name().eq_ignore_ascii_case(name.trim()))
        {
            return Err(McpError::InvalidConfig {
                server_id: truncated(&self.server_id),
                reason: format!(
                    "HTTP authentication header `{}` is configured more than once",
                    truncated(name.trim())
                ),
            });
        }
        self.auth_headers
            .push(McpHttpAuthHeader::new(name, value).map_err(|reason| {
                McpError::InvalidConfig {
                    server_id: truncated(&self.server_id),
                    reason,
                }
            })?);
        Ok(self)
    }

    /// 这次连接走直连还是系统代理（ADR 0022）。
    ///
    /// 代理只改变连接怎么走，不改变校验什么：endpoint 仍然必须 HTTPS（回环除外），
    /// 重定向仍然不跟随，认证头照旧。
    pub fn with_proxy(mut self, proxy: NetworkProxy, credential: Option<ProxyCredential>) -> Self {
        self.proxy = proxy;
        self.proxy_credential = credential;
        self
    }

    pub fn with_oauth_source(
        mut self,
        source: Arc<dyn McpOAuthTokenSource>,
    ) -> Result<Self, McpError> {
        if self.oauth.is_some() {
            return Err(McpError::InvalidConfig {
                server_id: truncated(&self.server_id),
                reason: "an HTTP endpoint may have only one OAuth token source".to_string(),
            });
        }
        if self
            .auth_headers
            .iter()
            .any(|header| header.name().eq_ignore_ascii_case("authorization"))
        {
            return Err(McpError::InvalidConfig {
                server_id: truncated(&self.server_id),
                reason: "OAuth and a static Authorization header are mutually exclusive"
                    .to_string(),
            });
        }
        self.oauth = Some(source);
        Ok(self)
    }

    pub fn internal_tool_name(&self, tool_segment: &str) -> Result<String, NameRejection> {
        descriptor::internal_tool_name(&self.segment, tool_segment)
    }
}

/// stdio 或 Streamable HTTP。配置边界只产出一个明确变体，后续代码不能把一种传输「顺手」
/// 当成另一种。
#[derive(Debug, Clone)]
pub enum McpTransportConfig {
    Stdio(McpStdioConfig),
    Http(McpHttpConfig),
}

impl McpTransportConfig {
    pub fn from_server_config(
        server: &StoredMcpServer,
        request_timeout: Duration,
    ) -> Result<Self, McpError> {
        match server.transport.as_str() {
            "stdio" => Ok(Self::Stdio(McpStdioConfig::from_server_config(
                server,
                request_timeout,
            )?)),
            "http" => Ok(Self::Http(McpHttpConfig::from_server_config(
                server,
                request_timeout,
            )?)),
            other => Err(McpError::UnsupportedTransport {
                server_id: truncated(&server.id),
                transport: truncated(other),
            }),
        }
    }

    #[must_use]
    pub fn server_id(&self) -> &str {
        match self {
            Self::Stdio(config) => &config.server_id,
            Self::Http(config) => &config.server_id,
        }
    }

    #[must_use]
    pub fn segment(&self) -> &str {
        match self {
            Self::Stdio(config) => &config.segment,
            Self::Http(config) => &config.segment,
        }
    }

    #[must_use]
    pub fn request_timeout(&self) -> Duration {
        match self {
            Self::Stdio(config) => config.request_timeout,
            Self::Http(config) => config.request_timeout,
        }
    }

    pub fn validate(&self) -> Result<(), McpError> {
        match self {
            Self::Stdio(config) => config.validate(),
            Self::Http(config) => config.validate(),
        }
    }

    pub fn internal_tool_name(&self, tool_segment: &str) -> Result<String, NameRejection> {
        match self {
            Self::Stdio(config) => config.internal_tool_name(tool_segment),
            Self::Http(config) => config.internal_tool_name(tool_segment),
        }
    }
}

impl From<McpStdioConfig> for McpTransportConfig {
    fn from(config: McpStdioConfig) -> Self {
        Self::Stdio(config)
    }
}

impl From<McpHttpConfig> for McpTransportConfig {
    fn from(config: McpHttpConfig) -> Self {
        Self::Http(config)
    }
}

fn validate_request_timeout(timeout: Duration, server_id: &str) -> Result<(), McpError> {
    if timeout < MIN_REQUEST_TIMEOUT || timeout > MAX_REQUEST_TIMEOUT {
        return Err(McpError::InvalidConfig {
            server_id: truncated(server_id),
            reason: format!(
                "the request timeout must be between {MIN_REQUEST_TIMEOUT:?} and \
                 {MAX_REQUEST_TIMEOUT:?}; a zero or unbounded timeout cannot be told apart \
                 from a hang"
            ),
        });
    }
    Ok(())
}

fn validate_http_url(server_id: &str, raw: &str) -> Result<String, McpError> {
    let invalid = |reason: String| McpError::InvalidUrl {
        server_id: truncated(server_id),
        reason,
    };
    let url = Url::parse(raw.trim()).map_err(|error| invalid(error.to_string()))?;
    let host = url
        .host_str()
        .ok_or_else(|| invalid("the endpoint has no host".to_string()))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid(
            "credentials must not be embedded in the endpoint URL".to_string(),
        ));
    }
    if url.fragment().is_some() {
        return Err(invalid(
            "the endpoint must not contain a URL fragment".to_string(),
        ));
    }
    if url.query().is_some() {
        return Err(invalid(
            "the endpoint must not contain query parameters; credentials and secrets do not \
             belong in a saved URL"
                .to_string(),
        ));
    }
    match url.scheme() {
        "https" => {}
        "http" if is_loopback_host(host) => {}
        "http" => {
            return Err(invalid(
                "plain HTTP is allowed only for loopback endpoints; remote MCP endpoints must \
                 use HTTPS"
                    .to_string(),
            ))
        }
        scheme => {
            return Err(invalid(format!(
                "the endpoint scheme \"{scheme}\" is unsupported; use https, or http on loopback"
            )))
        }
    }
    Ok(url.to_string())
}

fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
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
            http_auth_headers: Vec::new(),
            oauth: None,
            enabled: true,
            // 这两个字段本模块不读（allowed_tools 为空表示什么都不自动信任，trust_level
            // 决定要不要注册 —— 都是注册期的事，docs/boundaries/mcp.md 的「风险等级由本地决定」），
            // 但记录里有，就直接收下。
            allowed_tools: Vec::new(),
            trust_level: "unreviewed".to_string(),
        }
    }

    fn timeout() -> Duration {
        DEFAULT_REQUEST_TIMEOUT
    }

    #[test]
    fn an_http_server_carries_a_validated_endpoint() {
        let mut server = stored("http");
        server.command = None;
        server.url = Some("https://example.invalid/mcp".to_string());
        let config = McpHttpConfig::from_server_config(&server, timeout()).expect("valid HTTP");
        assert_eq!(config.server_id, "mcp_1");
        assert_eq!(config.segment, "mcp-1");
        assert_eq!(config.url, "https://example.invalid/mcp");

        let transport = McpTransportConfig::from_server_config(&server, timeout())
            .expect("the dispatcher recognizes HTTP");
        let McpTransportConfig::Http(actual) = transport else {
            panic!("the dispatcher must retain HTTP");
        };
        assert_eq!(actual.server_id, config.server_id);
        assert_eq!(actual.segment, config.segment);
        assert_eq!(actual.url, config.url);
        assert_eq!(actual.request_timeout, config.request_timeout);
        assert_eq!(actual.auth_headers, config.auth_headers);

        assert!(matches!(
            McpStdioConfig::from_server_config(&server, timeout())
                .expect_err("the stdio parser cannot silently accept an HTTP row"),
            McpError::UnsupportedTransport { .. }
        ));
    }

    #[test]
    fn http_urls_are_absolute_credential_free_and_https_unless_loopback() {
        assert_eq!(
            validate_http_url("mcp-1", "http://127.0.0.1:3000/mcp").expect("loopback"),
            "http://127.0.0.1:3000/mcp"
        );
        assert_eq!(
            validate_http_url("mcp-1", "http://[::1]:3000/mcp").expect("loopback"),
            "http://[::1]:3000/mcp"
        );
        for url in [
            "http://example.com/mcp",
            "ftp://example.com/mcp",
            "https://user:secret@example.com/mcp",
            "https://example.com/mcp#fragment",
            "https://example.com/mcp?token=secret",
            "not-a-url",
        ] {
            let error = validate_http_url("mcp-1", url).expect_err(url);
            assert!(
                matches!(error, McpError::InvalidUrl { .. }),
                "{url}: {error:?}"
            );
        }
    }

    #[test]
    fn http_auth_headers_are_bounded_reserved_safe_and_redacted() {
        let auth = McpHttpAuthHeader::new("Authorization", "Bearer super-secret")
            .expect("a normal authorization header");
        assert_eq!(auth.name(), "Authorization");
        assert_eq!(auth.value(), "Bearer super-secret");
        assert_eq!(
            format!("{auth:?}"),
            "McpHttpAuthHeader { name: \"Authorization\", value: \"<redacted>\" }"
        );

        for name in ["", "Bad Header", "Host", "content-type", "Mcp-Session-Id"] {
            assert!(
                McpHttpAuthHeader::new(name, "value").is_err(),
                "{name:?} must not become an authentication header"
            );
        }
        for value in ["", "line\rbreak", "line\nbreak", "nul\0byte"] {
            assert!(
                McpHttpAuthHeader::new("X-API-Key", value).is_err(),
                "{value:?} must not reach an HTTP request"
            );
        }
        assert!(McpHttpAuthHeader::new("X-API-Key", &"x".repeat(8 * 1024 + 1)).is_err());

        let config = McpHttpConfig::new("mcp-1", "label", "https://example.invalid/mcp", timeout())
            .expect("valid endpoint")
            .with_auth_header("X-API-Key", "another-secret")
            .expect("valid authentication header")
            .with_auth_header("Authorization", "Bearer third-secret")
            .expect("second header");
        assert_eq!(config.auth_headers.len(), 2);
        assert!(!format!("{config:?}").contains("another-secret"));
        assert!(!format!("{config:?}").contains("third-secret"));
        assert!(config
            .clone()
            .with_auth_header("x-api-key", "duplicate")
            .is_err());

        let mut bounded =
            McpHttpConfig::new("mcp-1", "label", "https://example.invalid/mcp", timeout())
                .expect("valid endpoint");
        for index in 0..MAX_HTTP_AUTH_HEADERS {
            bounded = bounded
                .with_auth_header(&format!("X-Header-{index}"), "value")
                .expect("within the bound");
        }
        assert!(bounded.with_auth_header("X-Overflow", "value").is_err());
    }

    #[test]
    fn an_enabled_http_server_without_a_url_is_a_configuration_error() {
        let mut server = stored("http");
        server.command = None;
        server.url = None;
        let error =
            McpHttpConfig::from_server_config(&server, timeout()).expect_err("nothing to connect");
        assert!(matches!(error, McpError::MissingUrl { .. }), "{error:?}");
    }

    #[test]
    fn an_unknown_transport_is_refused_too() {
        let error = McpTransportConfig::from_server_config(&stored("sse"), timeout())
            .expect_err("only stdio and http exist");
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
