//! 交给 sidecar 的 MCP 目录（`host.mcp.catalog`），以及 `mcp.<server>.<tool>` 名字的解析。
//!
//! 按本仓库的分层规则（`apps/desktop/src-tauri` 只做参数编组与事件转发，真逻辑放 `crates/*`），
//! 这些规则原先住在 `apps/desktop/src-tauri/src/commands/mcp.rs` 里。它们需要的东西这里全都有
//! （[`Database`] 与 [`McpSupervisor`]），需要的 Tauri 一样也没有，所以搬得进来 —— 而搬进来
//! 之后「目录会启动一个从未启动过的服务器」这条唯一有副作用的读路径也就有了直接的测试。
//!
//! # 目录为什么必须启动进程
//!
//! MCP 没有静态工具表：要回答「这个服务器有哪些工具」，唯一的办法是把进程起起来问它。所以
//! [`catalog`] 有一条明确的边界，三条都不肯让步：
//!
//! - 只碰 `enabled` 且 `transport == "stdio"` 的行；
//! - 只启动 supervisor **从未管过** 的 id。已经死掉的句柄带着退出记录留在表里，只被报告，
//!   **不被重启**（重启可能把上一次的副作用再执行一遍，ADR 0014）；
//! - 总等待时间不超过 [`CATALOG_START_BUDGET`]。

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use yukinal_database::models::McpServerConfig;
use yukinal_database::Database;
use yukinal_net::OutboundProxy;

use crate::supervisor::RestartRecord;

use super::config::{McpTransportConfig, DEFAULT_REQUEST_TIMEOUT};
use super::descriptor::{
    internal_tool_name, is_segment, McpExitRecord, McpToolDescriptor, MCP_NAMESPACE,
};
use super::error::McpError;
use super::oauth::{McpOAuthSourceConfig, McpOAuthTokenSource};
use super::supervisor::McpSupervisor;
use super::truncated;

/// Resolves a stored credential reference without making the MCP core own the
/// operating-system credential backend.
pub trait McpCredentialResolver: Send + Sync {
    fn resolve(&self, reference: &str) -> Result<String, String>;

    /// Build the host-owned OAuth token source. Core never reads or writes the
    /// credential store itself; deployments without OAuth reject this path.
    fn oauth_source(
        &self,
        _config: &McpOAuthSourceConfig,
    ) -> Result<Arc<dyn McpOAuthTokenSource>, String> {
        Err("OAuth token sources are unavailable on this path".to_string())
    }

    /// 出站代理（ADR 0022）。
    ///
    /// 这是**应用级**设置，不是每台服务器一份；默认直连，所以没有这一项的部署（测试、
    /// 嵌入式用法）行为一字不变。宿主实现它时会同时把凭据从系统凭据库取出来。
    fn outbound_proxy(&self) -> Result<OutboundProxy, String> {
        Ok(OutboundProxy::default())
    }
}

struct NoCredentialResolver;

impl McpCredentialResolver for NoCredentialResolver {
    fn resolve(&self, reference: &str) -> Result<String, String> {
        Err(format!(
            "MCP credential `{reference}` cannot be resolved on this path"
        ))
    }
}

/// 目录请求最多为「启动一个从未启动过的服务器」等多久（总量，不是每个服务器）。
///
/// 目录是 sidecar 在**握手期间**唯一会问的东西，而宿主给 sidecar 的握手预算是 10 秒
/// （`crates/core/src/sidecar/config.rs`，`YUKINAL_AGENT_TIMEOUT_SECS`）。冷启动的
/// `npx -y …` 可以慢到几十秒，所以这里必须有上限：超过预算的服务器这一次不出现，
/// 报告成 [`McpFailureCode::Timeout`]，用户可以显式启动它再重启 Agent。
pub const CATALOG_START_BUDGET: Duration = Duration::from_secs(4);

/// 一个 MCP 服务器「为什么不能用」。码是给分支用的，消息是给人看的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpFailureCode {
    Disabled,
    InvalidConfig,
    LaunchFailed,
    /// The process exited. Bounded automatic recovery may be pending or exhausted.
    Exited,
    Timeout,
    RequestFailed,
}

impl McpFailureCode {
    /// 全部取值，按声明的顺序。`packages/shared/src/types/host.ts` 的
    /// `MCP_CATALOG_FAILURE_CODES` 是同一张表的 TypeScript 一侧，测试把两者钉在一起。
    ///
    /// `cfg(test)` 是准确的：这张表唯一的用途就是那个「两边词形一致」的断言 —— 生产代码
    /// 只按名字用单个取值，从不遍历。不加 `cfg(test)` 的话 lib target 会（正确地）报它
    /// 从未被使用。
    #[cfg(test)]
    pub const ALL: [Self; 6] = [
        Self::Disabled,
        Self::InvalidConfig,
        Self::LaunchFailed,
        Self::Exited,
        Self::Timeout,
        Self::RequestFailed,
    ];

    /// 一个 `McpError` 在线上算哪一类。这个映射是**穷尽**的（没有 `_` 分支）：core 里加一个
    /// 变体时，这里必须一起想清楚它属于哪一类，而不是被一个通配符悄悄归进「别的」。
    #[must_use]
    pub fn of(error: &McpError) -> Self {
        match error {
            McpError::UnsupportedTransport { .. } => Self::InvalidConfig,
            McpError::Disabled { .. } => Self::Disabled,
            McpError::MissingCommand { .. }
            | McpError::MissingUrl { .. }
            | McpError::InvalidUrl { .. }
            | McpError::InvalidConfig { .. }
            | McpError::InvalidToolName { .. }
            | McpError::ToolNameCollision { .. }
            | McpError::UnsupportedProtocolVersion { .. }
            | McpError::UnknownTool { .. }
            | McpError::InvalidArguments { .. }
            | McpError::Protocol { .. } => Self::InvalidConfig,
            McpError::Launch { .. } => Self::LaunchFailed,
            McpError::Exited { .. } | McpError::NotRunning { .. } => Self::Exited,
            McpError::Cancelled { .. } => Self::RequestFailed,
            McpError::Timeout { .. } => Self::Timeout,
            McpError::Remote { .. } | McpError::Write { .. } | McpError::Http { .. } => {
                Self::RequestFailed
            }
            McpError::OAuth { .. } => Self::RequestFailed,
        }
    }
}

/// 目录里的一个工具。字段来自 `McpToolDescriptor`，不是另造一套形状：`description` 与
/// `input_schema` 是远端声明**原样**带出来的不可信内容（docs/boundaries/mcp.md 的「描述文本一律视为不可信数据」）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCatalogTool {
    /// 内部工具名（ADR 0004）：`mcp.<server>.<tool>`。sidecar 注册进 ToolRegistry 的就是它。
    pub name: String,
    /// 服务器 id（身份，不是内部名里的那一段）。
    pub server_id: String,
    /// `tools/call` 要用的名字段。
    pub tool: String,
    /// 只有远端拼写被改写时才出现，出现时告诉用户「远端叫它什么」。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_name: Option<String>,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCatalogServer {
    pub server_id: String,
    pub segment: String,
    pub label: String,
    pub tools: Vec<McpCatalogTool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCatalogFailure {
    pub server_id: String,
    pub code: McpFailureCode,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCatalogResponse {
    pub servers: Vec<McpCatalogServer>,
    pub failures: Vec<McpCatalogFailure>,
}

/// 交给 sidecar 的目录。
///
/// **它会启动从未启动过的服务器**，因为 MCP 没有静态工具表：要回答「这个服务器有哪些工具」，
/// 唯一的办法是把进程起起来问它。这是这条路径上唯一一处「读操作有副作用」，所以它有三条明确
/// 的边界：
///
/// - 只碰 `enabled` 且 `transport == "stdio"` 的行；
/// - 只启动 supervisor **从未管过** 的 id。已经死掉的句柄带着退出记录留在表里，只被报告，
///   **不被重启**（重启可能把上一次的副作用再执行一遍）；
/// - 总等待时间不超过 [`CATALOG_START_BUDGET`]。
pub async fn catalog(
    database: &Database,
    supervisor: &McpSupervisor,
) -> Result<McpCatalogResponse, String> {
    catalog_with_credentials(database, supervisor, &NoCredentialResolver).await
}

/// Same catalog with an explicit credential resolver used only for HTTP auth.
pub async fn catalog_with_credentials(
    database: &Database,
    supervisor: &McpSupervisor,
    credentials: &dyn McpCredentialResolver,
) -> Result<McpCatalogResponse, String> {
    let rows = database
        .mcp_servers()
        .list()
        .map_err(|error| format!("读取 MCP 服务器列表失败：{error}"))?;

    let mut failures = Vec::new();
    let mut candidates: Vec<(McpServerConfig, McpTransportConfig)> = Vec::new();
    for row in rows.into_iter().filter(|row| row.enabled) {
        match McpTransportConfig::from_server_config(&row, DEFAULT_REQUEST_TIMEOUT) {
            Ok(mut config) => match attach_http_auth(&row, &mut config, credentials) {
                Ok(()) => candidates.push((row, config)),
                Err(error) => failures.push(McpCatalogFailure {
                    server_id: row.id.clone(),
                    code: McpFailureCode::of(&error),
                    message: error.to_string(),
                }),
            },
            Err(error) => failures.push(McpCatalogFailure {
                server_id: row.id.clone(),
                code: McpFailureCode::of(&error),
                message: error.to_string(),
            }),
        }
    }

    // 两个 id 可以规范化成同一个名字段（`mcp_1` 与 `mcp-1` 都会变成 `mcp-1`）。内部名会因此
    // 撞车，而调用方的 `mcp.mcp-1.<tool>` 无法说明它指的是哪一个 —— 所以第二个不进目录，
    // 理由写清楚。（表里 `ORDER BY label` 不保证同 label 的顺序，所以这里按 id 排。）
    candidates.sort_by(|left, right| left.0.id.cmp(&right.0.id));
    let mut seen_segments: Vec<(String, String)> = Vec::new();
    candidates.retain(|(row, config)| {
        if let Some((_, first)) = seen_segments
            .iter()
            .find(|(segment, _)| segment == config.segment())
        {
            failures.push(McpCatalogFailure {
                server_id: row.id.clone(),
                code: McpFailureCode::InvalidConfig,
                message: format!(
                    "mcp server \"{}\" normalizes to the same internal name segment \"{}\" as \
                     \"{first}\"; only one of them can be addressed as \
                     \"{MCP_NAMESPACE}.{}.<tool>\" (ADR 0004). Rename one of them.",
                    row.id,
                    config.segment(),
                    config.segment()
                ),
            });
            return false;
        }
        seen_segments.push((config.segment().to_string(), row.id.clone()));
        true
    });

    let deadline = Instant::now() + CATALOG_START_BUDGET;
    let mut servers = Vec::with_capacity(candidates.len());
    for (row, config) in candidates {
        match supervisor.handle(&row.id).await {
            Some(handle) if handle.is_running() => {
                servers.push(catalog_server(&row, &config, handle.tools(), &mut failures));
            }
            // 管过但没在跑：崩溃记录与当前恢复尝试一起交出去。后台恢复会按有界
            // 退避重建进程；这里不等待它，也不把「正在恢复」说成已经可用。
            Some(handle) => {
                let status = supervisor.status(&row.id).await;
                failures.push(McpCatalogFailure {
                    server_id: row.id.clone(),
                    code: McpFailureCode::Exited,
                    message: describe_dead(&row.id, handle.last_exit(), status.restart),
                });
            }
            None => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    failures.push(budget_exhausted(&row.id));
                    continue;
                }
                match tokio::time::timeout(remaining, supervisor.start_transport(&config)).await {
                    Ok(Ok(_)) => {
                        let tools = supervisor.tools(&row.id).await;
                        servers.push(catalog_server(&row, &config, tools, &mut failures));
                    }
                    Ok(Err(error)) => failures.push(McpCatalogFailure {
                        server_id: row.id.clone(),
                        code: McpFailureCode::of(&error),
                        message: error.to_string(),
                    }),
                    Err(_elapsed) => failures.push(budget_exhausted(&row.id)),
                }
            }
        }
    }

    Ok(McpCatalogResponse { servers, failures })
}

fn attach_http_auth(
    row: &McpServerConfig,
    config: &mut McpTransportConfig,
    credentials: &dyn McpCredentialResolver,
) -> Result<(), McpError> {
    match config {
        McpTransportConfig::Stdio(_) if !row.http_auth_headers.is_empty() => {
            Err(McpError::InvalidConfig {
                server_id: truncated(&row.id),
                reason: "stdio transport cannot carry HTTP authentication settings".to_string(),
            })
        }
        McpTransportConfig::Stdio(_) if row.oauth.is_some() => Err(McpError::InvalidConfig {
            server_id: truncated(&row.id),
            reason: "stdio transport cannot carry OAuth authentication settings".to_string(),
        }),
        McpTransportConfig::Stdio(_) => Ok(()),
        McpTransportConfig::Http(http) => {
            // 代理先落地：它在 URL 校验之后、任何请求之前，而且「读到了却用不了」的代理
            // 配置必须在这里失败（`McpHttpHandle::new` 会把它变成启动错误）。
            let outbound =
                credentials
                    .outbound_proxy()
                    .map_err(|reason| McpError::InvalidConfig {
                        server_id: truncated(&row.id),
                        reason,
                    })?;
            let mut next = http
                .clone()
                .with_proxy(outbound.proxy.clone(), outbound.credential.clone());
            for header in &row.http_auth_headers {
                if header.name.trim().is_empty() || header.credential_ref.trim().is_empty() {
                    return Err(McpError::InvalidConfig {
                        server_id: truncated(&row.id),
                        reason:
                            "HTTP authentication headers require a name and credential reference"
                                .to_string(),
                    });
                }
                let secret = credentials
                    .resolve(&header.credential_ref)
                    .map_err(|reason| McpError::InvalidConfig {
                        server_id: truncated(&row.id),
                        reason: format!(
                            "could not resolve HTTP authentication credential: {reason}"
                        ),
                    })?;
                next = next.with_auth_header(&header.name, &secret)?;
            }
            if let Some(oauth) = &row.oauth {
                // A secret method without a stored secret cannot authenticate at all, so it
                // is refused here rather than failing later with a token request that the
                // server reads as an empty credential.
                if oauth.client_auth.needs_secret() && oauth.client_secret_ref.is_none() {
                    return Err(McpError::InvalidConfig {
                        server_id: truncated(&row.id),
                        reason: "OAuth uses client secret authentication but no secret is stored"
                            .to_string(),
                    });
                }
                if oauth.client_auth.needs_secret() && oauth.client_id.trim().is_empty() {
                    return Err(McpError::InvalidConfig {
                        server_id: truncated(&row.id),
                        reason: "client secret authentication needs a hand-filled client id"
                            .to_string(),
                    });
                }
                let token_endpoint =
                    oauth
                        .token_endpoint
                        .as_deref()
                        .ok_or_else(|| McpError::InvalidConfig {
                            server_id: truncated(&row.id),
                            reason: "OAuth is not connected: no token endpoint is stored"
                                .to_string(),
                        })?;
                let credential_ref =
                    oauth
                        .credential_ref
                        .as_deref()
                        .ok_or_else(|| McpError::InvalidConfig {
                            server_id: truncated(&row.id),
                            reason: "OAuth is not connected: no token credential is stored"
                                .to_string(),
                        })?;
                let source = credentials
                    .oauth_source(&McpOAuthSourceConfig {
                        server_id: row.id.clone(),
                        resource: next.url.clone(),
                        token_endpoint: token_endpoint.to_string(),
                        client_id: oauth.client_id.clone(),
                        client_auth: oauth.client_auth,
                        client_secret_ref: oauth.client_secret_ref.clone(),
                        dpop_key_ref: oauth.dpop_key_ref.clone(),
                        // token 请求与资源请求走同一条路：同一份解析结果，不各查一次环境变量。
                        proxy: outbound.clone(),
                        scopes: oauth.scopes.clone(),
                        credential_ref: credential_ref.to_string(),
                    })
                    .map_err(|reason| McpError::OAuth {
                        server_id: truncated(&row.id),
                        reason,
                    })?;
                next = next.with_oauth_source(source)?;
            }
            *http = next;
            Ok(())
        }
    }
}

fn budget_exhausted(server_id: &str) -> McpCatalogFailure {
    McpCatalogFailure {
        server_id: server_id.to_string(),
        code: McpFailureCode::Timeout,
        message: format!(
            "mcp server \"{server_id}\" was not started within this catalog request's \
             {CATALOG_START_BUDGET:?} budget; start it explicitly and restart the Agent"
        ),
    }
}

fn catalog_server(
    row: &McpServerConfig,
    config: &McpTransportConfig,
    descriptors: Vec<McpToolDescriptor>,
    failures: &mut Vec<McpCatalogFailure>,
) -> McpCatalogServer {
    let mut tools = Vec::with_capacity(descriptors.len());
    for descriptor in descriptors {
        // Registration is not trust. Only a deliberate review decision can expose a
        // third-party tool to the model; the permission engine still asks per call.
        if !row
            .allowed_tools
            .iter()
            .any(|allowed| allowed == &descriptor.name)
        {
            continue;
        }
        match internal_tool_name(config.segment(), &descriptor.name) {
            Ok(name) => tools.push(McpCatalogTool {
                name,
                server_id: row.id.clone(),
                tool: descriptor.name.clone(),
                remote_name: descriptor.remote_name.clone(),
                description: descriptor.description.clone(),
                input_schema: descriptor.input_schema.clone(),
            }),
            // `internal_tool_name` 只在长度预算被打爆时失败，而那个预算在导入时就已经由
            // `SEGMENT_MAX_LENGTH` 保证过。走到这里意味着某处不变量破了：跳过这个工具并如实
            // 报告，而不是把整个服务器丢掉。
            Err(rejection) => failures.push(McpCatalogFailure {
                server_id: row.id.clone(),
                code: McpFailureCode::InvalidConfig,
                message: rejection.to_string(),
            }),
        }
    }
    McpCatalogServer {
        server_id: row.id.clone(),
        segment: config.segment().to_string(),
        label: row.label.clone(),
        tools,
    }
}

/// `mcp.` 前缀的名字是不是 MCP 工具名（`host.tool.execute` 的分流条件）。
#[must_use]
pub fn is_mcp_tool_name(name: &str) -> bool {
    name.strip_prefix(MCP_NAMESPACE)
        .is_some_and(|rest| rest.starts_with('.'))
}

/// `mcp.<server-segment>.<tool-segment>` → `(server, tool)`。
///
/// **恰好三段。** 少一段不是 MCP 名字；多一段是伪造的 —— 工具的拼写在导入时就已经被规范化过
/// （段里不可能有点，见 [`super::descriptor`]），所以 `mcp.a.b.c` 只能是调用方自己拼出来的。
#[must_use]
pub fn split_mcp_tool_name(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix(MCP_NAMESPACE)?.strip_prefix('.')?;
    let (server, tool) = rest.split_once('.')?;
    if !is_segment(server) || !is_segment(tool) {
        return None;
    }
    Some((server, tool))
}

/// 崩掉的服务器要说的那一句：退出记录 + 「不会自己回来」+ 下一步。
#[must_use]
pub fn describe_dead(
    server_id: &str,
    exit: Option<McpExitRecord>,
    restart: Option<RestartRecord>,
) -> String {
    let recovery = match restart {
        Some(record) if record.exhausted => format!(
            "automatic recovery stopped after {}/{} attempts",
            record.attempt, record.max_attempts
        ),
        Some(record) => format!(
            "automatic recovery attempt {}/{} is pending",
            record.attempt, record.max_attempts
        ),
        None => "automatic recovery has not started".to_string(),
    };
    match exit {
        Some(record) => format!(
            "mcp server \"{server_id}\" exited ({}) at {}; {recovery}. Recovery only rebuilds \
             the process and tool catalog; it never replays the interrupted call.",
            record.reason(),
            record.at
        ),
        None => format!(
            "mcp server \"{server_id}\" is not running; {recovery}. Recovery only rebuilds the \
             process and tool catalog; it never replays the interrupted call"
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use super::*;
    use crate::mcp::McpOAuthFuture;

    /* ── 测试脚手架 ───────────────────────────────────────────────────────── */

    /// 一个临时数据库文件。
    ///
    /// 不用 `Database::in_memory()`：它是 `yukinal-database` 自己的 `#[cfg(test)]`，跨 crate
    /// 取不到（`crates/database/src/lib.rs`）。走文件顺带证明这条路径面对的就是磁盘上那份
    /// schema 与那份迁移结果。名字带进程 id 与计数器，所以并行跑的测试互不干扰。
    fn temp_db(name: &str) -> (PathBuf, Database) {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "yukinal-mcp-catalog-{}-{}-{}.sqlite",
            std::process::id(),
            name,
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        cleanup(&path);
        let database = Database::open(&path).expect("open the temp database");
        (path, database)
    }

    /// SQLite 的 WAL 会在旁边留下两个文件，一起删掉。
    fn cleanup(path: &Path) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(PathBuf::from(format!("{}{suffix}", path.display())));
        }
    }

    /// 直接写一行（绕过 `save` 的校验：**已经存在**于表里的 `http` 行就是这一类）。
    fn insert(database: &Database, row: &McpServerConfig) {
        database.mcp_servers().upsert(row).expect("upsert row");
    }

    fn fixture_path() -> PathBuf {
        // 与 `crates/core/tests/mcp_stdio.rs` 用的是**同一个**已提交的 fixture。
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("mcp-server.js")
    }

    fn node_path() -> Option<PathBuf> {
        std::env::var("YUKINAL_TEST_NODE")
            .ok()
            .map(PathBuf::from)
            .filter(|path| path.is_file())
    }

    fn required() -> bool {
        std::env::var("YUKINAL_TEST_REQUIRED")
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    }

    /// `None` = 这台机器没有 node（`node_or_skip` 已经决定那是跳过还是失败）。
    fn node_or_skip() -> Option<PathBuf> {
        let node = node_path();
        if node.is_none() {
            let message =
                "missing YUKINAL_TEST_NODE: set it, or unset YUKINAL_TEST_REQUIRED to skip";
            if required() {
                panic!("{message}");
            }
            eprintln!("skipped: {message}");
        }
        node
    }

    /// fixture 行：把已提交的 Node MCP 服务进程真的跑起来。
    fn fixture_row(id: &str, mode: &str, enabled: bool) -> McpServerConfig {
        // Disabled/collision tests never launch this row, so they must not require
        // a Node installation merely to construct a stored fixture.
        let node = node_path().unwrap_or_else(|| PathBuf::from("node"));
        McpServerConfig {
            id: id.to_string(),
            label: format!("fixture {mode}"),
            transport: "stdio".to_string(),
            command: Some(node.to_string_lossy().to_string()),
            args: Some(vec![
                fixture_path().to_string_lossy().to_string(),
                mode.to_string(),
            ]),
            url: None,
            http_auth_headers: Vec::new(),
            oauth: None,
            enabled,
            allowed_tools: vec!["echo".to_string(), "explode".to_string()],
            trust_level: "unreviewed".to_string(),
        }
    }

    /// 一个已配置好 fixture 的数据库与 supervisor。`None` = 这台机器没有 node。
    fn fixture_setup(
        name: &str,
        mode: &str,
        server_id: &str,
    ) -> Option<(PathBuf, Database, McpSupervisor)> {
        node_or_skip()?;
        assert!(
            fixture_path().is_file(),
            "the fixture must be committed, not generated: {}",
            fixture_path().display()
        );
        let (path, database) = temp_db(name);
        insert(&database, &fixture_row(server_id, mode, true));
        Some((path, database, McpSupervisor::new()))
    }

    /* ── 码表与名字 ───────────────────────────────────────────────────────── */

    /// 每个码都在 `ALL` 里 —— `ALL` 是交给 TypeScript 那一侧的那张表。
    #[test]
    fn every_failure_code_is_in_the_published_list() {
        let errors = [
            McpError::UnsupportedTransport {
                server_id: "s".into(),
                transport: "sse".into(),
            },
            McpError::Disabled {
                server_id: "s".into(),
            },
            McpError::MissingCommand {
                server_id: "s".into(),
            },
            McpError::InvalidConfig {
                server_id: "s".into(),
                reason: "r".into(),
            },
            McpError::InvalidToolName {
                server_id: "s".into(),
                name: "docker__get".into(),
                reason: "r".into(),
            },
            McpError::ToolNameCollision {
                server_id: "s".into(),
                name: "x".into(),
            },
            McpError::UnsupportedProtocolVersion {
                server_id: "s".into(),
                version: "1999-01-01".into(),
                supported: "2025-11-25".into(),
            },
            McpError::Protocol {
                server_id: "s".into(),
                reason: "r".into(),
            },
            McpError::Launch {
                server_id: "s".into(),
                program: "npx".into(),
                reason: "r".into(),
            },
            McpError::NotRunning {
                server_id: "s".into(),
            },
            McpError::Timeout {
                server_id: "s".into(),
                method: "tools/call".into(),
                timeout: Duration::from_secs(1),
            },
            McpError::Remote {
                server_id: "s".into(),
                code: -32603,
                message: "m".into(),
            },
            McpError::Exited {
                server_id: "s".into(),
                code: Some(7),
                signal: None,
                reason: "exit code 7".into(),
            },
            McpError::Write {
                server_id: "s".into(),
                reason: "r".into(),
            },
            McpError::UnknownTool {
                server_id: "s".into(),
                tool: "t".into(),
            },
            McpError::InvalidArguments {
                server_id: "s".into(),
                tool: "t".into(),
            },
        ];
        for error in &errors {
            let code = McpFailureCode::of(error);
            assert!(
                McpFailureCode::ALL.contains(&code),
                "{error} mapped to a code outside MCP_CATALOG_FAILURE_CODES"
            );
        }
        // 序列化出来的词形就是共享契约里的那些字符串。
        let words: Vec<String> = McpFailureCode::ALL
            .iter()
            .map(|code| serde_json::to_value(code).expect("serializable"))
            .map(|value| value.as_str().expect("a string").to_string())
            .collect();
        assert_eq!(
            words,
            vec![
                "disabled",
                "invalid_config",
                "launch_failed",
                "exited",
                "timeout",
                "request_failed",
            ]
        );
    }

    #[test]
    fn only_mcp_dot_names_are_routed_and_only_three_segments_parse() {
        assert!(is_mcp_tool_name("mcp.mcp-1.echo"));
        assert!(!is_mcp_tool_name("mcp"));
        assert!(!is_mcp_tool_name("mcp-1.echo"));
        assert!(!is_mcp_tool_name("docker.ps"));
        assert!(!is_mcp_tool_name("mcpx.y"));

        assert_eq!(
            split_mcp_tool_name("mcp.mcp-1.echo"),
            Some(("mcp-1", "echo"))
        );
        // 四段是伪造的：远端工具的拼写里不可能有点。
        assert_eq!(split_mcp_tool_name("mcp.a.b.c"), None);
        // 段规则与 ADR 0004 同一条（大写、下划线、空格、`__` 都不是段）。
        assert_eq!(split_mcp_tool_name("mcp.Mcp.echo"), None);
        assert_eq!(split_mcp_tool_name("mcp.mcp__1.echo"), None);
        assert_eq!(split_mcp_tool_name("mcp.mcp-1.echo.read"), None);
        assert_eq!(split_mcp_tool_name("mcp..echo"), None);
        assert_eq!(split_mcp_tool_name("mcp.mcp-1."), None);
    }

    /* ── 已经存在的 http 行 ───────────────────────────────────────────────── */

    #[tokio::test]
    async fn a_disabled_row_never_becomes_a_candidate() {
        let (path, db) = temp_db("disabled");
        insert(&db, &fixture_row("mcp_1", "ok", false));

        let supervisor = McpSupervisor::new();
        let response = catalog(&db, &supervisor).await.expect("catalog");
        assert!(response.servers.is_empty());
        assert!(response.failures.is_empty(), "禁用不是失败，只是不参与");
        assert!(
            supervisor.servers().await.is_empty(),
            "a disabled row must not become a process"
        );
        cleanup(&path);
    }

    /// 段撞车：两个 id 规范化到同一个名字段时，第二个不进目录（按 id 排序，`mcp-1` 胜出），
    /// 理由是「`mcp.mcp-1.<tool>` 说不清打给谁」。
    #[tokio::test]
    async fn two_ids_that_normalize_to_one_segment_cannot_both_be_addressed() {
        let (path, db) = temp_db("segment-collision");
        insert(&db, &fixture_row("mcp-1", "ok", true));
        insert(&db, &fixture_row("mcp_1", "ok", true));

        let supervisor = McpSupervisor::new();
        let response = catalog(&db, &supervisor).await.expect("catalog");

        assert_eq!(
            response.servers.len(),
            1,
            "only one of the two may be addressed: {:?}",
            response.servers
        );
        assert_eq!(response.servers[0].server_id, "mcp-1");
        let collision = response
            .failures
            .iter()
            .find(|failure| failure.message.contains("same internal name segment"))
            .expect("the refused row must say why");
        assert_eq!(collision.server_id, "mcp_1");
        assert_eq!(collision.code, McpFailureCode::InvalidConfig);
        supervisor.shutdown_all().await;
        cleanup(&path);
    }

    #[test]
    fn catalog_resolves_http_auth_at_the_start_boundary() {
        struct Resolver;

        impl McpCredentialResolver for Resolver {
            fn resolve(&self, reference: &str) -> Result<String, String> {
                match reference {
                    "keychain://mcp/test-1" => Ok("Bearer first-secret".to_string()),
                    "keychain://mcp/test-2" => Ok("gateway-secret".to_string()),
                    other => panic!("unexpected credential reference {other}"),
                }
            }
        }

        let mut row = fixture_row("mcp_http", "ok", true);
        row.transport = "http".to_string();
        row.command = None;
        row.args = None;
        row.url = Some("https://example.invalid/mcp".to_string());
        row.http_auth_headers = vec![
            yukinal_database::models::McpHttpAuthHeaderConfig {
                name: "Authorization".to_string(),
                credential_ref: "keychain://mcp/test-1".to_string(),
            },
            yukinal_database::models::McpHttpAuthHeaderConfig {
                name: "X-Gateway-Key".to_string(),
                credential_ref: "keychain://mcp/test-2".to_string(),
            },
        ];

        let mut config =
            McpTransportConfig::from_server_config(&row, DEFAULT_REQUEST_TIMEOUT).expect("HTTP");
        attach_http_auth(&row, &mut config, &Resolver).expect("resolved credential");
        let McpTransportConfig::Http(http) = config else {
            panic!("the dispatcher must retain the HTTP transport");
        };
        assert_eq!(http.auth_headers.len(), 2);
        assert_eq!(http.auth_headers[0].name(), "Authorization");
        assert_eq!(http.auth_headers[0].value(), "Bearer first-secret");
        assert_eq!(http.auth_headers[1].name(), "X-Gateway-Key");
        assert_eq!(http.auth_headers[1].value(), "gateway-secret");
        assert!(!format!("{http:?}").contains("gateway-secret"));
    }

    #[test]
    fn catalog_reports_a_missing_http_credential_without_starting() {
        struct Resolver;

        impl McpCredentialResolver for Resolver {
            fn resolve(&self, _reference: &str) -> Result<String, String> {
                Err("not found".to_string())
            }
        }

        let mut row = fixture_row("mcp_http", "ok", true);
        row.transport = "http".to_string();
        row.command = None;
        row.args = None;
        row.url = Some("https://example.invalid/mcp".to_string());
        row.http_auth_headers = vec![yukinal_database::models::McpHttpAuthHeaderConfig {
            name: "Authorization".to_string(),
            credential_ref: "keychain://mcp/missing".to_string(),
        }];

        let mut config =
            McpTransportConfig::from_server_config(&row, DEFAULT_REQUEST_TIMEOUT).expect("HTTP");
        let error = attach_http_auth(&row, &mut config, &Resolver)
            .expect_err("an unresolved credential must fail closed");
        assert!(matches!(error, McpError::InvalidConfig { .. }), "{error:?}");
        assert!(error.to_string().contains("not found"));
    }

    #[test]
    fn catalog_builds_oauth_sources_at_the_start_boundary() {
        struct Source;

        impl McpOAuthTokenSource for Source {
            fn authorization<'a>(
                &'a self,
                _request: crate::mcp::McpAuthorizationRequest,
            ) -> McpOAuthFuture<'a, crate::mcp::McpAuthorization> {
                Box::pin(async {
                    Ok(crate::mcp::McpAuthorization {
                        scheme: crate::mcp::McpAuthScheme::Bearer,
                        token: "oauth-token".to_string(),
                        proof: None,
                    })
                })
            }
        }

        struct Resolver;

        impl McpCredentialResolver for Resolver {
            fn resolve(&self, _reference: &str) -> Result<String, String> {
                Err("not used".to_string())
            }

            fn oauth_source(
                &self,
                config: &McpOAuthSourceConfig,
            ) -> Result<Arc<dyn McpOAuthTokenSource>, String> {
                assert_eq!(config.server_id, "mcp_oauth");
                assert_eq!(config.resource, "https://example.invalid/mcp");
                assert_eq!(config.token_endpoint, "https://auth.example.invalid/token");
                assert_eq!(config.client_id, "desktop-client");
                assert_eq!(config.scopes, vec!["mcp.read"]);
                assert_eq!(config.credential_ref, "keychain://mcp/oauth");
                Ok(Arc::new(Source))
            }
        }

        let mut row = fixture_row("mcp_oauth", "ok", true);
        row.transport = "http".to_string();
        row.command = None;
        row.args = None;
        row.url = Some("https://example.invalid/mcp".to_string());
        row.oauth = Some(yukinal_database::models::McpOAuthConfig {
            issuer: "https://auth.example.invalid".to_string(),
            client_id: "desktop-client".to_string(),
            flow: yukinal_database::models::McpOAuthFlow::AuthorizationCode,
            client_auth: yukinal_database::models::McpOAuthClientAuth::None,
            client_secret_ref: None,
            dpop: false,
            dpop_key_ref: None,
            scopes: vec!["mcp.read".to_string()],
            token_endpoint: Some("https://auth.example.invalid/token".to_string()),
            credential_ref: Some("keychain://mcp/oauth".to_string()),
        });

        let mut config =
            McpTransportConfig::from_server_config(&row, DEFAULT_REQUEST_TIMEOUT).expect("HTTP");
        attach_http_auth(&row, &mut config, &Resolver).expect("OAuth source");
        let McpTransportConfig::Http(http) = config else {
            panic!("the dispatcher must retain the HTTP transport");
        };
        assert!(http.oauth.is_some());
    }

    /* ── 真进程：目录会启动一个从未启动过的服务器 ─────────────────────────── */

    /// 目录会启动一个**从未启动过**的服务器，并把 `McpToolDescriptor` 变成
    /// `mcp.<segment>.<tool>`（ADR 0004）。
    #[tokio::test]
    async fn the_catalog_starts_a_never_started_server_and_names_its_tools() {
        let Some((path, db, supervisor)) = fixture_setup("catalog", "ok", "mcp_1") else {
            return;
        };

        let response = catalog(&db, &supervisor).await.expect("catalog");
        assert!(
            response.failures.is_empty(),
            "nothing should have failed: {:?}",
            response.failures
        );
        assert_eq!(response.servers.len(), 1);

        let server = &response.servers[0];
        assert_eq!(server.server_id, "mcp_1", "身份是数据库里的 id");
        assert_eq!(server.segment, "mcp-1", "内部名里的那一段是规范化后的");
        let names: Vec<&str> = server.tools.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(names, vec!["mcp.mcp-1.echo", "mcp.mcp-1.explode"]);
        assert_eq!(server.tools[0].tool, "echo");
        assert_eq!(server.tools[0].server_id, "mcp_1");
        assert_eq!(
            server.tools[0].input_schema["properties"]["text"]["type"],
            json!("string"),
            "远端声明的入参形状要原样带过去"
        );

        // 第二次要目录：已经跑着，不派第二个进程，结果一样。
        let again = catalog(&db, &supervisor).await.expect("catalog");
        assert_eq!(again.servers, response.servers);

        supervisor.shutdown("mcp_1").await;
        cleanup(&path);
    }

    #[tokio::test]
    async fn only_explicitly_reviewed_tools_enter_the_agent_catalog() {
        let Some((path, db, supervisor)) = fixture_setup("allow-list", "ok", "mcp_1") else {
            return;
        };
        let mut row = db.mcp_servers().get("mcp_1").expect("fixture row");
        row.allowed_tools = vec!["echo".to_string()];
        row.trust_level = "reviewed".to_string();
        insert(&db, &row);

        let response = catalog(&db, &supervisor).await.expect("catalog");
        assert_eq!(response.servers.len(), 1);
        assert_eq!(
            response.servers[0]
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            vec!["mcp.mcp-1.echo"],
            "the unreviewed explode tool must not be exposed to the model"
        );
        supervisor.shutdown("mcp_1").await;
        cleanup(&path);
    }

    /// 目录的预算：一个什么都不答、最后自己退 9 的服务进程不能让目录请求挂住（它是 sidecar
    /// 握手期间唯一会问的东西）。
    #[tokio::test]
    async fn the_catalog_reports_a_server_that_dies_instead_of_hanging() {
        let Some(node) = node_or_skip() else {
            return;
        };
        let (path, db) = temp_db("budget");
        insert(
            &db,
            &McpServerConfig {
                id: "mcp_silent".to_string(),
                label: "silent".to_string(),
                transport: "stdio".to_string(),
                command: Some(node.to_string_lossy().to_string()),
                args: Some(vec![
                    fixture_path().to_string_lossy().to_string(),
                    "silent-exit".to_string(),
                ]),
                url: None,
                http_auth_headers: Vec::new(),
                oauth: None,
                enabled: true,
                allowed_tools: Vec::new(),
                trust_level: "unreviewed".to_string(),
            },
        );

        let supervisor = McpSupervisor::new();
        let started = Instant::now();
        let response = catalog(&db, &supervisor).await.expect("catalog");
        assert!(
            started.elapsed() < CATALOG_START_BUDGET + Duration::from_secs(5),
            "the budget must bound the request: {:?}",
            started.elapsed()
        );
        assert!(response.servers.is_empty());
        assert_eq!(response.failures.len(), 1);
        assert_eq!(
            response.failures[0].code,
            McpFailureCode::Exited,
            "silent-exit dies with code 9 rather than hanging: {:?}",
            response.failures[0]
        );
        assert!(response.failures[0].message.contains("exit code 9"));
        cleanup(&path);
    }
}
