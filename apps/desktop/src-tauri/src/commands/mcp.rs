//! MCP 服务器：配置、启停、状态，以及把宿主的 MCP 工具接进 `host.tool.execute`。
//!
//! 这个文件是 MCP 的**接线层**，不是 MCP 客户端：进程、握手、`tools/list`、`tools/call`、
//! 超时、退出记录、stderr 尾部全部在 `yukinal_core::mcp`（ADR 0014）。这里只做三件只有这里
//! 才做得了的事：
//!
//! 1. **配置**（`mcp_server_*` 命令）：`mcp_servers` 表的读写，以及「这一行能不能起」的
//!    判断 —— 判断本身不在这里重写，而是问 [`McpStdioConfig::from_server_config`]，因为
//!    「`http` 被拒绝」这件事只允许有一个来源（MCP README §7）。
//! 2. **目录**（[`catalog`]，走 `host.mcp.catalog`）：把宿主正在跑的服务器与它们的工具
//!    描述符交给 sidecar。这是 sidecar 唯一能知道 MCP 存在的地方，所以它也是
//!    `capabilities.mcp` 的唯一依据。
//! 3. **执行**（[`execute`]）：`host.tool.execute` 里 `mcp.` 前缀的那些名字走这里。
//!
//! # 三条不会让步的规则
//!
//! - **崩了不自愈。** 目录请求会启动一个**从未启动过**的服务器（第一次用它就是启动它的
//!   时机），但绝不会把一个已经死掉的拉起来：`McpSupervisor` 里留着退出记录的句柄只被
//!   报告，不被重启。重启是用户按下的动作（`mcp_server_start`），因为重启可能把上一次的
//!   副作用再执行一遍（ADR 0014，`crates/core/src/mcp/mod.rs` 的生命周期契约）。
//! - **`http` 不是静默无事发生。** 保存一个 `http` 行会被拒绝（同一个 `McpError`），已经
//!   存在于表里的 `http` 行在列表、目录与启动路径上都会得到
//!   [`McpFailureCode::TransportNotImplemented`] 与那句完整理由，而不是一个「没反应」的按钮。
//! - **审计要能分辨来源。** 一次 MCP 调用的工具名是 `mcp.<server>.<tool>`（ADR 0004），
//!   服务器 id 作为 `McpCatalogServer::server_id` 交给 sidecar，最终变成工具声明上的
//!   `origin: { kind: "mcp", serverId }`（`apps/agent/src/tools/registry.ts`）。

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::State;
use tokio_util::sync::CancellationToken;
use yukinal_core::mcp::{
    internal_tool_name, is_segment, McpContentBlock, McpError, McpExitRecord, McpServerStatus,
    McpStdioConfig, McpSupervisor, McpToolDescriptor, DEFAULT_REQUEST_TIMEOUT, MCP_NAMESPACE,
};
use yukinal_database::models::McpServerConfig;
use yukinal_database::{Database, DatabaseError};

use crate::commands::host::{cancelled_failure, failed, success};
use crate::state::AppState;

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
    /// `http` 传输：类型层面就不存在（MCP README §7）。不仅仅是「没实现」，而是目前没有
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
    fn of(error: &McpError) -> Self {
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

// ---------------------------------------------------------------------------
// 界面的读数

/// 「这一行现在为什么不能用」。缺席表示「没有已知的阻碍」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerUnavailable {
    pub code: McpFailureCode,
    /// 原样来自 `yukinal_core::mcp` 的错误文本：里面写着下一步该做什么。
    pub message: String,
}

/// 界面上的一行：存下来的配置 + supervisor 的状态 + 已经拿到的工具表。
///
/// 没有 `PartialEq`：`McpServerConfig`（`crates/database/src/models.rs`）没有，而为了让测试
/// 能 `assert_eq!` 就去要求数据库模型实现它，是让测试的形状决定上了线的东西。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerView {
    pub config: McpServerConfig,
    pub status: McpServerStatus,
    /// 运行中的服务器缓存的工具描述符；没跑起来时是空表。
    pub tools: Vec<McpToolDescriptor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<McpServerUnavailable>,
}

#[derive(Debug, Serialize)]
pub struct McpServerListResponse {
    pub servers: Vec<McpServerView>,
}

/// `mcp_server_save` 的入参。
///
/// `allowedTools` 与 `trustLevel` **不在**这里：目前没有任何界面写它们，而让一次「改个标签」
/// 的保存顺手清空它们，就是把两个存着的字段变成两个会被悄悄丢掉的字段。保存时保留原值
/// （见 [`save`]）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpServerSaveInput {
    pub id: String,
    pub label: String,
    /// `"stdio"` 或 `"http"`。`http` 会被拒绝：出站网络策略还不存在。
    pub transport: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct McpServerDeleteResponse {
    pub deleted: bool,
    /// 这一行当时有个正在跑的进程、并且它已经被关掉了。
    pub stopped: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStopResponse {
    pub server: McpServerView,
    /// `None` 表示这个 supervisor 从没管过这个 id（本来就没有进程可关）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shutdown: Option<ShutdownOutcome>,
}

/// [`yukinal_core::mcp::ShutdownReport`] 的线上形状（字段已经是它自己的 camelCase）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShutdownOutcome {
    pub was_running: bool,
    /// True 表示「关 stdin，等它自己走」没成功，最后动用了强杀。
    pub killed: bool,
    /// True 表示强杀之后也没能在预算内确认它消失。
    pub unreaped: bool,
}

// ---------------------------------------------------------------------------
// sidecar 的目录（host.mcp.catalog）

/// 目录里的一个工具。字段来自 `McpToolDescriptor`，不是另造一套形状：`description` 与
/// `input_schema` 是远端声明**原样**带出来的不可信内容（README §6）。
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

// ---------------------------------------------------------------------------
// 命令

/// `mcp_server_list`：**只读**。它不启动任何进程 —— 一个「看一眼有哪些服务器」的动作不该
/// 派生出第三方进程来。
#[tauri::command]
pub async fn mcp_server_list(state: State<'_, AppState>) -> Result<McpServerListResponse, String> {
    let rows = state
        .database
        .mcp_servers()
        .list()
        .map_err(|error| format!("读取 MCP 服务器列表失败：{error}"))?;
    let mut servers = Vec::with_capacity(rows.len());
    for row in rows {
        servers.push(view(&state.mcp, row).await);
    }
    Ok(McpServerListResponse { servers })
}

/// `mcp_server_save`：新增或覆盖一行。
///
/// 保存**不启动**服务器：那是另一个动作、另一次点击（[`mcp_server_start`]）。把两者合成一次
/// 调用，会让「我不想动它，只是改个标签」变成一次派生。
#[tauri::command]
pub async fn mcp_server_save(
    state: State<'_, AppState>,
    input: McpServerSaveInput,
) -> Result<McpServerView, String> {
    let saved = save(&state.database, input)?;
    Ok(view(&state.mcp, saved).await)
}

/// `mcp_server_delete`：先关进程，再删行。
///
/// 顺序不能反：删了行再关进程的话，一个进程会活得比它的配置更久，而「谁在跑」这个问题就再也
/// 答不上来了。
#[tauri::command]
pub async fn mcp_server_delete(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<McpServerDeleteResponse, String> {
    let stopped = match state.mcp.shutdown(&server_id).await {
        Some(report) => report.was_running,
        None => false,
    };
    state
        .database
        .mcp_servers()
        .delete(&server_id)
        .map_err(|error| describe_delete_failure(&server_id, &error))?;
    Ok(McpServerDeleteResponse {
        deleted: true,
        stopped,
    })
}

/// `mcp_server_start`：显式启动（也是崩溃之后唯一的重启路径）。
#[tauri::command]
pub async fn mcp_server_start(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<McpServerView, String> {
    let row = load(&state.database, &server_id)?;
    match McpStdioConfig::from_server_config(&row, DEFAULT_REQUEST_TIMEOUT) {
        // 起不来（http、缺 command、配置非法）时**不报错**，而是把理由放进视图里：界面只有
        // 一个地方显示「为什么它没在跑」，`Err` 留给「这个请求本身做不到」。
        Err(error) => {
            let message = error.to_string();
            Ok(view_with_unavailable(&state.mcp, row, &error, message).await)
        }
        Ok(config) => match state.mcp.start(&config).await {
            Ok(_) => Ok(view(&state.mcp, row).await),
            Err(error) => {
                let message = error.to_string();
                Ok(view_with_unavailable(&state.mcp, row, &error, message).await)
            }
        },
    }
}

/// `mcp_server_stop`：关掉一个服务器。没跑着也算成功（`shutdown` 如实说明本来就没在跑）。
#[tauri::command]
pub async fn mcp_server_stop(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<McpServerStopResponse, String> {
    let row = load(&state.database, &server_id)?;
    let shutdown = state
        .mcp
        .shutdown(&server_id)
        .await
        .map(|report| ShutdownOutcome {
            was_running: report.was_running,
            killed: report.killed,
            unreaped: report.unreaped,
        });
    Ok(McpServerStopResponse {
        server: view(&state.mcp, row).await,
        shutdown,
    })
}

// ---------------------------------------------------------------------------
// 目录与执行（host.rs 用）

/// 交给 sidecar 的目录。
///
/// **它会启动从未启动过的服务器**，因为 MCP 没有静态工具表：要回答「这个服务器有哪些工具」，
/// 唯一的办法是把进程起起来问它。这是这个文件里唯一一处「读操作有副作用」，所以它有三条明确
/// 的边界：
///
/// - 只碰 `enabled` 且 `transport == "stdio"` 的行；
/// - 只启动 supervisor **从未管过** 的 id。已经死掉的句柄带着退出记录留在表里，只被报告，
///   **不被重启**（重启可能把上一次的副作用再执行一遍）；
/// - 总等待时间不超过 [`CATALOG_START_BUDGET`]。
pub(crate) async fn catalog(
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
pub(crate) fn is_mcp_tool_name(name: &str) -> bool {
    name.strip_prefix(MCP_NAMESPACE)
        .is_some_and(|rest| rest.starts_with('.'))
}

/// `mcp.<server-segment>.<tool-segment>` → `(server, tool)`。
///
/// **恰好三段。** 少一段不是 MCP 名字；多一段是伪造的 —— 工具的拼写在导入时就已经被规范化过
/// （段里不可能有点，`crates/core/src/mcp/descriptor.rs`），所以 `mcp.a.b.c` 只能是调用方自己
/// 拼出来的。
fn split_mcp_tool_name(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix(MCP_NAMESPACE)?.strip_prefix('.')?;
    let (server, tool) = rest.split_once('.')?;
    if !is_segment(server) || !is_segment(tool) {
        return None;
    }
    Some((server, tool))
}

/// 执行一次 MCP 工具调用，返回 `host.tool.execute` 的响应体。
///
/// 结果永远以 `Ok` 返回：调用失败是**工具调用的结果**，不是宿主协议的失败。`Err` 只留给
/// 「宿主自己写不出一个合法响应」。
pub(crate) async fn execute(
    supervisor: &McpSupervisor,
    tool_name: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let Some((server_segment, tool)) = split_mcp_tool_name(tool_name) else {
        return Ok(failed(
            "not_found",
            format!(
                "`{tool_name}` is not an MCP tool name; the shape is \
                 `{MCP_NAMESPACE}.<server>.<tool>` (ADR 0004)"
            ),
            false,
            None,
        ));
    };

    let server_id = match server_for_segment(supervisor, server_segment).await {
        Ok(server_id) => server_id,
        Err(message) => return Ok(failed("not_found", message, false, None)),
    };

    let call = supervisor.call(&server_id, tool, input.clone());
    tokio::select! {
        // 取消**停在这里**：宿主不再等这个响应，但服务进程那边的调用可能还在跑 —— MCP 的
        // 线上协议里没有「取消一次 tools/call」这个方法（`crates/core/src/mcp/wire.rs` 只有
        // 三个方法），所以这一点如实写在 ADR 0014 里，而不是假装副作用已经被撤回。
        () = cancel.cancelled() => Ok(cancelled_failure()),
        result = call => match result {
            Ok(result) => Ok(success(json!(McpToolCallOutput {
                server_id,
                tool: tool.to_string(),
                is_error: result.is_error,
                text: result.text(),
                content: result.content,
                structured_content: result.structured_content,
            }))),
            Err(error) => Ok(mcp_tool_failure(&server_id, tool, error, cancel)),
        },
    }
}

/// `tools/call` 的响应体。
///
/// `isError` 一起带出去，而不是在这里翻译成宿主失败：MCP 的规则是「工具自己报了错」与「这次
/// 调用没能发生」是两件事（`McpToolResult::is_error` vs `McpError::Remote`），而把它翻译成哪
/// 一类是**适配器**的决定（`apps/agent/src/mcp/tool.ts`）。宿主只搬运，不解释。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct McpToolCallOutput {
    server_id: String,
    tool: String,
    is_error: bool,
    /// `content` 里文本块的拼接（不可信内容，走的是 `McpToolResult::text()`）。
    text: String,
    content: Vec<McpContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    structured_content: Option<Value>,
}

fn mcp_tool_failure(
    server_id: &str,
    tool: &str,
    error: McpError,
    cancel: &CancellationToken,
) -> Value {
    if cancel.is_cancelled() {
        return cancelled_failure();
    }
    // 一个第三方进程的失败对调用方只有两种：重试可能有用（超时），或者没用（其余）。
    // `Exited` **不是**可重试的：那个进程不会自己回来（ADR 0014）。
    let retryable = matches!(error, McpError::Timeout { .. });
    let code = match &error {
        McpError::Timeout { .. } => "timeout",
        McpError::UnknownTool { .. } => "not_found",
        McpError::InvalidArguments { .. } => "invalid_input",
        McpError::TransportNotImplemented { .. } | McpError::UnsupportedTransport { .. } => {
            "denied_by_policy"
        }
        McpError::Remote { .. } => "execution_failed",
        _ => "transport",
    };
    let detail = match &error {
        McpError::Exited {
            code,
            signal,
            reason,
            ..
        } => json!({
            "serverId": server_id,
            "tool": tool,
            "exitCode": code,
            "exitSignal": signal,
            "exitReason": reason,
            "restarted": false,
        }),
        _ => json!({ "serverId": server_id, "tool": tool }),
    };
    failed(code, error.to_string(), retryable, Some(detail))
}

/// 一个名字段对应哪个 serverId。
///
/// 唯一性由目录保证（段撞车的行不进目录），但执行路径不能**假设**它已经保证了：两个句柄共用
/// 一个段时，随便挑一个就是一次说不清打给谁的调用，所以这里报错。
async fn server_for_segment(supervisor: &McpSupervisor, segment: &str) -> Result<String, String> {
    let mut matches = Vec::new();
    for server_id in supervisor.servers().await {
        if let Some(handle) = supervisor.handle(&server_id).await {
            if handle.segment() == segment {
                matches.push(server_id);
            }
        }
    }
    match matches.len() {
        0 => Err(format!(
            "no running MCP server provides the name segment \"{segment}\"; it may have been \
             stopped, or it crashed and is deliberately not restarted (ADR 0014)"
        )),
        1 => Ok(matches.remove(0)),
        _ => Err(format!(
            "the name segment \"{segment}\" belongs to {} servers ({}), so \
             `{MCP_NAMESPACE}.{segment}.…` cannot say which one to call (ADR 0004)",
            matches.len(),
            matches.join(", ")
        )),
    }
}

/// 崩掉的服务器要说的那一句：退出记录 + 「不会自己回来」+ 下一步。
pub(crate) fn describe_dead(server_id: &str, exit: Option<McpExitRecord>) -> String {
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

// ---------------------------------------------------------------------------
// 纯逻辑（可测）

/// 保存一行。`Err(String)` 是**给用户看**的拒绝理由，所以它必须能照原样显示。
pub(crate) fn save(
    database: &Database,
    input: McpServerSaveInput,
) -> Result<McpServerConfig, String> {
    let id = input.id.trim();
    if id.is_empty() {
        return Err(
            "MCP 服务器需要一个 id：它是身份，也是内部工具名 `mcp.<id>.<tool>` 的来源（ADR 0004）。"
                .to_string(),
        );
    }
    let label = input.label.trim();
    if label.is_empty() {
        return Err("MCP 服务器需要一个名字：列表里靠它分辨。".to_string());
    }

    let existing = database.mcp_servers().get(id).ok();
    let config = McpServerConfig {
        id: id.to_string(),
        label: label.to_string(),
        transport: input.transport.trim().to_string(),
        command: input
            .command
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        args: input.args,
        url: input.url.filter(|value| !value.trim().is_empty()),
        enabled: input.enabled,
        // 界面没有这两个字段，所以保存保留原值：一次改标签不该清空它们。
        allowed_tools: existing
            .as_ref()
            .map(|row| row.allowed_tools.clone())
            .unwrap_or_default(),
        trust_level: existing
            .map(|row| row.trust_level)
            .unwrap_or_else(|| "unreviewed".to_string()),
    };

    check_transport(&config)?;

    database
        .mcp_servers()
        .upsert(&config)
        .map_err(|error| format!("保存 MCP 服务器 `{id}` 失败：{error}"))?;
    Ok(config)
}

/// 这一行的**传输方式**能不能用。
///
/// 判断不在这里重写：问 [`McpStdioConfig::from_server_config`]，因为「`http` 被拒绝」这件事
/// 只允许有一个来源（MCP README §7）。它同时会拒绝一个无法变成内部名段的 id —— 那种行永远
/// 不可能有工具，早一点拒绝比留一个永远不工作的条目好。
///
/// 「还没写完」的错误（禁用、没填 command）**允许**保存：草稿是合法的，用户是先写下来再补的。
fn check_transport(config: &McpServerConfig) -> Result<(), String> {
    match McpStdioConfig::from_server_config(config, DEFAULT_REQUEST_TIMEOUT) {
        Ok(_) => Ok(()),
        Err(
            error @ (McpError::TransportNotImplemented { .. }
            | McpError::UnsupportedTransport { .. }
            | McpError::InvalidConfig { .. }),
        ) => Err(format!(
            "{}。当前只支持 stdio 传输（MCP README §7：出站网络策略还不存在）。",
            error
        )),
        Err(_) => Ok(()),
    }
}

/// 一次「为什么它没在跑」的视图。
async fn view_with_unavailable(
    supervisor: &McpSupervisor,
    row: McpServerConfig,
    error: &McpError,
    message: String,
) -> McpServerView {
    let mut view = view(supervisor, row).await;
    view.unavailable = Some(McpServerUnavailable {
        code: McpFailureCode::of(error),
        message,
    });
    view
}

/// 一行 → 界面读数。
///
/// 规则是「跑着就以状态为准，没跑才说为什么」：一个跑着的 `http` 行不存在（它起不来），所以
/// 这两条不会互相矛盾。
async fn view(supervisor: &McpSupervisor, row: McpServerConfig) -> McpServerView {
    let status = supervisor.status(&row.id).await;
    let tools = if status.running {
        supervisor.tools(&row.id).await
    } else {
        Vec::new()
    };
    let unavailable = if status.running {
        None
    } else {
        match McpStdioConfig::from_server_config(&row, DEFAULT_REQUEST_TIMEOUT) {
            Ok(_) => dead_or_never_started(&row.id, &status),
            Err(error) => Some(McpServerUnavailable {
                code: McpFailureCode::of(&error),
                message: error.to_string(),
            }),
        }
    };
    McpServerView {
        config: row,
        status,
        tools,
        unavailable,
    }
}

/// 没在跑的**理由**：崩过（有退出记录）与从没启动过是两件不同的事，界面要分开说。
fn dead_or_never_started(
    server_id: &str,
    status: &McpServerStatus,
) -> Option<McpServerUnavailable> {
    let exit = status.last_exit.clone()?;
    Some(McpServerUnavailable {
        code: McpFailureCode::Exited,
        message: describe_dead(server_id, Some(exit)),
    })
}

fn load(database: &Database, server_id: &str) -> Result<McpServerConfig, String> {
    database.mcp_servers().get(server_id).map_err(|error| {
        if matches!(error, DatabaseError::NotFound) {
            format!("找不到 MCP 服务器 `{server_id}`；列表可能已经过期，刷新后重试。")
        } else {
            format!("读取 MCP 服务器 `{server_id}` 失败：{error}")
        }
    })
}

fn describe_delete_failure(server_id: &str, error: &DatabaseError) -> String {
    if matches!(error, DatabaseError::NotFound) {
        format!(
            "删除 MCP 服务器 `{server_id}` 失败：它已经不在列表里了（可能是另一个窗口删掉了）。"
        )
    } else {
        format!("删除 MCP 服务器 `{server_id}` 失败：{error}")
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
    /// 取不到（`crates/database/src/lib.rs`）。走文件顺带证明这些命令面对的就是磁盘上那份
    /// schema 与那份迁移结果。名字带进程 id 与计数器，所以并行跑的测试互不干扰。
    fn temp_db(name: &str) -> (PathBuf, Database) {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "yukinal-mcp-{}-{}-{}.sqlite",
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

    fn save_input(id: &str, transport: &str, enabled: bool) -> McpServerSaveInput {
        McpServerSaveInput {
            id: id.to_string(),
            label: format!("label for {id}"),
            transport: transport.to_string(),
            command: Some("node".to_string()),
            args: None,
            url: None,
            enabled,
        }
    }

    /// 直接写一行（绕过 `save` 的校验：**已经存在**于表里的 `http` 行就是这一类）。
    fn insert(database: &Database, row: &McpServerConfig) {
        database.mcp_servers().upsert(row).expect("upsert row");
    }

    /// 一行存进去再读出来时比什么。
    ///
    /// 不用 `assert_eq!`：`McpServerConfig`（`crates/database/src/models.rs`）没有 `PartialEq`，
    /// 而为了一个测试去要求数据库模型实现它，是让测试的形状决定上了线的东西。比 JSON 另有好处
    /// —— 它连以后新加的字段一起比，「往返不改写任何东西」这句话不会因为漏了一个字段而变假。
    fn as_json(row: &McpServerConfig) -> Value {
        serde_json::to_value(row).expect("McpServerConfig is serializable")
    }

    fn fixture_path() -> PathBuf {
        // 与 `crates/core/tests/mcp_stdio.rs` 用的是**同一个**已提交的 fixture：cargo 把每个
        // 集成测试编译成独立 crate，所以它没法被共享成模块，但路径可以被共享。
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("..")
            .join("crates")
            .join("core")
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

    /* ── 配置往返 ─────────────────────────────────────────────────────────── */

    #[test]
    fn a_saved_row_reads_back_unchanged_through_get_and_list() {
        let (path, db) = temp_db("round-trip");
        let mut input = save_input("mcp_1", "stdio", true);
        input.args = Some(vec!["/srv/mcp/server.js".to_string(), "--mode".to_string()]);
        let saved = save(&db, input).expect("save");

        assert_eq!(saved.transport, "stdio");
        assert_eq!(
            saved.trust_level, "unreviewed",
            "a new row starts unreviewed; nothing in this repo promotes it"
        );
        assert!(saved.enabled);

        let by_id = db.mcp_servers().get("mcp_1").expect("get");
        assert_eq!(
            as_json(&by_id),
            as_json(&saved),
            "the round trip must not rewrite anything"
        );

        let listed = db.mcp_servers().list().expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(as_json(&listed[0]), as_json(&saved));
        cleanup(&path);
    }

    #[test]
    fn saving_again_preserves_the_fields_the_form_does_not_own() {
        let (path, db) = temp_db("preserve");
        save(&db, save_input("mcp_1", "stdio", true)).expect("first save");

        // 模拟「这两个字段由别的东西写过」（它们目前没有界面）。
        let mut stored = db.mcp_servers().get("mcp_1").expect("get");
        stored.allowed_tools = vec!["echo".to_string()];
        stored.trust_level = "reviewed".to_string();
        insert(&db, &stored);

        let mut renamed = save_input("mcp_1", "stdio", false);
        renamed.label = "renamed".to_string();
        let saved = save(&db, renamed).expect("second save");

        assert_eq!(saved.label, "renamed");
        assert!(!saved.enabled);
        assert_eq!(
            saved.allowed_tools,
            vec!["echo".to_string()],
            "a rename must not silently drop the reviewed tool list"
        );
        assert_eq!(saved.trust_level, "reviewed");
        cleanup(&path);
    }

    #[test]
    fn an_http_row_is_refused_at_save_with_the_core_reason() {
        let (path, db) = temp_db("http-refused");
        let error = save(&db, save_input("mcp_http", "http", true)).expect_err("must refuse");
        assert!(
            error.contains("outbound network policy"),
            "拒绝理由必须来自 core 那句完整解释：{error}"
        );
        assert!(
            db.mcp_servers().list().expect("list").is_empty(),
            "a refused row must not be written"
        );
        cleanup(&path);
    }

    #[test]
    fn an_unknown_transport_and_an_impossible_id_are_refused() {
        let (path, db) = temp_db("transport-refused");
        let error = save(&db, save_input("mcp_sse", "sse", true)).expect_err("must refuse");
        assert!(error.contains("unsupported transport"), "{error}");

        // `mcp_1` 规范化成 `mcp-1`，合法；`mcp..1` 永远变不成名字段。
        let error = save(&db, save_input("mcp..1", "stdio", true)).expect_err("must refuse");
        assert!(error.contains("mcp..1"), "{error}");
        assert!(save(&db, save_input("mcp_1", "stdio", true)).is_ok());
        cleanup(&path);
    }

    #[test]
    fn a_draft_row_is_saveable_but_a_blank_id_or_label_is_not() {
        let (path, db) = temp_db("draft");
        // 草稿：禁用 + 没有 command。可以保存。
        let mut draft = save_input("mcp_draft", "stdio", false);
        draft.command = None;
        assert!(save(&db, draft).is_ok(), "a draft is a legitimate row");

        let mut blank = save_input("", "stdio", true);
        blank.label = "x".into();
        assert!(save(&db, blank).is_err());

        let mut unlabelled = save_input("mcp_1", "stdio", true);
        unlabelled.label = "   ".into();
        assert!(save(&db, unlabelled).is_err());
        cleanup(&path);
    }

    #[tokio::test]
    async fn a_missing_row_is_reported_as_such() {
        let (path, db) = temp_db("missing");
        let error = load(&db, "mcp_missing").expect_err("must report");
        assert!(error.contains("mcp_missing"), "{error}");
        assert!(error.contains("刷新"), "要说下一步：{error}");
        cleanup(&path);
    }

    /* ── 已经存在的 http 行 ────────────────────────────────────────────────── */

    /// 表里已经有一行 `http`（旧版本写的、或者手工改的）时，目录与视图都必须**说出来**，
    /// 而不是静默无事发生（MCP README §7）。
    #[tokio::test]
    async fn an_http_row_in_the_table_refuses_with_the_same_reason() {
        let (path, db) = temp_db("http-row");
        let mut http = fixture_row("mcp_http", "ok", true);
        http.transport = "http".to_string();
        http.command = None;
        http.args = None;
        http.url = Some("https://mcp.example.com/sse".to_string());
        insert(&db, &http);

        let supervisor = McpSupervisor::new();
        let response = catalog(&db, &supervisor)
            .await
            .expect("catalog never fails");

        assert!(
            response.servers.is_empty(),
            "an http server must not appear as a usable server"
        );
        assert_eq!(response.failures.len(), 1, "{:?}", response.failures);
        let failure = &response.failures[0];
        assert_eq!(failure.code, McpFailureCode::TransportNotImplemented);
        assert_eq!(failure.server_id, "mcp_http");
        assert!(
            failure.message.contains("outbound network policy"),
            "拒绝理由必须说清为什么：{}",
            failure.message
        );

        // 视图（列表用的那个函数）说同一件事。
        let view = view(&supervisor, http).await;
        assert!(!view.status.running);
        let unavailable = view.unavailable.expect("a reason must be present");
        assert_eq!(unavailable.code, McpFailureCode::TransportNotImplemented);
        assert!(supervisor.servers().await.is_empty());
        cleanup(&path);
    }

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

    /* ── 真进程：目录 → 执行 → 崩溃 ────────────────────────────────────────── */

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

    /// `tools/call` 的结果被**搬运**，不被解释：工具自己报错时 `isError` 原样带出去，宿主仍然
    /// 回 `success`（翻译成哪一类失败是适配器的事）。
    #[tokio::test]
    async fn an_mcp_tool_name_is_routed_to_its_server_and_errors_are_not_translated_here() {
        let Some((path, db, supervisor)) = fixture_setup("execute", "ok", "mcp_1") else {
            return;
        };
        catalog(&db, &supervisor).await.expect("catalog");
        let cancel = CancellationToken::new();

        let ok = execute(
            &supervisor,
            "mcp.mcp-1.echo",
            &json!({ "text": "hello" }),
            &cancel,
        )
        .await
        .expect("host response");
        assert_eq!(ok["status"], json!("success"));
        assert_eq!(ok["output"]["serverId"], json!("mcp_1"));
        assert_eq!(ok["output"]["tool"], json!("echo"));
        assert_eq!(ok["output"]["isError"], json!(false));
        assert_eq!(ok["output"]["text"], json!("echo: hello"));

        let reported = execute(&supervisor, "mcp.mcp-1.explode", &json!({}), &cancel)
            .await
            .expect("host response");
        assert_eq!(
            reported["status"],
            json!("success"),
            "a tool-level error is a successful call that reports isError"
        );
        assert_eq!(reported["output"]["isError"], json!(true));

        // 名字本身先说清楚是哪一种不合法。
        let malformed = execute(&supervisor, "mcp.mcp-1", &json!({}), &cancel)
            .await
            .expect("host response");
        assert_eq!(malformed["status"], json!("failed"));
        assert_eq!(malformed["error"]["code"], json!("not_found"));

        let unknown_tool = execute(&supervisor, "mcp.mcp-1.nope", &json!({}), &cancel)
            .await
            .expect("host response");
        assert_eq!(unknown_tool["status"], json!("failed"));
        assert_eq!(unknown_tool["error"]["code"], json!("not_found"));

        supervisor.shutdown("mcp_1").await;
        cleanup(&path);
    }

    /// 崩掉的服务器**被报告，不被重启**：这是 ADR 0014 里最不肯让步的那一条，而这里是它唯一
    /// 能被证伪的地方（真的派一个进程，真的让它退 7）。
    #[tokio::test]
    async fn a_crashed_server_is_reported_with_its_exit_code_and_never_restarted() {
        let Some((path, db, supervisor)) = fixture_setup("crash", "misbehave", "mcp_1") else {
            return;
        };
        let cancel = CancellationToken::new();
        let first = catalog(&db, &supervisor).await.expect("catalog");
        assert_eq!(first.servers.len(), 1);
        let killed_pid = supervisor.handle("mcp_1").await.expect("tracked").pid();

        // `quit` 不回任何东西就退 7：这一次调用以「进程没了」结束。
        let crashed = execute(&supervisor, "mcp.mcp-1.quit", &json!({}), &cancel)
            .await
            .expect("host response");
        assert_eq!(crashed["status"], json!("failed"));
        assert_eq!(crashed["error"]["code"], json!("transport"));
        assert_eq!(
            crashed["error"]["retryable"],
            json!(false),
            "一个死掉的进程不会自己回来，所以重试没有意义"
        );
        assert_eq!(crashed["error"]["detail"]["exitCode"], json!(7));
        assert_eq!(crashed["error"]["detail"]["restarted"], json!(false));

        // 再要一次目录：它必须**报告**，而不是把它拉起来。
        let after = catalog(&db, &supervisor).await.expect("catalog");
        assert!(
            after.servers.is_empty(),
            "a crashed server must not come back through the catalog"
        );
        let failure = after
            .failures
            .iter()
            .find(|failure| failure.server_id == "mcp_1")
            .expect("the crash must be reported");
        assert_eq!(failure.code, McpFailureCode::Exited);
        assert!(
            failure.message.contains("exit code 7"),
            "退出码是给用户的那条可行动信息：{}",
            failure.message
        );
        assert!(
            failure.message.contains("not restarted"),
            "必须说清它不会自己回来：{}",
            failure.message
        );
        assert_eq!(
            supervisor
                .handle("mcp_1")
                .await
                .expect("still tracked")
                .pid(),
            killed_pid,
            "no second process may exist"
        );

        // 第三次也一样：没有自动重启，也没有重试预算。
        let third = catalog(&db, &supervisor).await.expect("catalog");
        assert_eq!(third.failures.len(), after.failures.len());

        // 而调用一个崩掉的服务器是一个说得清的失败，不是一次挂住。
        let dead = execute(
            &supervisor,
            "mcp.mcp-1.echo",
            &json!({ "text": "x" }),
            &cancel,
        )
        .await
        .expect("host response");
        assert_eq!(dead["status"], json!("failed"));
        assert_eq!(dead["error"]["code"], json!("transport"));

        cleanup(&path);
    }

    /// 取消：宿主不再等，而且如实说这是取消（不是工具的失败）。
    #[tokio::test]
    async fn a_cancelled_call_stops_waiting_and_says_so() {
        let Some((path, db, supervisor)) = fixture_setup("cancel", "misbehave", "mcp_1") else {
            return;
        };
        catalog(&db, &supervisor).await.expect("catalog");

        let cancel = CancellationToken::new();
        cancel.cancel();
        let cancelled = execute(&supervisor, "mcp.mcp-1.never-answer", &json!({}), &cancel)
            .await
            .expect("host response");
        assert_eq!(cancelled["status"], json!("failed"));
        assert_eq!(cancelled["error"]["code"], json!("cancelled"));

        supervisor.shutdown("mcp_1").await;
        cleanup(&path);
    }

    /// 一个从来没启动过的段（比如用户把它停了）：执行路径给出的是一句能读懂的话，而不是一个
    /// `not_found` 了事。
    #[tokio::test]
    async fn an_unknown_server_segment_explains_itself() {
        let supervisor = McpSupervisor::new();
        let cancel = CancellationToken::new();
        let response = execute(&supervisor, "mcp.mcp-9.echo", &json!({}), &cancel)
            .await
            .expect("host response");
        assert_eq!(response["status"], json!("failed"));
        assert_eq!(response["error"]["code"], json!("not_found"));
        let message = response["error"]["message"].as_str().expect("a message");
        assert!(message.contains("mcp-9"), "{message}");
        assert!(
            message.contains("not restarted"),
            "要提到崩溃不会被自动恢复这件事：{message}"
        );
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

    /// 环境门：没有 node 的机器上，上面那些真进程测试**跳过**；`YUKINAL_TEST_REQUIRED=1`
    /// 时它们失败（一个绿色的 CI 不能意味着「这些测试从来没跑过」）。
    #[test]
    fn the_node_gate_matches_the_one_the_core_integration_tests_use() {
        if node_path().is_none() {
            assert!(node_or_skip().is_none());
        } else {
            assert!(node_or_skip().is_some());
        }
    }
}
