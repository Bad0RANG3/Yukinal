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

use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use yukinal_database::models::McpServerConfig;
use yukinal_database::Database;

use super::config::{McpStdioConfig, DEFAULT_REQUEST_TIMEOUT};
use super::descriptor::{
    internal_tool_name, is_segment, McpExitRecord, McpToolDescriptor, MCP_NAMESPACE,
};
use super::error::McpError;
use super::supervisor::McpSupervisor;

/// 目录请求最多为「启动一个从未启动过的服务器」等多久（总量，不是每个服务器）。
///
/// 目录是 sidecar 在**握手期间**唯一会问的东西，而宿主给 sidecar 的握手预算是 10 秒
/// （`crates/core/src/sidecar/config.rs`，`YUKINAL_AGENT_TIMEOUT_SECS`）。冷启动的
/// `npx -y …` 可以慢到几十秒，所以这里必须有上限：超过预算的服务器这一次不出现，
/// 报告成 [`McpFailureCode::Timeout`]，用户可以显式启动它再重启 Agent。
pub const CATALOG_START_BUDGET: Duration = Duration::from_secs(4);

/// 一个 MCP 服务器「为什么不能用」。码是给分支用的，消息是给人看的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpFailureCode {
    /// `http` 传输：类型层面就不存在（docs/boundaries/mcp.md 的「进程生命周期仍然归 Rust」）。不仅仅是「没实现」，而是目前没有
    /// 任何出站网络策略可以让它成立。
    TransportNotImplemented,
    Disabled,
    InvalidConfig,
    LaunchFailed,
    /// 进程死了。**不会被自动重启**。
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
    pub const ALL: [Self; 7] = [
        Self::TransportNotImplemented,
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
            McpError::TransportNotImplemented { .. } | McpError::UnsupportedTransport { .. } => {
                Self::TransportNotImplemented
            }
            McpError::Disabled { .. } => Self::Disabled,
            McpError::MissingCommand { .. }
            | McpError::InvalidConfig { .. }
            | McpError::InvalidToolName { .. }
            | McpError::ToolNameCollision { .. }
            | McpError::UnsupportedProtocolVersion { .. }
            | McpError::UnknownTool { .. }
            | McpError::InvalidArguments { .. }
            | McpError::Protocol { .. } => Self::InvalidConfig,
            McpError::Launch { .. } => Self::LaunchFailed,
            McpError::Exited { .. } | McpError::NotRunning { .. } => Self::Exited,
            McpError::Timeout { .. } => Self::Timeout,
            McpError::Remote { .. } | McpError::Write { .. } => Self::RequestFailed,
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
    let rows = database
        .mcp_servers()
        .list()
        .map_err(|error| format!("读取 MCP 服务器列表失败：{error}"))?;

    let mut failures = Vec::new();
    let mut candidates: Vec<(McpServerConfig, McpStdioConfig)> = Vec::new();
    for row in rows.into_iter().filter(|row| row.enabled) {
        match McpStdioConfig::from_server_config(&row, DEFAULT_REQUEST_TIMEOUT) {
            Ok(config) => candidates.push((row, config)),
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
            .find(|(segment, _)| segment == &config.segment)
        {
            failures.push(McpCatalogFailure {
                server_id: row.id.clone(),
                code: McpFailureCode::InvalidConfig,
                message: format!(
                    "mcp server \"{}\" normalizes to the same internal name segment \"{}\" as \
                     \"{first}\"; only one of them can be addressed as \
                     \"{MCP_NAMESPACE}.{}.<tool>\" (ADR 0004). Rename one of them.",
                    row.id, config.segment, config.segment
                ),
            });
            return false;
        }
        seen_segments.push((config.segment.clone(), row.id.clone()));
        true
    });

    let deadline = Instant::now() + CATALOG_START_BUDGET;
    let mut servers = Vec::with_capacity(candidates.len());
    for (row, config) in candidates {
        match supervisor.handle(&row.id).await {
            Some(handle) if handle.is_running() => {
                servers.push(catalog_server(&row, &config, handle.tools(), &mut failures));
            }
            // 管过但没在跑：崩了。**不重启**，把退出记录交出去（ADR 0014）。
            Some(handle) => failures.push(McpCatalogFailure {
                server_id: row.id.clone(),
                code: McpFailureCode::Exited,
                message: describe_dead(&row.id, handle.last_exit()),
            }),
            None => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    failures.push(budget_exhausted(&row.id));
                    continue;
                }
                match tokio::time::timeout(remaining, supervisor.start(&config)).await {
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
    config: &McpStdioConfig,
    descriptors: Vec<McpToolDescriptor>,
    failures: &mut Vec<McpCatalogFailure>,
) -> McpCatalogServer {
    let mut tools = Vec::with_capacity(descriptors.len());
    for descriptor in descriptors {
        match internal_tool_name(&config.segment, &descriptor.name) {
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
        segment: config.segment.clone(),
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
pub fn describe_dead(server_id: &str, exit: Option<McpExitRecord>) -> String {
    match exit {
        Some(record) => format!(
            "mcp server \"{server_id}\" exited ({}) at {} and is deliberately not restarted: a \
             restart can re-execute side effects. Start it explicitly if you want it back.",
            record.reason(),
            record.at
        ),
        None => format!(
            "mcp server \"{server_id}\" is not running and will not be restarted automatically; \
             start it explicitly if you want it back"
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use super::*;

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
        let node = node_path().expect("the caller asked for the node path first");
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
            enabled,
            allowed_tools: Vec::new(),
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
            McpError::TransportNotImplemented {
                server_id: "s".into(),
                transport: "http".into(),
            },
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
                "transport_not_implemented",
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
