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
    catalog_with_credentials, effective_risk, McpContentBlock, McpCredentialResolver, McpError,
    McpFailureCode, McpHttpAuthHeader, McpOAuthSourceConfig, McpOAuthTokenSource, McpServerStatus,
    McpSupervisor, McpToolAnnotations, McpToolDescriptor, McpTransportConfig,
    DEFAULT_REQUEST_TIMEOUT, MCP_NAMESPACE,
};
use yukinal_credentials::{CredentialRef, CredentialStore, Secret};
use yukinal_database::models::{
    McpAnnotationTrust, McpHttpAuthHeaderConfig, McpOAuthClientAuth, McpOAuthConfig, McpOAuthFlow,
    McpServerConfig,
};
use yukinal_database::{Database, DatabaseError};
use yukinal_net::OutboundProxy;

use crate::commands::host::{cancelled_failure, failed, success};
use crate::commands::server::next_id;
use crate::state::AppState;

mod configuration;
mod dpop;
mod oauth;

use configuration::*;
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
    /// Trust this server's own tool annotations (ADR 0074). Absent preserves the stored
    /// value; a new server defaults to `none` (annotations are not evidence).
    #[serde(default)]
    pub annotation_trust: Option<McpAnnotationTrust>,
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
    /// 是否信任这台服务器自述的工具注解（ADR 0074）。缺省保留原值。
    #[serde(default)]
    pub annotation_trust: Option<McpAnnotationTrust>,
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
        crate::state::credential_cleanup::reclaim(database, credentials, &reference)
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
        crate::state::credential_cleanup::reclaim(database, credentials, &reference).map_err(
            |error| format!("MCP 服务器 `{server_id}` 已删除，但 OAuth 凭据回收失败：{error}"),
        )?;
    }
    if let Some(reference) = row
        .oauth
        .as_ref()
        .and_then(|oauth| oauth.client_secret_ref.as_deref())
    {
        let reference = CredentialRef::parse(reference).map_err(|error| {
            format!("MCP 服务器 `{server_id}` 已删除，但其 OAuth client secret 引用无效：{error}")
        })?;
        crate::state::credential_cleanup::reclaim(database, credentials, &reference).map_err(
            |error| {
                format!("MCP 服务器 `{server_id}` 已删除，但 OAuth client secret 回收失败：{error}")
            },
        )?;
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
        crate::state::credential_cleanup::reclaim(database, credentials, &reference).map_err(
            |error| format!("MCP 服务器 `{server_id}` 已删除，但 DPoP 密钥回收失败：{error}"),
        )?;
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
    let row = save_review(
        &state.database,
        &state.mcp,
        state.credentials.clone(),
        input,
    )
    .await?;
    Ok(view(&state.mcp, row).await)
}

async fn save_review(
    database: &Database,
    supervisor: &McpSupervisor,
    credentials: Arc<dyn CredentialStore>,
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
    let previous_annotation_trust = row.annotation_trust;
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
    if let Some(annotation_trust) = input.annotation_trust {
        row.annotation_trust = annotation_trust;
    }
    database
        .mcp_servers()
        .upsert(&row)
        .map_err(|error| format!("保存 MCP 工具审核失败：{error}"))?;
    if status.running && previous_annotation_trust != row.annotation_trust {
        // The running process owns the old `tools/list` snapshot. Rebuild it before returning
        // so the next catalog request cannot observe a stale risk mapping after the user flips
        // the trust switch.
        supervisor.shutdown(&row.id).await;
        let mut config = McpTransportConfig::from_server_config(&row, DEFAULT_REQUEST_TIMEOUT)
            .map_err(|error| format!("MCP 注解信任已保存，但重新启动服务器失败：{error}"))?;
        apply_http_auth(&row, &mut config, database, credentials)
            .map_err(|error| format!("MCP 注解信任已保存，但重新启动服务器失败：{error}"))?;
        supervisor
            .start_transport(&config)
            .await
            .map_err(|error| format!("MCP 注解信任已保存，但重新启动服务器失败：{error}"))?;
    }
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

/// 宿主对一次 MCP 工具调用的有效风险判定。
///
/// 返回 `None` 表示「无法判定」——服务器没跑、名字对不上、行读不出来等等。调用方**必须**
/// 按 effectful 处理（失败关闭），绝不能因为判不出来就当它是只读的。
///
/// 这是唯一允许把服务器注解变成档位的地方；sidecar 只读结果，不自己看注解（ADR 0074）。
pub(crate) async fn tool_effective_risk(state: &AppState, tool_name: &str) -> Option<&'static str> {
    let (segment, tool_segment) = split_mcp_tool_name(tool_name)?;
    let server_id = server_for_segment(&state.mcp, segment).await.ok()?;
    let row = state.database.mcp_servers().get(&server_id).ok()?;
    let annotations = if row.annotation_trust == McpAnnotationTrust::Trusted {
        let handle = state.mcp.handle(&server_id).await?;
        if !handle.is_running() {
            return None;
        }
        handle
            .tools()
            .into_iter()
            .find(|tool| tool.name == tool_segment)
            .map(|tool| tool.annotations)?
    } else {
        McpToolAnnotations::default()
    };
    Some(effective_risk(row.annotation_trust, &annotations))
}

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
    let annotation_trust = input
        .annotation_trust
        .or_else(|| existing.as_ref().map(|row| row.annotation_trust))
        .unwrap_or_default();
    let requested_headers = input.http_auth_headers.unwrap_or_default();
    if !is_http && !requested_headers.is_empty() {
        return Err("HTTP authentication settings require the HTTP transport".to_string());
    }
    let existing_headers = existing
        .as_ref()
        .map(|row| row.http_auth_headers.as_slice())
        .unwrap_or_default();
    let (http_auth_headers, newly_staged) = resolve_http_auth_headers(
        database,
        id,
        existing_headers,
        requested_headers,
        credentials,
    )?;
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
            delete_credentials(database, credentials, &newly_staged);
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
        annotation_trust,
    };

    if let Err(error) = check_transport(&config) {
        delete_credentials(database, credentials, &newly_staged);
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
        delete_credentials(database, credentials, &newly_staged);
        return Err(format!("保存 MCP 服务器 `{id}` 失败：{error}"));
    }

    for old_ref in previous_refs {
        if !config
            .http_auth_headers
            .iter()
            .any(|header| header.credential_ref == old_ref)
        {
            if let Ok(reference) = CredentialRef::parse(&old_ref) {
                if let Err(error) =
                    crate::state::credential_cleanup::reclaim(database, credentials, &reference)
                {
                    tracing::warn!(
                        server_id = %id,
                        "saved MCP HTTP authentication but could not immediately reclaim its previous credential: {error}"
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
                if let Err(error) =
                    crate::state::credential_cleanup::reclaim(database, credentials, &reference)
                {
                    tracing::warn!(
                        server_id = %id,
                        "saved MCP OAuth configuration but could not immediately reclaim its previous token: {error}"
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
                if let Err(error) =
                    crate::state::credential_cleanup::reclaim(database, credentials, &reference)
                {
                    tracing::warn!(
                        server_id = %id,
                        "saved MCP OAuth configuration but could not immediately reclaim its previous client secret: {error}"
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
                if let Err(error) =
                    crate::state::credential_cleanup::reclaim(database, credentials, &reference)
                {
                    tracing::warn!(
                        server_id = %id,
                        "saved MCP OAuth configuration but could not immediately reclaim its previous DPoP key: {error}"
                    );
                }
            }
        }
    }
    Ok(config)
}

#[cfg(test)]
mod tests;
