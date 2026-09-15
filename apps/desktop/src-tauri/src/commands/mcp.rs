//! MCP 服务器：配置、启停、状态，以及把宿主的 MCP 工具接进 `host.tool.execute`。
//!
//! 这个文件是 MCP 的**接线层**，不是 MCP 客户端：进程、握手、`tools/list`、`tools/call`、
//! 超时、退出记录、stderr 尾部全部在 `yukinal_core::mcp`（ADR 0014）。这里只做三件只有这里
//! 才做得了的事：
//!
//! 1. **配置**（`mcp_server_*` 命令）：`mcp_servers` 表的读写，以及「这一行能不能起」的
//!    判断 —— 判断本身不在这里重写，而是问 [`McpTransportConfig::from_server_config`]。
//! 2. **目录**（[`catalog`]，走 `host.mcp.catalog`）：把宿主正在跑的服务器与它们的工具
//!    描述符交给 sidecar。这是 sidecar 唯一能知道 MCP 存在的地方，所以它也是
//!    `capabilities.mcp` 的唯一依据。
//! 3. **执行**（[`execute`]）：`host.tool.execute` 里 `mcp.` 前缀的那些名字走这里。
//!
//! # 三条不会让步的规则
//!
//! - **崩溃按预算自愈。** 目录请求可以启动一个从未启动过的服务器；已经崩溃的服务器由
//!   `McpSupervisor` 按有界退避重建进程和工具目录。恢复不重放中断的调用，预算耗尽后
//!   由 `mcp_server_start` 显式重置。
//! - **HTTP 端点先过本地策略。** 远程端点必须使用 HTTPS，明文 HTTP 只允许回环地址，重定向
//!   被关闭；这些规则在建立任何网络连接之前于 `McpHttpConfig` 生效。
//! - **审计要能分辨来源。** 一次 MCP 调用的工具名是 `mcp.<server>.<tool>`（ADR 0004），
//!   服务器 id 作为 `McpCatalogServer::server_id` 交给 sidecar，最终变成工具声明上的
//!   `origin: { kind: "mcp", serverId }`（`apps/agent/src/tools/registry.ts`）。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_opener::OpenerExt;
use tokio_util::sync::CancellationToken;
use yukinal_core::mcp::{
    catalog_with_credentials, McpContentBlock, McpCredentialResolver, McpError, McpFailureCode,
    McpHttpAuthHeader, McpOAuthSourceConfig, McpOAuthTokenSource, McpServerStatus, McpSupervisor,
    McpToolDescriptor, McpTransportConfig, DEFAULT_REQUEST_TIMEOUT, MCP_NAMESPACE,
};
use yukinal_credentials::{CredentialRef, CredentialStore, Secret};
use yukinal_database::models::{
    McpHttpAuthHeaderConfig, McpOAuthClientAuth, McpOAuthConfig, McpOAuthFlow, McpServerConfig,
};
use yukinal_database::{Database, DatabaseError};
use yukinal_net::OutboundProxy;

use crate::commands::host::{cancelled_failure, failed, success};
use crate::commands::server::next_id;
use crate::state::AppState;

mod dpop;
mod oauth;
#[cfg(test)]
pub(crate) use oauth::McpOAuthConnectResult;

/// 目录与名字解析住在 `yukinal_core::mcp`（数据库与 supervisor 那里都有，Tauri 那里没有）。
/// `commands/host.rs` 按 `mcp::…` 的名字调用它们，所以在这里原样转出。
pub(crate) use yukinal_core::mcp::{describe_dead, is_mcp_tool_name, split_mcp_tool_name};

/// The host catalog is the only automatic-start path, so it must resolve credentials
/// at the same boundary as an explicit start.
pub(crate) async fn catalog(
    database: &Database,
    supervisor: &McpSupervisor,
    credentials: Arc<dyn CredentialStore>,
) -> Result<yukinal_core::mcp::McpCatalogResponse, String> {
    struct Resolver<'a> {
        database: &'a Database,
        credentials: Arc<dyn CredentialStore>,
    }

    impl McpCredentialResolver for Resolver<'_> {
        fn resolve(&self, reference: &str) -> Result<String, String> {
            let reference = CredentialRef::parse(reference).map_err(|error| error.to_string())?;
            self.credentials
                .get(&reference)
                .map_err(|error| error.to_string())?
                .as_utf8()
                .map(|secret| secret.into_owned())
                .map_err(|error| error.to_string())
        }

        fn oauth_source(
            &self,
            config: &McpOAuthSourceConfig,
        ) -> Result<Arc<dyn McpOAuthTokenSource>, String> {
            oauth::source_from_config(self.credentials.clone(), config)
        }

        /// 应用级的出站代理（ADR 0022）：MCP 传输与 OAuth 用同一份解析结果。
        fn outbound_proxy(&self) -> Result<OutboundProxy, String> {
            crate::commands::network::resolve_outbound_proxy(
                self.database,
                self.credentials.as_ref(),
            )
        }
    }

    catalog_with_credentials(
        database,
        supervisor,
        &Resolver {
            database,
            credentials,
        },
    )
    .await
}

// ---------------------------------------------------------------------------
// 界面的读数

/// 「这一行现在为什么不能用」。缺席表示「没有已知的阻碍」。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
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
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerView {
    pub config: McpServerConfig,
    pub status: McpServerStatus,
    /// 运行中的服务器缓存的工具描述符；没跑起来时是空表。
    pub tools: Vec<McpToolDescriptor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<McpServerUnavailable>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct McpServerListResponse {
    pub servers: Vec<McpServerView>,
}

/// `mcp_server_save` 的入参。
///
/// `allowedTools` 与 `trustLevel` **不在**这里：目前没有任何界面写它们，而让一次「改个标签」
/// 的保存顺手清空它们，就是把两个存着的字段变成两个会被悄悄丢掉的字段。保存时保留原值
/// （见 [`save`]）。
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpServerSaveInput {
    pub id: String,
    pub label: String,
    /// `"stdio"` 或 `"http"`。HTTP 端点由 core 的传输边界校验。
    pub transport: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub http_auth_headers: Option<Vec<McpHttpAuthHeaderInput>>,
    #[serde(default)]
    pub oauth: Option<McpOAuthInput>,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpOAuthInput {
    pub issuer: String,
    pub client_id: String,
    /// Absent means `authorization_code`, which is what rows written before the setting
    /// existed mean.
    #[serde(default)]
    pub flow: Option<McpOAuthFlow>,
    /// Absent means `none`: a public client.
    #[serde(default)]
    pub client_auth: Option<McpOAuthClientAuth>,
    /// Write-only: absent preserves the stored secret, and `none` reclaims it.
    #[serde(default)]
    pub client_secret: Option<String>,
    /// 是否要求发送方约束令牌（RFC 9449 DPoP，ADR 0018）。
    #[serde(default)]
    pub dpop: bool,
    #[serde(default)]
    pub scopes: Vec<String>,
}

/// The client secret is the one field here that must never reach a log, an error or a
/// settings response, so this is a hand-written `Debug` rather than a derived one — a
/// derive is exactly how a secret ends up in a `{:?}` that nobody thought about.
impl fmt::Debug for McpOAuthInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthInput")
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .field("flow", &self.flow)
            .field("client_auth", &self.client_auth)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "<redacted>"),
            )
            .field("dpop", &self.dpop)
            .field("scopes", &self.scopes)
            .finish()
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpHttpAuthHeaderInput {
    pub name: String,
    #[serde(default)]
    pub secret: Option<String>,
}

impl fmt::Debug for McpHttpAuthHeaderInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpHttpAuthHeaderInput")
            .field("name", &self.name)
            .field("secret", &self.secret.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl fmt::Debug for McpServerSaveInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpServerSaveInput")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("transport", &self.transport)
            .field("command", &self.command)
            .field("args", &self.args)
            .field("url", &self.url)
            .field("http_auth_headers", &self.http_auth_headers)
            .field("oauth", &self.oauth)
            .field("enabled", &self.enabled)
            .finish()
    }
}

/// A review decision is deliberately a separate command from ordinary save.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpServerReviewInput {
    pub server_id: String,
    pub allowed_tools: Vec<String>,
    pub trust_level: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct McpServerDeleteResponse {
    pub deleted: bool,
    /// 这一行当时有个正在跑的进程、并且它已经被关掉了。
    pub stopped: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStopResponse {
    pub server: McpServerView,
    /// `None` 表示这个 supervisor 从没管过这个 id（本来就没有进程可关）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shutdown: Option<ShutdownOutcome>,
}

/// [`yukinal_core::mcp::ShutdownReport`] 的线上形状（字段已经是它自己的 camelCase）。
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShutdownOutcome {
    pub was_running: bool,
    /// True 表示「关 stdin，等它自己走」没成功，最后动用了强杀。
    pub killed: bool,
    /// True 表示强杀之后也没能在预算内确认它消失。
    pub unreaped: bool,
}

/// `mcp_oauth_cancel` 的响应：`accepted` 说的是**有没有真的停下什么**，而不是「命令收到了」。
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpOAuthCancelResponse {
    pub accepted: bool,
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
    let server_id = input.id.trim().to_string();
    // An in-flight authorization would write the row back when it finishes, so an edit
    // while one is waiting would be silently undone. The edit wins and the flow stops.
    state.oauth.cancel(&server_id);
    let previous = state.database.mcp_servers().get(&server_id).ok();
    let saved = save(&state.database, state.credentials.as_ref(), input)?;
    if previous
        .as_ref()
        .is_some_and(|previous| requires_restart(previous, &saved))
    {
        state.mcp.shutdown(&saved.id).await;
    }
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
    // Same reason as saving: a flow that finishes after the row is gone would resurrect it.
    state.oauth.cancel(server_id.trim());
    delete_server(
        &state.database,
        &state.mcp,
        state.credentials.as_ref(),
        &server_id,
    )
    .await
}

async fn delete_server(
    database: &Database,
    supervisor: &McpSupervisor,
    credentials: &dyn CredentialStore,
    server_id: &str,
) -> Result<McpServerDeleteResponse, String> {
    let row = load(database, server_id)?;
    let stopped = match supervisor.shutdown(server_id).await {
        Some(report) => report.was_running,
        None => false,
    };
    database
        .mcp_servers()
        .delete(server_id)
        .map_err(|error| describe_delete_failure(server_id, &error))?;
    for header in &row.http_auth_headers {
        let reference = CredentialRef::parse(&header.credential_ref).map_err(|error| {
            format!(
                "MCP 服务器 `{server_id}` 已删除，但认证头 `{}` 的凭据引用无效：{error}",
                header.name
            )
        })?;
        credentials
            .delete(&reference)
            .map_err(|error| format!("MCP 服务器 `{server_id}` 已删除，但凭据回收失败：{error}"))?;
    }
    if let Some(reference) = row
        .oauth
        .as_ref()
        .and_then(|oauth| oauth.credential_ref.as_deref())
    {
        let reference = CredentialRef::parse(reference).map_err(|error| {
            format!("MCP 服务器 `{server_id}` 已删除，但其 OAuth 凭据引用无效：{error}")
        })?;
        credentials.delete(&reference).map_err(|error| {
            format!("MCP 服务器 `{server_id}` 已删除，但 OAuth 凭据回收失败：{error}")
        })?;
    }
    if let Some(reference) = row
        .oauth
        .as_ref()
        .and_then(|oauth| oauth.client_secret_ref.as_deref())
    {
        let reference = CredentialRef::parse(reference).map_err(|error| {
            format!("MCP 服务器 `{server_id}` 已删除，但其 OAuth client secret 引用无效：{error}")
        })?;
        credentials.delete(&reference).map_err(|error| {
            format!("MCP 服务器 `{server_id}` 已删除，但 OAuth client secret 回收失败：{error}")
        })?;
    }
    // 私钥也一起回收：那台服务器已经不在了，留下一把谁也用不了的钥匙只会让凭据库更难读。
    if let Some(reference) = row
        .oauth
        .as_ref()
        .and_then(|oauth| oauth.dpop_key_ref.as_deref())
    {
        let reference = CredentialRef::parse(reference).map_err(|error| {
            format!("MCP 服务器 `{server_id}` 已删除，但其 DPoP 密钥引用无效：{error}")
        })?;
        credentials.delete(&reference).map_err(|error| {
            format!("MCP 服务器 `{server_id}` 已删除，但 DPoP 密钥回收失败：{error}")
        })?;
    }
    Ok(McpServerDeleteResponse {
        deleted: true,
        stopped,
    })
}

/// `mcp_server_start`：显式启动，并重置已经耗尽的自动恢复预算。
#[tauri::command]
pub async fn mcp_server_start(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<McpServerView, String> {
    let row = load(&state.database, &server_id)?;
    match McpTransportConfig::from_server_config(&row, DEFAULT_REQUEST_TIMEOUT) {
        // 起不来（http、缺 command、配置非法）时**不报错**，而是把理由放进视图里：界面只有
        // 一个地方显示「为什么它没在跑」，`Err` 留给「这个请求本身做不到」。
        Err(error) => {
            let message = error.to_string();
            Ok(view_with_unavailable(&state.mcp, row, &error, message).await)
        }
        Ok(mut config) => {
            if let Err(error) = apply_http_auth(
                &row,
                &mut config,
                &state.database,
                state.credentials.clone(),
            ) {
                let message = error.to_string();
                return Ok(view_with_unavailable(&state.mcp, row, &error, message).await);
            }
            match state.mcp.start_transport(&config).await {
                Ok(_) => Ok(view(&state.mcp, row).await),
                Err(error) => {
                    let message = error.to_string();
                    Ok(view_with_unavailable(&state.mcp, row, &error, message).await)
                }
            }
        }
    }
}

/// Run the configured OAuth flow and persist the resulting token bundle in the OS
/// credential store.
///
/// This call **blocks until the flow ends**: the browser redirect waits for the loopback
/// callback, and the device-code flow waits for the user to approve while polling. That is
/// deliberate — the polling then lives in this request instead of in a task nobody owns,
/// so abandoning the call (closing the window) ends it. [`mcp_oauth_cancel`] is the other
/// way out.
#[tauri::command]
pub async fn mcp_oauth_connect(
    state: State<'_, AppState>,
    app: AppHandle,
    server_id: String,
) -> Result<oauth::McpOAuthConnectResult, String> {
    let flow = state.oauth.begin(server_id.trim())?;
    // 授权与 token 请求和别的出站请求走同一条路：应用级代理设置（ADR 0022）。
    let proxy = crate::commands::network::resolve_outbound_proxy(
        &state.database,
        state.credentials.as_ref(),
    )?;
    let result = oauth::connect(
        &state.database,
        state.credentials.clone(),
        server_id.trim(),
        proxy,
        |url| {
            app.opener()
                .open_url(url, None::<&str>)
                .map_err(|error| format!("无法打开系统浏览器完成 OAuth：{error}"))
        },
        |prompt| {
            // The prompt is an event, not a return value: the call above is still waiting,
            // and the UI needs the code *while* it waits. No token is in this payload.
            app.emit(
                &crate::commands::tauri_event_name("mcp.oauth_device_code"),
                json!({
                    "serverId": server_id.trim(),
                    "userCode": prompt.user_code,
                    "verificationUri": prompt.verification_uri,
                    "verificationUriComplete": prompt.verification_uri_complete,
                    "expiresAt": yukinal_core::sidecar::iso8601_utc(prompt.expires_at),
                }),
            )
            .map_err(|error| format!("无法把设备码显示到界面：{error}"))
        },
        flow.token(),
    )
    .await?;
    drop(flow);
    // A running session still has the old token. Stop it after the new token is
    // durably stored; the next start creates a fresh authenticated session.
    state.mcp.shutdown(server_id.trim()).await;
    Ok(result)
}

/// Stop an in-flight authorization for one server.
///
/// The answer is a real one: `false` means nothing was waiting (already finished, timed
/// out, or never started in this window), which is not an error the UI should report as
/// a failure.
#[tauri::command]
pub async fn mcp_oauth_cancel(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<McpOAuthCancelResponse, String> {
    Ok(McpOAuthCancelResponse {
        accepted: state.oauth.cancel(server_id.trim()),
    })
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

/// Save a user's review of the currently advertised tool list.
///
/// Keeping review separate from the normal form means a rename or command edit cannot
/// accidentally widen the tool surface. Unknown names are rejected, not filtered.
#[tauri::command]
pub async fn mcp_server_review(
    state: State<'_, AppState>,
    input: McpServerReviewInput,
) -> Result<McpServerView, String> {
    let row = save_review(&state.database, &state.mcp, input).await?;
    Ok(view(&state.mcp, row).await)
}

async fn save_review(
    database: &Database,
    supervisor: &McpSupervisor,
    input: McpServerReviewInput,
) -> Result<McpServerConfig, String> {
    let mut row = load(database, &input.server_id)?;
    let status = supervisor.status(&input.server_id).await;
    if !status.running {
        return Err("启动 MCP 服务器后才能审核它声明的工具。".into());
    }
    if input.trust_level != "reviewed" && input.trust_level != "unreviewed" {
        return Err(format!(
            "未知的 MCP trustLevel `{}`；只支持 reviewed 或 unreviewed。",
            input.trust_level
        ));
    }
    let advertised_tools = supervisor.tools(&input.server_id).await;
    let advertised: HashSet<&str> = advertised_tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    let mut seen = HashSet::new();
    for tool in &input.allowed_tools {
        if !advertised.contains(tool.as_str()) {
            return Err(format!(
                "工具 `{tool}` 不在服务器当前声明的工具列表中；刷新并重新启动后再审核。"
            ));
        }
        if !seen.insert(tool.as_str()) {
            return Err(format!("工具 `{tool}` 在审核列表中重复。"));
        }
    }
    row.allowed_tools = input.allowed_tools;
    row.trust_level = input.trust_level;
    database
        .mcp_servers()
        .upsert(&row)
        .map_err(|error| format!("保存 MCP 工具审核失败：{error}"))?;
    Ok(row)
}

// ---------------------------------------------------------------------------
// 执行（host.rs 用）

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

    // Cancellation sends MCP `notifications/cancelled`, then stops waiting. The
    // notification is best-effort: a server may already have completed, so the UI
    // must not claim that side effects were rolled back.
    match supervisor
        .call_with_cancel(&server_id, tool, input.clone(), cancel)
        .await
    {
        Ok(result) => Ok(success(json!(McpToolCallOutput {
            server_id,
            tool: tool.to_string(),
            is_error: result.is_error,
            text: result.text(),
            content: result.content,
            structured_content: result.structured_content,
        }))),
        Err(McpError::Cancelled { .. }) => Ok(cancelled_failure()),
        Err(error) => Ok(mcp_tool_failure(&server_id, tool, error, cancel)),
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
    // `Exited` **不是**可重试的：当前策略不会把第三方进程拉回来。
    let retryable = matches!(error, McpError::Timeout { .. });
    let code = match &error {
        McpError::Timeout { .. } => "timeout",
        McpError::UnknownTool { .. } => "not_found",
        McpError::InvalidArguments { .. }
        | McpError::MissingUrl { .. }
        | McpError::InvalidUrl { .. } => "invalid_input",
        McpError::UnsupportedTransport { .. } => "denied_by_policy",
        McpError::Http { .. } => "transport",
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
             stopped, or it crashed and its bounded automatic recovery has not finished"
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

// ---------------------------------------------------------------------------
// 纯逻辑（可测）

/// 保存一行。`Err(String)` 是**给用户看**的拒绝理由，所以它必须能照原样显示。
pub(crate) fn save(
    database: &Database,
    credentials: &dyn CredentialStore,
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

    let transport = input.transport.trim().to_ascii_lowercase();
    let is_stdio = transport == "stdio";
    let is_http = transport == "http";
    let command = if is_stdio {
        input
            .command
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    } else {
        None
    };
    let args = if is_stdio { input.args } else { None };
    let url = if is_http {
        input
            .url
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    } else {
        None
    };
    let existing = database.mcp_servers().get(id).ok();
    let requested_headers = input.http_auth_headers.unwrap_or_default();
    if !is_http && !requested_headers.is_empty() {
        return Err("HTTP authentication settings require the HTTP transport".to_string());
    }
    let existing_headers = existing
        .as_ref()
        .map(|row| row.http_auth_headers.as_slice())
        .unwrap_or_default();
    let (http_auth_headers, newly_staged) =
        resolve_http_auth_headers(id, existing_headers, requested_headers, credentials)?;
    let (oauth, staged_secrets) = match resolve_oauth_config(
        id,
        is_http,
        input.oauth,
        existing.as_ref().and_then(|row| row.oauth.clone()),
        &http_auth_headers,
        credentials,
    ) {
        Ok(resolved) => resolved,
        Err(error) => {
            delete_credentials(credentials, &newly_staged);
            return Err(error);
        }
    };
    let mut newly_staged = newly_staged;
    newly_staged.extend(staged_secrets);
    let review_surface_changed = existing.as_ref().is_none_or(|row| {
        row.transport != transport || row.command != command || row.args != args || row.url != url
    });
    let config = McpServerConfig {
        id: id.to_string(),
        label: label.to_string(),
        transport,
        command,
        args,
        url,
        http_auth_headers,
        oauth,
        enabled: input.enabled,
        // Changing what is being trusted invalidates the old review. A label-only edit
        // preserves it because the reviewed tool surface has not moved.
        allowed_tools: if review_surface_changed {
            Vec::new()
        } else {
            existing
                .as_ref()
                .map(|row| row.allowed_tools.clone())
                .unwrap_or_default()
        },
        trust_level: if review_surface_changed {
            "unreviewed".to_string()
        } else {
            existing
                .as_ref()
                .map(|row| row.trust_level.clone())
                .unwrap_or_else(|| "unreviewed".to_string())
        },
    };

    if let Err(error) = check_transport(&config) {
        delete_credentials(credentials, &newly_staged);
        return Err(error);
    }

    let previous_refs = existing
        .as_ref()
        .map(|row| {
            row.http_auth_headers
                .iter()
                .map(|header| header.credential_ref.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let previous_oauth_ref = existing
        .as_ref()
        .and_then(|row| row.oauth.as_ref())
        .and_then(|oauth| oauth.credential_ref.clone());
    let previous_oauth_secret_ref = existing
        .as_ref()
        .and_then(|row| row.oauth.as_ref())
        .and_then(|oauth| oauth.client_secret_ref.clone());
    let previous_oauth_key_ref = existing
        .as_ref()
        .and_then(|row| row.oauth.as_ref())
        .and_then(|oauth| oauth.dpop_key_ref.clone());

    if let Err(error) = database.mcp_servers().upsert(&config) {
        delete_credentials(credentials, &newly_staged);
        return Err(format!("保存 MCP 服务器 `{id}` 失败：{error}"));
    }

    for old_ref in previous_refs {
        if !config
            .http_auth_headers
            .iter()
            .any(|header| header.credential_ref == old_ref)
        {
            if let Ok(reference) = CredentialRef::parse(&old_ref) {
                if let Err(error) = credentials.delete(&reference) {
                    tracing::warn!(
                        server_id = %id,
                        "saved MCP HTTP authentication but could not reclaim its previous credential: {error}"
                    );
                }
            }
        }
    }
    if let Some(old_ref) = previous_oauth_ref {
        if config
            .oauth
            .as_ref()
            .and_then(|oauth| oauth.credential_ref.as_deref())
            != Some(old_ref.as_str())
        {
            if let Ok(reference) = CredentialRef::parse(&old_ref) {
                if let Err(error) = credentials.delete(&reference) {
                    tracing::warn!(
                        server_id = %id,
                        "saved MCP OAuth configuration but could not reclaim its previous token: {error}"
                    );
                }
            }
        }
    }
    // Same rule for the client secret: it is reclaimed when nothing points at it any more
    // (dropped, rotated, or replaced by a different authentication method).
    if let Some(old_ref) = previous_oauth_secret_ref {
        if config
            .oauth
            .as_ref()
            .and_then(|oauth| oauth.client_secret_ref.as_deref())
            != Some(old_ref.as_str())
        {
            if let Ok(reference) = CredentialRef::parse(&old_ref) {
                if let Err(error) = credentials.delete(&reference) {
                    tracing::warn!(
                        server_id = %id,
                        "saved MCP OAuth configuration but could not reclaim its previous client secret: {error}"
                    );
                }
            }
        }
    }
    // And the same rule for the DPoP key: it is reclaimed when the new configuration no
    // longer points at it (turned off, or the identity changed). An orphaned private key
    // helps nobody, and the next connection that wants one generates a fresh key.
    if let Some(old_ref) = previous_oauth_key_ref {
        if config
            .oauth
            .as_ref()
            .and_then(|oauth| oauth.dpop_key_ref.as_deref())
            != Some(old_ref.as_str())
        {
            if let Ok(reference) = CredentialRef::parse(&old_ref) {
                if let Err(error) = credentials.delete(&reference) {
                    tracing::warn!(
                        server_id = %id,
                        "saved MCP OAuth configuration but could not reclaim its previous DPoP key: {error}"
                    );
                }
            }
        }
    }
    Ok(config)
}

fn resolve_oauth_config(
    server_id: &str,
    is_http: bool,
    requested: Option<McpOAuthInput>,
    existing: Option<McpOAuthConfig>,
    http_auth_headers: &[McpHttpAuthHeaderConfig],
    credentials: &dyn CredentialStore,
) -> Result<(Option<McpOAuthConfig>, Vec<CredentialRef>), String> {
    let Some(requested) = requested else {
        return Ok((None, Vec::new()));
    };
    if !is_http {
        return Err("OAuth settings require the HTTP transport".to_string());
    }
    if http_auth_headers
        .iter()
        .any(|header| header.name.eq_ignore_ascii_case("authorization"))
    {
        return Err(
            "OAuth and a static Authorization header cannot be configured together".to_string(),
        );
    }
    let raw_issuer = requested.issuer.trim();
    let issuer = if raw_issuer.is_empty() {
        String::new()
    } else {
        yukinal_core::mcp::validate_oauth_url(server_id, raw_issuer)
            .map_err(|error| format!("无效的 OAuth issuer：{error}"))?
            .trim_end_matches('/')
            .to_string()
    };
    let client_id = requested.client_id.trim();
    if client_id.len() > 512 || client_id.chars().any(char::is_control) {
        return Err("OAuth client id must be at most 512 printable characters".to_string());
    }
    let flow = requested.flow.unwrap_or_default();
    let client_auth = requested.client_auth.unwrap_or_default();
    if client_auth.needs_secret() && client_id.is_empty() {
        return Err(
            "客户端密钥认证需要手填 client id：动态注册得到的是一个公共客户端，\
             服务端即使返回 secret 也不会被使用。"
                .to_string(),
        );
    }
    if client_auth == McpOAuthClientAuth::ClientSecretBasic && client_id.contains(':') {
        return Err(
            "client_secret_basic 的 client id 不能包含冒号：RFC 7617 用它分隔用户名与密码。"
                .to_string(),
        );
    }
    if requested.scopes.len() > 32 {
        return Err("OAuth may request at most 32 scopes".to_string());
    }
    let mut scopes = Vec::new();
    for scope in requested.scopes {
        let scope = scope.trim();
        if scope.is_empty()
            || scope.len() > 128
            || scope
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
        {
            return Err("each OAuth scope must be 1 to 128 non-whitespace characters".to_string());
        }
        if !scopes.iter().any(|existing| existing == scope) {
            scopes.push(scope.to_string());
        }
    }
    let preserve = existing.as_ref().is_some_and(|existing| {
        !issuer.is_empty()
            && existing.issuer.trim_end_matches('/') == issuer.trim_end_matches('/')
            && existing.client_id == client_id
            // The flow is part of the identity, not a display preference: a token obtained
            // through the browser redirect is not automatically the one this flow would
            // ask for, so switching forces a fresh authorization instead of reusing it.
            && existing.flow == flow
            && existing.client_auth == client_auth
            // 打开/关掉发送方约束同样是换身份：旧令牌没有绑定这把钥匙（或绑的是另一把），
            // 留着它只会让下一次请求以一种说不清的方式失败。
            && existing.dpop == requested.dpop
            && existing.scopes == scopes
    });
    // The secret is write-only, so "no new value" means one of two things: keep the one
    // that is already stored (same method), or refuse. It never means "store an empty
    // secret" — and switching the method drops the old secret, because the user is telling
    // us the previous way of authenticating was wrong.
    let mut staged = Vec::new();
    let client_secret_ref = if !client_auth.needs_secret() {
        None
    } else {
        let entered = match requested.client_secret.filter(|value| !value.is_empty()) {
            Some(secret) => {
                if secret.len() > 8_192 || secret.chars().any(char::is_control) {
                    return Err(
                        "OAuth client secret 必须是 1 到 8192 个不含控制字符的字符".to_string()
                    );
                }
                let reference = credentials
                    .set(
                        "mcp",
                        &format!("{server_id}-{}", next_id("oauth-secret")),
                        &Secret::from_utf8(secret),
                    )
                    .map_err(|error| format!("保存 OAuth client secret 失败：{error}"))?;
                staged.push(reference.clone());
                Some(reference.to_string_ref())
            }
            None => None,
        };
        // Keeping the stored secret is only legitimate for the *same* method: switching
        // methods reclaims the old secret, so the user has to enter the new one.
        let carried = existing
            .as_ref()
            .filter(|oauth| oauth.client_auth == client_auth)
            .and_then(|oauth| oauth.client_secret_ref.clone());
        Some(match entered.or(carried) {
            Some(reference) => reference,
            None => return Err(
                "这种客户端认证方式需要一个 client secret，而这次没有输入新的，也没有可保留的。\
                     请填入 secret（切换认证方式会让旧 secret 失效）。"
                    .to_string(),
            ),
        })
    };
    Ok((
        Some(McpOAuthConfig {
            issuer,
            client_id: client_id.to_string(),
            flow,
            client_auth,
            client_secret_ref,
            dpop: requested.dpop,
            // 密钥引用只在「同一个身份 + 仍然要 DPoP」时沿用：换身份等于旧令牌作废，
            // 那把钥匙也就没有对手了，回收掉比留着更像话（下一次连接会生成新的）。
            dpop_key_ref: preserve
                .then(|| {
                    existing
                        .as_ref()
                        .and_then(|oauth| oauth.dpop_key_ref.clone())
                })
                .flatten(),
            scopes,
            token_endpoint: preserve
                .then(|| {
                    existing
                        .as_ref()
                        .and_then(|oauth| oauth.token_endpoint.clone())
                })
                .flatten(),
            credential_ref: preserve
                .then(|| {
                    existing
                        .as_ref()
                        .and_then(|oauth| oauth.credential_ref.clone())
                })
                .flatten(),
        }),
        staged,
    ))
}

fn resolve_http_auth_headers(
    server_id: &str,
    existing: &[McpHttpAuthHeaderConfig],
    requested: Vec<McpHttpAuthHeaderInput>,
    credentials: &dyn CredentialStore,
) -> Result<(Vec<McpHttpAuthHeaderConfig>, Vec<CredentialRef>), String> {
    if requested.len() > yukinal_core::mcp::MAX_HTTP_AUTH_HEADERS {
        return Err(format!(
            "一个 HTTP endpoint 最多配置 {} 个认证头。",
            yukinal_core::mcp::MAX_HTTP_AUTH_HEADERS
        ));
    }
    let mut seen = HashSet::new();
    let mut resolved = Vec::with_capacity(requested.len());
    let mut staged = Vec::new();
    for header in requested {
        let name = header.name.trim();
        if name.is_empty() {
            delete_credentials(credentials, &staged);
            return Err("HTTP 认证头需要一个非空名称。".to_string());
        }
        if !seen.insert(name.to_ascii_lowercase()) {
            delete_credentials(credentials, &staged);
            return Err(format!("HTTP 认证头 `{name}` 重复。"));
        }
        let secret = header.secret.filter(|value| !value.is_empty());
        if let Err(error) = McpHttpAuthHeader::new(name, secret.as_deref().unwrap_or("placeholder"))
        {
            delete_credentials(credentials, &staged);
            return Err(format!("无效的 HTTP 认证头 `{name}`：{error}"));
        }
        if let Some(secret) = secret {
            let reference = match credentials.set(
                "mcp",
                &format!("{server_id}-{}", next_id("cred")),
                &Secret::from_utf8(secret),
            ) {
                Ok(reference) => reference,
                Err(error) => {
                    delete_credentials(credentials, &staged);
                    return Err(format!("保存 MCP HTTP 凭据失败：{error}"));
                }
            };
            staged.push(reference.clone());
            resolved.push(McpHttpAuthHeaderConfig {
                name: name.to_string(),
                credential_ref: reference.to_string_ref(),
            });
            continue;
        }
        let Some(existing) = existing
            .iter()
            .find(|candidate| candidate.name.eq_ignore_ascii_case(name))
        else {
            delete_credentials(credentials, &staged);
            return Err(format!(
                "HTTP 认证头 `{name}` 没有新 secret，也没有可保留的现有凭据。"
            ));
        };
        resolved.push(McpHttpAuthHeaderConfig {
            name: name.to_string(),
            credential_ref: existing.credential_ref.clone(),
        });
    }
    Ok((resolved, staged))
}

fn delete_credentials(credentials: &dyn CredentialStore, references: &[CredentialRef]) {
    for reference in references {
        let _ = credentials.delete(reference);
    }
}

/// 这一行的**传输方式**能不能用。
///
/// 判断不在这里重写：问 [`McpTransportConfig::from_server_config`]。它同时会拒绝一个无法变成
/// 内部名段的 id —— 那种行永远不可能有工具，早一点拒绝比留一个永远不工作的条目好。
///
/// 「还没写完」的错误（禁用、没填 command/url）**允许**保存：草稿是合法的，用户先写下来再补。
fn check_transport(config: &McpServerConfig) -> Result<(), String> {
    match McpTransportConfig::from_server_config(config, DEFAULT_REQUEST_TIMEOUT) {
        Ok(_) => Ok(()),
        Err(
            error @ (McpError::UnsupportedTransport { .. }
            | McpError::InvalidUrl { .. }
            | McpError::InvalidConfig { .. }),
        ) => Err(format!(
            "{}。当前支持 stdio 与 Streamable HTTP 传输；远程 HTTP 必须使用 HTTPS，明文 HTTP 只允许回环地址。",
            error
        )),
        Err(_) => Ok(()),
    }
}

fn http_auth_config_error(row: &McpServerConfig) -> Option<McpError> {
    if row.transport != "http" && !row.http_auth_headers.is_empty() {
        return Some(McpError::InvalidConfig {
            server_id: row.id.clone(),
            reason: "stdio transport cannot carry HTTP authentication settings".to_string(),
        });
    }
    if row.transport != "http" && row.oauth.is_some() {
        return Some(McpError::InvalidConfig {
            server_id: row.id.clone(),
            reason: "stdio transport cannot carry OAuth authentication settings".to_string(),
        });
    }
    if row.http_auth_headers.len() > yukinal_core::mcp::MAX_HTTP_AUTH_HEADERS {
        return Some(McpError::InvalidConfig {
            server_id: row.id.clone(),
            reason: format!(
                "an HTTP endpoint may carry at most {} authentication headers",
                yukinal_core::mcp::MAX_HTTP_AUTH_HEADERS
            ),
        });
    }
    let mut names = HashSet::new();
    for header in &row.http_auth_headers {
        if header.name.trim().is_empty() || header.credential_ref.trim().is_empty() {
            return Some(McpError::InvalidConfig {
                server_id: row.id.clone(),
                reason: "HTTP authentication headers require a name and credential reference"
                    .to_string(),
            });
        }
        if let Err(reason) = McpHttpAuthHeader::new(&header.name, "placeholder") {
            return Some(McpError::InvalidConfig {
                server_id: row.id.clone(),
                reason,
            });
        }
        if !names.insert(header.name.to_ascii_lowercase()) {
            return Some(McpError::InvalidConfig {
                server_id: row.id.clone(),
                reason: format!(
                    "HTTP authentication header `{}` is configured more than once",
                    header.name
                ),
            });
        }
    }
    if let Some(oauth) = &row.oauth {
        if row
            .http_auth_headers
            .iter()
            .any(|header| header.name.eq_ignore_ascii_case("authorization"))
        {
            return Some(McpError::InvalidConfig {
                server_id: row.id.clone(),
                reason: "OAuth and a static Authorization header are mutually exclusive"
                    .to_string(),
            });
        }
        if !oauth.issuer.trim().is_empty() {
            if let Err(error) = yukinal_core::mcp::validate_oauth_url(&row.id, &oauth.issuer) {
                return Some(error);
            }
        }
        if oauth.client_id.len() > 512 || oauth.client_id.chars().any(char::is_control) {
            return Some(McpError::InvalidConfig {
                server_id: row.id.clone(),
                reason: "OAuth client id is too long or contains control characters".to_string(),
            });
        }
        if oauth.client_auth.needs_secret() {
            if oauth.client_id.trim().is_empty() {
                return Some(McpError::InvalidConfig {
                    server_id: row.id.clone(),
                    reason: "client secret authentication needs a hand-filled client id"
                        .to_string(),
                });
            }
            if oauth.client_secret_ref.is_none() {
                return Some(McpError::InvalidConfig {
                    server_id: row.id.clone(),
                    reason: "OAuth uses client secret authentication but no secret is stored; \
                             edit the server and enter it"
                        .to_string(),
                });
            }
        }
        match (
            oauth.token_endpoint.as_deref(),
            oauth.credential_ref.as_deref(),
        ) {
            (None, None) => {
                return Some(McpError::InvalidConfig {
                    server_id: row.id.clone(),
                    reason: "OAuth is configured but not connected; run Connect OAuth".to_string(),
                })
            }
            (Some(token_endpoint), Some(_)) => {
                if oauth.client_id.trim().is_empty() {
                    return Some(McpError::InvalidConfig {
                        server_id: row.id.clone(),
                        reason: "connected OAuth configuration has no client id".to_string(),
                    });
                }
                if let Err(error) = yukinal_core::mcp::validate_oauth_url(&row.id, token_endpoint) {
                    return Some(error);
                }
            }
            _ => {
                return Some(McpError::InvalidConfig {
                    server_id: row.id.clone(),
                    reason: "OAuth token endpoint and credential reference must both be present"
                        .to_string(),
                })
            }
        }
    }
    None
}

fn apply_http_auth(
    row: &McpServerConfig,
    config: &mut McpTransportConfig,
    database: &Database,
    credentials: Arc<dyn CredentialStore>,
) -> Result<(), McpError> {
    if let Some(error) = http_auth_config_error(row) {
        return Err(error);
    }
    let McpTransportConfig::Http(http) = config else {
        return Ok(());
    };
    // 出站代理是应用级设置（ADR 0022）：在这里落地，之后建立连接、发请求都按它走。
    let outbound = crate::commands::network::resolve_outbound_proxy(database, credentials.as_ref())
        .map_err(|reason| McpError::InvalidConfig {
            server_id: row.id.clone(),
            reason,
        })?;
    let mut next = http
        .clone()
        .with_proxy(outbound.proxy.clone(), outbound.credential.clone());
    for header in &row.http_auth_headers {
        let reference = CredentialRef::parse(&header.credential_ref).map_err(|error| {
            McpError::InvalidConfig {
                server_id: row.id.clone(),
                reason: format!("invalid HTTP credential reference: {error}"),
            }
        })?;
        let secret = credentials
            .get(&reference)
            .map_err(|error| McpError::InvalidConfig {
                server_id: row.id.clone(),
                reason: format!("could not read HTTP authentication secret: {error}"),
            })?;
        let secret = secret.as_utf8().map_err(|error| McpError::InvalidConfig {
            server_id: row.id.clone(),
            reason: format!("HTTP authentication secret is not UTF-8: {error}"),
        })?;
        next = next.with_auth_header(&header.name, secret.as_ref())?;
    }
    if let Some(oauth) = &row.oauth {
        let token_endpoint = oauth
            .token_endpoint
            .as_deref()
            .ok_or_else(|| McpError::OAuth {
                server_id: row.id.clone(),
                reason: "OAuth is not connected: no token endpoint is stored".to_string(),
            })?;
        let credential_ref = oauth
            .credential_ref
            .as_deref()
            .ok_or_else(|| McpError::OAuth {
                server_id: row.id.clone(),
                reason: "OAuth is not connected: no token credential is stored".to_string(),
            })?;
        let source = oauth::source_from_config(
            credentials,
            &McpOAuthSourceConfig {
                server_id: row.id.clone(),
                resource: next.url.clone(),
                token_endpoint: token_endpoint.to_string(),
                client_id: oauth.client_id.clone(),
                client_auth: oauth.client_auth,
                client_secret_ref: oauth.client_secret_ref.clone(),
                dpop_key_ref: oauth.dpop_key_ref.clone(),
                proxy: outbound.clone(),
                scopes: oauth.scopes.clone(),
                credential_ref: credential_ref.to_string(),
            },
        )
        .map_err(|reason| McpError::OAuth {
            server_id: row.id.clone(),
            reason,
        })?;
        next = next.with_oauth_source(source)?;
    }
    *http = next;
    Ok(())
}

/// A saved connection change must not leave the supervisor attached to the old target.
fn requires_restart(previous: &McpServerConfig, next: &McpServerConfig) -> bool {
    (previous.enabled && !next.enabled)
        || previous.transport != next.transport
        || previous.command != next.command
        || previous.args != next.args
        || previous.url != next.url
        || previous.http_auth_headers != next.http_auth_headers
        || previous.oauth != next.oauth
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
/// 规则是「跑着就以状态为准，没跑才说为什么」；stdio 与 HTTP 共用同一份状态视图。
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
        match http_auth_config_error(&row).map_or_else(
            || McpTransportConfig::from_server_config(&row, DEFAULT_REQUEST_TIMEOUT),
            Err,
        ) {
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
        message: describe_dead(server_id, Some(exit), status.restart.clone()),
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
    use yukinal_credentials::memory::MemoryCredentialStore;

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
            http_auth_headers: None,
            oauth: None,
            enabled,
        }
    }

    fn auth_header(name: &str, secret: Option<&str>) -> McpHttpAuthHeaderInput {
        McpHttpAuthHeaderInput {
            name: name.to_string(),
            secret: secret.map(str::to_string),
        }
    }

    /// A public-client OAuth input; tests that need a flow or a secret layer it on with
    /// struct-update syntax, so adding a field here does not touch every call site.
    fn oauth_input(issuer: &str, client_id: &str, scopes: &[&str]) -> McpOAuthInput {
        McpOAuthInput {
            issuer: issuer.to_string(),
            client_id: client_id.to_string(),
            flow: None,
            client_auth: None,
            client_secret: None,
            dpop: false,
            scopes: scopes.iter().map(|scope| scope.to_string()).collect(),
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
        // Tests that never launch the row (disabled rows and invalid HTTP endpoints)
        // must not require Node just to construct a fixture record.
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

    /* ── 配置往返 ─────────────────────────────────────────────────────────── */

    #[test]
    fn a_saved_row_reads_back_unchanged_through_get_and_list() {
        let (path, db) = temp_db("round-trip");
        let credentials = MemoryCredentialStore::new();
        let mut input = save_input("mcp_1", "stdio", true);
        input.args = Some(vec!["/srv/mcp/server.js".to_string(), "--mode".to_string()]);
        let saved = save(&db, &credentials, input).expect("save");

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
        let credentials = MemoryCredentialStore::new();
        save(&db, &credentials, save_input("mcp_1", "stdio", true)).expect("first save");

        // 模拟「这两个字段由别的东西写过」（它们目前没有界面）。
        let mut stored = db.mcp_servers().get("mcp_1").expect("get");
        stored.allowed_tools = vec!["echo".to_string()];
        stored.trust_level = "reviewed".to_string();
        insert(&db, &stored);

        let mut renamed = save_input("mcp_1", "stdio", false);
        renamed.label = "renamed".to_string();
        let saved = save(&db, &credentials, renamed).expect("second save");

        assert_eq!(saved.label, "renamed");
        assert!(!saved.enabled);
        assert_eq!(
            saved.allowed_tools,
            vec!["echo".to_string()],
            "a rename must not silently drop the reviewed tool list"
        );
        assert_eq!(saved.trust_level, "reviewed");

        let mut moved = save_input("mcp_1", "stdio", false);
        moved.command = Some("different-node".to_string());
        let moved = save(&db, &credentials, moved).expect("the moved endpoint is still saveable");
        assert!(
            moved.allowed_tools.is_empty(),
            "changing the command must revoke the old endpoint's review"
        );
        assert_eq!(moved.trust_level, "unreviewed");
        cleanup(&path);
    }

    #[test]
    fn saving_a_connection_change_stops_the_old_supervisor_session() {
        let (path, db) = temp_db("restart-required");
        let credentials = MemoryCredentialStore::new();
        let first =
            save(&db, &credentials, save_input("mcp_1", "stdio", true)).expect("first save");

        let mut renamed = save_input("mcp_1", "stdio", true);
        renamed.label = "renamed".to_string();
        let renamed = save(&db, &credentials, renamed).expect("label-only save");
        assert!(!requires_restart(&first, &renamed));

        let mut moved = save_input("mcp_1", "stdio", true);
        moved.command = Some("different-node".to_string());
        let moved = save(&db, &credentials, moved).expect("command change");
        assert!(requires_restart(&renamed, &moved));

        let mut disabled = save_input("mcp_1", "stdio", false);
        disabled.command = moved.command.clone();
        let disabled = save(&db, &credentials, disabled).expect("disable");
        assert!(requires_restart(&moved, &disabled));
        cleanup(&path);
    }

    #[tokio::test]
    async fn tool_review_accepts_only_currently_advertised_names() {
        let Some((path, db, supervisor)) = fixture_setup("review", "ok", "mcp_1") else {
            return;
        };
        let credentials = MemoryCredentialStore::new();
        catalog(&db, &supervisor, Arc::new(credentials))
            .await
            .expect("start and catalog");

        let saved = save_review(
            &db,
            &supervisor,
            McpServerReviewInput {
                server_id: "mcp_1".into(),
                allowed_tools: vec!["echo".into()],
                trust_level: "reviewed".into(),
            },
        )
        .await
        .expect("review succeeds");
        assert_eq!(saved.allowed_tools, vec!["echo".to_string()]);
        assert_eq!(saved.trust_level, "reviewed");

        let error = save_review(
            &db,
            &supervisor,
            McpServerReviewInput {
                server_id: "mcp_1".into(),
                allowed_tools: vec!["invented".into()],
                trust_level: "reviewed".into(),
            },
        )
        .await
        .expect_err("unknown tool must be rejected");
        assert!(error.contains("invented"), "{error}");
        assert_eq!(
            db.mcp_servers().get("mcp_1").expect("row").allowed_tools,
            vec!["echo".to_string()],
            "a rejected review must not partially update the row"
        );

        supervisor.shutdown("mcp_1").await;
        cleanup(&path);
    }

    #[test]
    fn an_http_row_is_saved_with_only_its_endpoint() {
        let (path, db) = temp_db("http-saved");
        let credentials = MemoryCredentialStore::new();
        let mut input = save_input("mcp_http", "http", true);
        input.command = Some("must-not-survive".to_string());
        input.args = Some(vec!["--also-not".to_string()]);
        input.url = Some("https://mcp.example.com/mcp".to_string());
        let saved = save(&db, &credentials, input).expect("a valid HTTP row");
        assert_eq!(saved.transport, "http");
        assert!(saved.command.is_none());
        assert!(saved.args.is_none());
        assert_eq!(saved.url.as_deref(), Some("https://mcp.example.com/mcp"));

        let mut unsafe_transport = save_input("mcp_remote_http", "http", true);
        unsafe_transport.url = Some("http://mcp.example.com/mcp".to_string());
        let error = save(&db, &credentials, unsafe_transport)
            .expect_err("remote plaintext HTTP is refused");
        assert!(error.contains("HTTPS"), "{error}");
        assert_eq!(db.mcp_servers().list().expect("list").len(), 1);
        cleanup(&path);
    }

    #[test]
    fn http_auth_secret_is_stored_outside_the_config_and_rotated() {
        let (path, db) = temp_db("http-auth");
        let credentials = MemoryCredentialStore::new();
        let url = "https://mcp.example.com/mcp".to_string();

        let mut first = save_input("mcp_http", "http", true);
        first.url = Some(url.clone());
        first.http_auth_headers = Some(vec![
            auth_header("Authorization", Some("Bearer first-secret")),
            auth_header("X-API-Key", Some("second-secret")),
        ]);
        let saved = save(&db, &credentials, first).expect("save authenticated HTTP row");
        assert_eq!(
            saved
                .http_auth_headers
                .iter()
                .map(|header| header.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Authorization", "X-API-Key"]
        );
        assert!(
            !serde_json::to_string(&saved)
                .expect("serialize config")
                .contains("first-secret"),
            "the config model must never carry the secret value"
        );

        let first_reference =
            CredentialRef::parse(&saved.http_auth_headers[0].credential_ref).expect("reference");
        let second_reference =
            CredentialRef::parse(&saved.http_auth_headers[1].credential_ref).expect("reference");
        assert_eq!(
            credentials
                .get(&first_reference)
                .expect("stored secret")
                .as_utf8()
                .expect("UTF-8 secret"),
            "Bearer first-secret"
        );
        assert_eq!(
            credentials
                .get(&second_reference)
                .expect("stored second secret")
                .as_utf8()
                .expect("UTF-8 secret"),
            "second-secret"
        );

        let mut preserved = save_input("mcp_http", "http", true);
        preserved.label = "renamed".to_string();
        preserved.url = Some(url.clone());
        preserved.http_auth_headers = Some(vec![
            auth_header("X-API-Key", None),
            auth_header("Authorization", None),
        ]);
        let preserved = save(&db, &credentials, preserved).expect("preserve existing secret");
        assert_eq!(
            preserved.http_auth_headers[0].credential_ref,
            saved.http_auth_headers[1].credential_ref,
            "headers are matched by name even when their order changes"
        );
        assert_eq!(
            preserved.http_auth_headers[1].credential_ref,
            saved.http_auth_headers[0].credential_ref
        );
        assert!(credentials.has(&first_reference).expect("still present"));
        assert!(credentials.has(&second_reference).expect("still present"));

        let mut changed_without_secret = save_input("mcp_http", "http", true);
        changed_without_secret.url = Some(url.clone());
        changed_without_secret.http_auth_headers = Some(vec![auth_header("X-Other-Key", None)]);
        let error = save(&db, &credentials, changed_without_secret)
            .expect_err("a changed header cannot silently reuse the old secret");
        assert!(error.contains("没有新 secret"), "{error}");
        assert_eq!(
            db.mcp_servers()
                .get("mcp_http")
                .expect("unchanged row")
                .http_auth_headers
                .len(),
            2
        );

        let mut rotated = save_input("mcp_http", "http", true);
        rotated.url = Some(url.clone());
        rotated.http_auth_headers = Some(vec![
            auth_header("Authorization", Some("rotated-secret")),
            auth_header("X-API-Key", None),
        ]);
        let rotated = save(&db, &credentials, rotated).expect("rotate credential");
        let rotated_reference = CredentialRef::parse(&rotated.http_auth_headers[0].credential_ref)
            .expect("rotated reference");
        assert_ne!(rotated_reference, first_reference);
        assert_eq!(
            rotated.http_auth_headers[1].credential_ref,
            second_reference.to_string_ref()
        );
        assert!(
            !credentials
                .has(&first_reference)
                .expect("old reference lookup"),
            "the replaced secret must be reclaimed"
        );
        assert_eq!(
            credentials
                .get(&rotated_reference)
                .expect("rotated secret")
                .as_utf8()
                .expect("UTF-8 secret"),
            "rotated-secret"
        );
        assert!(credentials.has(&second_reference).expect("preserved"));

        let mut cleared = save_input("mcp_http", "http", true);
        cleared.url = Some(url);
        let cleared = save(&db, &credentials, cleared).expect("clear authentication");
        assert!(cleared.http_auth_headers.is_empty());
        assert!(
            !credentials
                .has(&rotated_reference)
                .expect("rotated reference lookup"),
            "clearing the headers must reclaim rotated credentials"
        );
        assert!(
            !credentials
                .has(&second_reference)
                .expect("preserved reference lookup"),
            "clearing the headers must reclaim every credential"
        );
        cleanup(&path);
    }

    /// A form payload for one OAuth row, with the client-authentication choice explicit.
    fn oauth_save_input(
        client_auth: Option<McpOAuthClientAuth>,
        client_secret: Option<&str>,
        scopes: &[&str],
    ) -> McpServerSaveInput {
        let mut input = save_input("mcp_oauth", "http", true);
        input.url = Some("https://mcp.example.com/mcp".to_string());
        input.oauth = Some(McpOAuthInput {
            client_auth,
            client_secret: client_secret.map(str::to_string),
            ..oauth_input("https://auth.example.com", "confidential-client", scopes)
        });
        input
    }

    #[test]
    fn oauth_client_secret_is_write_only_preserved_rotated_and_reclaimed() {
        let (path, db) = temp_db("oauth-client-secret");
        let credentials = MemoryCredentialStore::new();

        // 1. Configure `client_secret_post` with a secret.
        let saved = save(
            &db,
            &credentials,
            oauth_save_input(
                Some(McpOAuthClientAuth::ClientSecretPost),
                Some("first-secret"),
                &["mcp.read"],
            ),
        )
        .expect("save the client secret");
        let oauth = saved.oauth.as_ref().expect("OAuth config");
        assert_eq!(oauth.client_auth, McpOAuthClientAuth::ClientSecretPost);
        let first = CredentialRef::parse(oauth.client_secret_ref.as_deref().expect("secret ref"))
            .expect("credential reference");
        assert_eq!(
            credentials
                .get(&first)
                .expect("stored secret")
                .as_utf8()
                .expect("UTF-8 secret"),
            "first-secret"
        );

        // The settings response carries the reference and nothing else: the secret has no
        // field to travel in, and no JSON rendering may contain it.
        let rendered = serde_json::to_string(&saved).expect("serialize the stored row");
        assert!(!rendered.contains("first-secret"), "{rendered}");
        assert!(rendered.contains("clientSecretRef"), "{rendered}");
        let debugged = format!(
            "{:?}",
            oauth_save_input(
                Some(McpOAuthClientAuth::ClientSecretPost),
                Some("first-secret"),
                &["mcp.read"],
            )
        );
        assert!(!debugged.contains("first-secret"), "{debugged}");
        assert!(debugged.contains("<redacted>"), "{debugged}");

        // 2. An edit that carries the method but no new secret keeps the stored one.
        let preserved = save(
            &db,
            &credentials,
            oauth_save_input(
                Some(McpOAuthClientAuth::ClientSecretPost),
                None,
                &["mcp.read"],
            ),
        )
        .expect("preserve the client secret");
        assert_eq!(
            preserved
                .oauth
                .as_ref()
                .and_then(|oauth| oauth.client_secret_ref.as_deref()),
            Some(first.to_string_ref().as_str())
        );
        assert!(credentials.has(&first).expect("still stored"));

        // 3. Switching the method without entering a secret is refused: the old secret is
        //    not carried across methods, and it is not silently reused either.
        let error = save(
            &db,
            &credentials,
            oauth_save_input(
                Some(McpOAuthClientAuth::ClientSecretBasic),
                None,
                &["mcp.read"],
            ),
        )
        .expect_err("a method switch needs the secret again");
        assert!(error.contains("client secret"), "{error}");
        assert!(
            credentials.has(&first).expect("lookup"),
            "a refused save must not have touched the stored secret"
        );

        // 4. Switching the method *with* a secret rotates it and reclaims the old value.
        let switched = save(
            &db,
            &credentials,
            oauth_save_input(
                Some(McpOAuthClientAuth::ClientSecretBasic),
                Some("second-secret"),
                &["mcp.read"],
            ),
        )
        .expect("switch to client_secret_basic");
        let second = CredentialRef::parse(
            switched
                .oauth
                .as_ref()
                .and_then(|oauth| oauth.client_secret_ref.as_deref())
                .expect("new secret reference"),
        )
        .expect("credential reference");
        assert_ne!(second, first);
        assert!(
            !credentials.has(&first).expect("old secret lookup"),
            "the replaced secret must be reclaimed"
        );
        assert_eq!(
            credentials
                .get(&second)
                .expect("rotated secret")
                .as_utf8()
                .expect("UTF-8 secret"),
            "second-secret"
        );

        // 5. The stored row survives a reopen with only a reference, and the database files
        //    contain neither secret.
        drop(db);
        let reopened = Database::open(&path).expect("reopen");
        let row = reopened.mcp_servers().get("mcp_oauth").expect("stored row");
        let oauth = row.oauth.as_ref().expect("OAuth config");
        assert_eq!(oauth.client_auth, McpOAuthClientAuth::ClientSecretBasic);
        assert_eq!(
            oauth.client_secret_ref.as_deref(),
            Some(second.to_string_ref().as_str())
        );
        for suffix in ["", "-wal", "-shm"] {
            let file = PathBuf::from(format!("{}{suffix}", path.display()));
            let Ok(bytes) = std::fs::read(&file) else {
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            assert!(
                !text.contains("first-secret") && !text.contains("second-secret"),
                "{} must not contain a client secret",
                file.display()
            );
        }

        // 6. Going back to a public client reclaims the secret as well.
        let public = save(
            &reopened,
            &credentials,
            oauth_save_input(None, None, &["mcp.read"]),
        )
        .expect("switch back to a public client");
        let oauth = public.oauth.as_ref().expect("OAuth config");
        assert_eq!(oauth.client_auth, McpOAuthClientAuth::None);
        assert!(oauth.client_secret_ref.is_none());
        assert!(
            !credentials.has(&second).expect("secret lookup"),
            "a public client must not leave an orphaned secret behind"
        );
        cleanup(&path);
    }

    #[test]
    fn a_client_secret_method_needs_a_client_id_and_a_bounded_secret() {
        let (path, db) = temp_db("oauth-client-secret-bounds");
        let credentials = MemoryCredentialStore::new();

        // A dynamically registered public client cannot be turned confidential afterwards.
        let mut anonymous = oauth_save_input(
            Some(McpOAuthClientAuth::ClientSecretPost),
            Some("s3cret"),
            &["mcp.read"],
        );
        anonymous.oauth = Some(McpOAuthInput {
            client_auth: Some(McpOAuthClientAuth::ClientSecretPost),
            client_secret: Some("s3cret".to_string()),
            ..oauth_input("https://auth.example.com", "", &["mcp.read"])
        });
        let error = save(&db, &credentials, anonymous).expect_err("must refuse");
        assert!(error.contains("client id"), "{error}");

        // RFC 7617 splits the header on the first colon, so a colon in the client id would
        // silently change the credentials that are sent.
        let mut colon = oauth_save_input(
            Some(McpOAuthClientAuth::ClientSecretBasic),
            Some("s3cret"),
            &["mcp.read"],
        );
        colon.oauth = Some(McpOAuthInput {
            client_auth: Some(McpOAuthClientAuth::ClientSecretBasic),
            client_secret: Some("s3cret".to_string()),
            ..oauth_input("https://auth.example.com", "cli:ent", &["mcp.read"])
        });
        let error = save(&db, &credentials, colon).expect_err("must refuse");
        assert!(error.contains("冒号"), "{error}");

        let oversized = oauth_save_input(
            Some(McpOAuthClientAuth::ClientSecretPost),
            Some(&"x".repeat(8_193)),
            &["mcp.read"],
        );
        let error = save(&db, &credentials, oversized).expect_err("must refuse");
        assert!(error.contains("8192"), "{error}");

        assert!(
            db.mcp_servers().get("mcp_oauth").is_err(),
            "no refused save may leave a row behind"
        );
        cleanup(&path);
    }

    #[test]
    fn oauth_accepts_an_empty_client_id_for_dynamic_registration() {
        let (path, db) = temp_db("oauth-dynamic-client");
        let credentials = MemoryCredentialStore::new();
        let mut input = save_input("mcp_oauth_dynamic", "http", true);
        input.url = Some("https://mcp.example.com/mcp".to_string());
        input.oauth = Some(oauth_input("", "", &["mcp.read"]));

        let saved = save(&db, &credentials, input).expect("save deferred registration");
        let oauth = saved.oauth.as_ref().expect("OAuth config");
        assert!(oauth.client_id.is_empty());
        assert!(oauth.token_endpoint.is_none());
        assert!(oauth.credential_ref.is_none());

        let unavailable = http_auth_config_error(&saved).expect("not connected yet");
        assert!(unavailable.to_string().contains("run Connect OAuth"));
        cleanup(&path);
    }

    #[test]
    fn toggling_dpop_is_an_identity_change_and_reclaims_the_old_key() {
        let (path, db) = temp_db("dpop-config");
        let credentials = MemoryCredentialStore::new();

        // 连上过一次的 DPoP 服务器：密钥与令牌都在凭据库里。
        let mut first = save_input("mcp_oauth", "http", true);
        first.url = Some("https://mcp.example.com/mcp".to_string());
        first.oauth = Some(McpOAuthInput {
            dpop: true,
            ..oauth_input("https://auth.example.com", "desktop-client", &["mcp.read"])
        });
        let saved = save(&db, &credentials, first).expect("save the DPoP configuration");
        let key_reference = credentials
            .set(
                "mcp",
                "dpop-key",
                &Secret::from_utf8("pkcs8-placeholder".to_string()),
            )
            .expect("store the key");
        let token_reference = credentials
            .set("mcp", "dpop-token", &Secret::from_utf8("{}".to_string()))
            .expect("store the token");
        let mut connected = saved;
        {
            let oauth = connected.oauth.as_mut().expect("OAuth config");
            oauth.dpop_key_ref = Some(key_reference.to_string_ref());
            oauth.token_endpoint = Some("https://auth.example.com/token".to_string());
            oauth.credential_ref = Some(token_reference.to_string_ref());
        }
        db.mcp_servers().upsert(&connected).expect("connect row");

        // 同一个身份再存一次：密钥引用原样保留，令牌也留着。
        let mut same = save_input("mcp_oauth", "http", true);
        same.url = Some("https://mcp.example.com/mcp".to_string());
        same.oauth = Some(McpOAuthInput {
            dpop: true,
            ..oauth_input("https://auth.example.com", "desktop-client", &["mcp.read"])
        });
        let preserved = save(&db, &credentials, same).expect("save the same identity");
        let oauth = preserved.oauth.as_ref().expect("OAuth config");
        assert_eq!(
            oauth.dpop_key_ref.as_deref(),
            Some(key_reference.to_string_ref().as_str())
        );
        assert_eq!(
            oauth.credential_ref.as_deref(),
            Some(token_reference.to_string_ref().as_str())
        );

        // 关掉 DPoP ＝ 换身份：令牌作废，旧的私钥也回收（它已经没有对手了）。
        let mut off = save_input("mcp_oauth", "http", true);
        off.url = Some("https://mcp.example.com/mcp".to_string());
        off.oauth = Some(oauth_input(
            "https://auth.example.com",
            "desktop-client",
            &["mcp.read"],
        ));
        let changed = save(&db, &credentials, off).expect("turn DPoP off");
        let oauth = changed.oauth.as_ref().expect("OAuth config");
        assert!(!oauth.dpop);
        assert!(oauth.dpop_key_ref.is_none());
        assert!(oauth.credential_ref.is_none());
        assert!(
            !credentials.has(&key_reference).expect("key lookup"),
            "an orphaned private key helps nobody"
        );
        cleanup(&path);
    }

    #[test]
    fn oauth_configuration_keeps_tokens_only_for_the_same_identity() {
        let (path, db) = temp_db("oauth-config");
        let credentials = MemoryCredentialStore::new();
        let mut first = save_input("mcp_oauth", "http", true);
        first.url = Some("https://mcp.example.com/mcp".to_string());
        first.oauth = Some(oauth_input(
            "https://auth.example.com",
            "desktop-client",
            &["mcp.read"],
        ));
        let saved = save(&db, &credentials, first).expect("save OAuth configuration");
        let oauth = saved.oauth.as_ref().expect("OAuth config");
        assert_eq!(oauth.issuer, "https://auth.example.com");
        assert_eq!(oauth.client_id, "desktop-client");
        assert_eq!(oauth.flow, McpOAuthFlow::AuthorizationCode);
        assert_eq!(oauth.scopes, vec!["mcp.read"]);
        assert!(oauth.token_endpoint.is_none());
        assert!(oauth.credential_ref.is_none());

        let reference = credentials
            .set(
                "mcp",
                "oauth-token",
                &Secret::from_utf8("{\"access_token\":\"secret\"}"),
            )
            .expect("store token");
        let mut connected = saved;
        let oauth = connected.oauth.as_mut().expect("OAuth config");
        oauth.token_endpoint = Some("https://auth.example.com/token".to_string());
        oauth.credential_ref = Some(reference.to_string_ref());
        db.mcp_servers().upsert(&connected).expect("connect row");

        let mut preserved = save_input("mcp_oauth", "http", true);
        preserved.url = Some("https://mcp.example.com/mcp".to_string());
        preserved.oauth = Some(oauth_input(
            "https://auth.example.com/",
            "desktop-client",
            &["mcp.read"],
        ));
        let preserved = save(&db, &credentials, preserved).expect("preserve OAuth token");
        assert_eq!(
            preserved
                .oauth
                .as_ref()
                .and_then(|oauth| oauth.credential_ref.as_deref()),
            Some(reference.to_string_ref().as_str())
        );
        assert!(credentials.has(&reference).expect("still stored"));

        let mut changed = save_input("mcp_oauth", "http", true);
        changed.url = Some("https://mcp.example.com/mcp".to_string());
        changed.oauth = Some(oauth_input(
            "https://auth.example.com",
            "desktop-client",
            &["mcp.write"],
        ));
        let changed = save(&db, &credentials, changed).expect("change OAuth identity");
        let oauth = changed.oauth.as_ref().expect("OAuth config");
        assert!(oauth.token_endpoint.is_none());
        assert!(oauth.credential_ref.is_none());
        assert!(
            !credentials.has(&reference).expect("old token lookup"),
            "changing the OAuth identity must reclaim the old token"
        );

        // Switching the flow is an identity change, not a display preference: the browser
        // flow's token was issued to a request the device flow would not have made.
        let flow_reference = credentials
            .set(
                "mcp",
                "oauth-token-flow",
                &Secret::from_utf8("{\"access_token\":\"secret\"}"),
            )
            .expect("store token for the flow switch");
        let mut reconnected = save_input("mcp_oauth", "http", true);
        reconnected.url = Some("https://mcp.example.com/mcp".to_string());
        reconnected.oauth = Some(oauth_input(
            "https://auth.example.com",
            "desktop-client",
            &["mcp.read"],
        ));
        let mut reconnected = save(&db, &credentials, reconnected).expect("restore the identity");
        let oauth = reconnected.oauth.as_mut().expect("OAuth config");
        oauth.token_endpoint = Some("https://auth.example.com/token".to_string());
        oauth.credential_ref = Some(flow_reference.to_string_ref());
        db.mcp_servers()
            .upsert(&reconnected)
            .expect("connect row again");

        let mut switched = save_input("mcp_oauth", "http", true);
        switched.url = Some("https://mcp.example.com/mcp".to_string());
        switched.oauth = Some(McpOAuthInput {
            flow: Some(McpOAuthFlow::DeviceCode),
            ..oauth_input("https://auth.example.com", "desktop-client", &["mcp.read"])
        });
        let switched = save(&db, &credentials, switched).expect("switch to the device code flow");
        let oauth = switched.oauth.as_ref().expect("OAuth config");
        assert_eq!(oauth.flow, McpOAuthFlow::DeviceCode);
        assert!(oauth.token_endpoint.is_none());
        assert!(oauth.credential_ref.is_none());
        assert!(
            !credentials.has(&flow_reference).expect("flow token lookup"),
            "switching the flow must reclaim the token the other flow obtained"
        );

        let mut mixed = save_input("mcp_oauth", "http", true);
        mixed.url = Some("https://mcp.example.com/mcp".to_string());
        mixed.http_auth_headers = Some(vec![auth_header("Authorization", Some("Bearer static"))]);
        mixed.oauth = Some(oauth_input(
            "https://auth.example.com",
            "desktop-client",
            &[],
        ));
        assert!(save(&db, &credentials, mixed).is_err());
        cleanup(&path);
    }

    #[tokio::test]
    async fn deleting_a_server_reclaims_its_oauth_token() {
        let (path, db) = temp_db("oauth-delete");
        let credentials = MemoryCredentialStore::new();
        let mut input = save_input("mcp_oauth", "http", true);
        input.url = Some("https://mcp.example.com/mcp".to_string());
        input.oauth = Some(oauth_input(
            "https://auth.example.com",
            "desktop-client",
            &[],
        ));
        let mut saved = save(&db, &credentials, input).expect("save OAuth row");
        let reference = credentials
            .set("mcp", "oauth-delete-token", &Secret::from_utf8("token"))
            .expect("store token");
        let oauth = saved.oauth.as_mut().expect("OAuth config");
        oauth.token_endpoint = Some("https://auth.example.com/token".to_string());
        oauth.credential_ref = Some(reference.to_string_ref());
        db.mcp_servers().upsert(&saved).expect("update row");

        let supervisor = McpSupervisor::new();
        delete_server(&db, &supervisor, &credentials, "mcp_oauth")
            .await
            .expect("delete");
        assert!(!credentials.has(&reference).expect("token lookup"));
        cleanup(&path);
    }

    #[tokio::test]
    async fn deleting_a_server_reclaims_its_http_credential() {
        let (path, db) = temp_db("http-auth-delete");
        let credentials = MemoryCredentialStore::new();
        let mut input = save_input("mcp_http", "http", true);
        input.url = Some("https://mcp.example.com/mcp".to_string());
        input.http_auth_headers = Some(vec![
            auth_header("Authorization", Some("Bearer delete-me")),
            auth_header("X-API-Key", Some("delete-me-too")),
        ]);
        let saved = save(&db, &credentials, input).expect("save authenticated HTTP row");
        let references = saved
            .http_auth_headers
            .iter()
            .map(|header| CredentialRef::parse(&header.credential_ref).expect("reference"))
            .collect::<Vec<_>>();

        let supervisor = McpSupervisor::new();
        let deleted = delete_server(&db, &supervisor, &credentials, "mcp_http")
            .await
            .expect("delete");
        assert!(deleted.deleted);
        assert!(!deleted.stopped);
        assert!(matches!(
            db.mcp_servers().get("mcp_http"),
            Err(DatabaseError::NotFound)
        ));
        for reference in references {
            assert!(
                !credentials.has(&reference).expect("credential lookup"),
                "deleting the server must reclaim every secret"
            );
        }
        cleanup(&path);
    }

    #[test]
    fn an_unknown_transport_and_an_impossible_id_are_refused() {
        let (path, db) = temp_db("transport-refused");
        let credentials = MemoryCredentialStore::new();
        let error =
            save(&db, &credentials, save_input("mcp_sse", "sse", true)).expect_err("must refuse");
        assert!(error.contains("unsupported transport"), "{error}");

        // `mcp_1` 规范化成 `mcp-1`，合法；`mcp..1` 永远变不成名字段。
        let error =
            save(&db, &credentials, save_input("mcp..1", "stdio", true)).expect_err("must refuse");
        assert!(error.contains("mcp..1"), "{error}");
        assert!(save(&db, &credentials, save_input("mcp_1", "stdio", true)).is_ok());
        cleanup(&path);
    }

    #[test]
    fn a_draft_row_is_saveable_but_a_blank_id_or_label_is_not() {
        let (path, db) = temp_db("draft");
        let credentials = MemoryCredentialStore::new();
        // 草稿：禁用 + 没有 command。可以保存。
        let mut draft = save_input("mcp_draft", "stdio", false);
        draft.command = None;
        assert!(
            save(&db, &credentials, draft).is_ok(),
            "a draft is a legitimate row"
        );

        let mut blank = save_input("", "stdio", true);
        blank.label = "x".into();
        assert!(save(&db, &credentials, blank).is_err());

        let mut unlabelled = save_input("mcp_1", "stdio", true);
        unlabelled.label = "   ".into();
        assert!(save(&db, &credentials, unlabelled).is_err());
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

    /* ── 表里的无效 HTTP 行 ───────────────────────────────────────────────── */

    /// 表里有一行绕过保存校验的远程明文 HTTP 时，目录与视图都必须说出来，且不能发起网络请求。
    #[tokio::test]
    async fn an_invalid_http_row_in_the_table_refuses_with_the_same_reason() {
        let (path, db) = temp_db("http-row");
        let mut http = fixture_row("mcp_http", "ok", true);
        http.transport = "http".to_string();
        http.command = None;
        http.args = None;
        http.url = Some("http://mcp.example.com/mcp".to_string());
        insert(&db, &http);

        let supervisor = McpSupervisor::new();
        let credentials = MemoryCredentialStore::new();
        let response = catalog(&db, &supervisor, Arc::new(credentials))
            .await
            .expect("catalog never fails");

        assert!(
            response.servers.is_empty(),
            "an invalid HTTP endpoint must not appear as usable"
        );
        assert_eq!(response.failures.len(), 1, "{:?}", response.failures);
        let failure = &response.failures[0];
        assert_eq!(failure.code, McpFailureCode::InvalidConfig);
        assert_eq!(failure.server_id, "mcp_http");
        assert!(
            failure.message.contains("HTTPS"),
            "the reason must say what endpoint rule was broken: {}",
            failure.message
        );

        // 视图（列表用的那个函数）说同一件事。
        let view = view(&supervisor, http).await;
        assert!(!view.status.running);
        let unavailable = view.unavailable.expect("a reason must be present");
        assert_eq!(unavailable.code, McpFailureCode::InvalidConfig);
        assert!(supervisor.servers().await.is_empty());
        cleanup(&path);
    }

    /// `tools/call` 的结果被**搬运**，不被解释：工具自己报错时 `isError` 原样带出去，宿主仍然
    /// 回 `success`（翻译成哪一类失败是适配器的事）。
    #[tokio::test]
    async fn an_mcp_tool_name_is_routed_to_its_server_and_errors_are_not_translated_here() {
        let Some((path, db, supervisor)) = fixture_setup("execute", "ok", "mcp_1") else {
            return;
        };
        let credentials = MemoryCredentialStore::new();
        catalog(&db, &supervisor, Arc::new(credentials))
            .await
            .expect("catalog");
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

    /// A crash is reported, then recovered with a fresh process. The interrupted call is not
    /// replayed, and the exit record remains visible after recovery.
    #[tokio::test]
    async fn a_crashed_server_is_recovered_without_hiding_its_exit_code() {
        let Some((path, db, supervisor)) = fixture_setup("crash", "misbehave", "mcp_1") else {
            return;
        };
        let cancel = CancellationToken::new();
        let credentials = MemoryCredentialStore::new();
        let first = catalog(&db, &supervisor, Arc::new(credentials))
            .await
            .expect("catalog");
        assert_eq!(first.servers.len(), 1);
        let killed_pid = supervisor
            .handle("mcp_1")
            .await
            .expect("tracked")
            .info()
            .pid
            .expect("stdio pid");

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

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let status = loop {
            let status = supervisor.status("mcp_1").await;
            if status.running && status.pid != Some(killed_pid) {
                break status;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the crash must be recovered within the published budget"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };
        assert_eq!(
            status.last_exit.as_ref().and_then(|exit| exit.code),
            Some(7),
            "recovery must keep the crash visible"
        );
        assert_eq!(
            status.restart.as_ref().map(|restart| restart.attempt),
            Some(1)
        );
        assert!(!status.restart.as_ref().expect("restart record").exhausted);

        let recovered = execute(
            &supervisor,
            "mcp.mcp-1.echo",
            &json!({ "text": "after recovery" }),
            &cancel,
        )
        .await
        .expect("host response");
        assert_eq!(recovered["status"], json!("success"));
        assert_eq!(recovered["output"]["text"], json!("echo: after recovery"));

        supervisor.shutdown("mcp_1").await;
        cleanup(&path);
    }

    /// 取消：宿主不再等，而且如实说这是取消（不是工具的失败）。
    #[tokio::test]
    async fn a_cancelled_call_stops_waiting_and_says_so() {
        let Some((path, db, supervisor)) = fixture_setup("cancel", "misbehave", "mcp_1") else {
            return;
        };
        let credentials = MemoryCredentialStore::new();
        catalog(&db, &supervisor, Arc::new(credentials))
            .await
            .expect("catalog");

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
            message.contains("automatic recovery"),
            "要说明自动恢复的状态：{message}"
        );
    }

    /// 目录的预算：一个什么都不答、最后自己退 9 的服务进程不能让目录请求挂住（它是 sidecar
    /// 握手期间唯一会问的东西）。
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
