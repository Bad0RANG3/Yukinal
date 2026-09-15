//! Streamable HTTP transport for one MCP endpoint.
//!
//! This follows the transport shape implemented by the repository's pinned MCP SDK:
//! every JSON-RPC frame is POSTed to one endpoint, the response is either JSON or an
//! SSE stream, an optional long-lived GET stream carries server messages, and a session
//! created during `initialize` is carried in `Mcp-Session-Id` and terminated with DELETE.
//!
//! Redirects are disabled and the endpoint is validated before a client is built:
//! remote endpoints require HTTPS, while plain HTTP is limited to loopback hosts.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Once;
use std::sync::{
    Arc, Mutex as StdMutex, MutexGuard as StdMutexGuard, PoisonError, Weak as StdWeak,
};
use std::time::Duration;

use reqwest::header::{ACCEPT, CONTENT_TYPE};
use reqwest::{Client, RequestBuilder, Response, StatusCode, Url};
use serde_json::{json, Value};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::config::McpHttpConfig;
use super::descriptor::{McpExitRecord, McpToolDescriptor, McpToolResult};
use super::error::McpError;
use super::handle::{McpServerInfo, ShutdownReport};
use super::oauth::{McpAuthScheme, McpAuthorizationRequest};
use super::wire::{self, McpInitialize};
use super::{redacted, truncated};

const SESSION_HEADER: &str = "mcp-session-id";
const PROTOCOL_HEADER: &str = "mcp-protocol-version";
/// RFC 9449 §4.2: the header carrying one request's proof.
const DPOP_HEADER: &str = "dpop";
/// RFC 9449 §8: the header a server uses to hand out its current nonce.
const DPOP_NONCE_HEADER: &str = "dpop-nonce";
const DIAGNOSTIC_TAIL_LINES: usize = 100;
const MAX_SESSION_ID_BYTES: usize = 1_024;
const MAX_HTTP_ERROR_BYTES: usize = 64 * 1024;
const GET_START_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
const TOOLS_LIST_MAX_PAGES: usize = 16;
/// How many times one request may be sent while authenticating.
///
/// Three, in this order: as-is, with the DPoP nonce the server just challenged with, and
/// with a forcibly refreshed token. Any fewer and a server that both rotates its nonce and
/// expires tokens would fail; any more and a misbehaving server becomes a retry loop.
const AUTH_ATTEMPTS: usize = 3;

struct PendingHttp {
    sender: oneshot::Sender<Result<Value, McpError>>,
    method: String,
}

struct HttpInner {
    config: McpHttpConfig,
    endpoint: Url,
    client: Client,
    started_at: String,
    handshake: StdMutex<Option<McpInitialize>>,
    session_id: StdMutex<Option<String>>,
    protocol_version: StdMutex<Option<String>>,
    pending: StdMutex<HashMap<i64, PendingHttp>>,
    tools: StdMutex<Vec<McpToolDescriptor>>,
    exit: StdMutex<Option<McpExitRecord>>,
    diagnostics: StdMutex<VecDeque<String>>,
    next_id: AtomicI64,
    running: AtomicBool,
    shutdown: CancellationToken,
    get_task: StdMutex<Option<JoinHandle<()>>>,
}

/// A live Streamable HTTP MCP session.
#[derive(Clone)]
pub struct McpHttpHandle {
    inner: Arc<HttpInner>,
}

impl std::fmt::Debug for McpHttpHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHttpHandle")
            .field("server_id", &self.server_id())
            .field("running", &self.is_running())
            .finish()
    }
}

impl McpHttpHandle {
    pub fn new(config: &McpHttpConfig) -> Result<Self, McpError> {
        config.validate()?;
        install_crypto_provider();
        let endpoint = Url::parse(&config.url).map_err(|error| McpError::InvalidUrl {
            server_id: truncated(&config.server_id),
            reason: truncated(&error.to_string()),
        })?;
        let builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10));
        // 代理在这里落地：直连会被明确设成 `no_proxy()`，而读到了却用不了的配置
        // （只配了 PAC、代理 URL 非法）在这里就失败 —— 不静默改成直连（ADR 0022）。
        let builder = yukinal_net::apply(builder, &config.proxy, config.proxy_credential.as_ref())
            .map_err(|reason| McpError::InvalidConfig {
                server_id: truncated(&config.server_id),
                reason: truncated(&reason),
            })?;
        let client = builder.build().map_err(|error| McpError::Http {
            server_id: truncated(&config.server_id),
            method: "client setup".to_string(),
            reason: truncated(&error.without_url().to_string()),
        })?;
        Ok(Self {
            inner: Arc::new(HttpInner {
                config: config.clone(),
                endpoint,
                client,
                started_at: yukinal_time::iso8601_now(),
                handshake: StdMutex::new(None),
                session_id: StdMutex::new(None),
                protocol_version: StdMutex::new(None),
                pending: StdMutex::new(HashMap::new()),
                tools: StdMutex::new(Vec::new()),
                exit: StdMutex::new(None),
                diagnostics: StdMutex::new(VecDeque::new()),
                next_id: AtomicI64::new(1),
                running: AtomicBool::new(true),
                shutdown: CancellationToken::new(),
                get_task: StdMutex::new(None),
            }),
        })
    }

    #[must_use]
    pub fn server_id(&self) -> &str {
        &self.inner.config.server_id
    }

    #[must_use]
    pub fn segment(&self) -> &str {
        &self.inner.config.segment
    }

    #[must_use]
    pub fn request_timeout(&self) -> Duration {
        self.inner.config.request_timeout
    }

    #[must_use]
    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn info(&self) -> McpServerInfo {
        McpServerInfo {
            server_id: self.server_id().to_string(),
            pid: None,
            program: None,
            started_at: self.inner.started_at.clone(),
            handshake: self.handshake(),
        }
    }

    #[must_use]
    pub fn handshake(&self) -> Option<McpInitialize> {
        lock_or_recover(&self.inner.handshake).clone()
    }

    #[must_use]
    pub fn tools(&self) -> Vec<McpToolDescriptor> {
        lock_or_recover(&self.inner.tools).clone()
    }

    #[must_use]
    pub fn last_exit(&self) -> Option<McpExitRecord> {
        lock_or_recover(&self.inner.exit).clone()
    }

    #[must_use]
    pub fn stderr_tail(&self) -> Vec<String> {
        Vec::new()
    }

    #[must_use]
    pub fn diagnostics(&self) -> Vec<String> {
        lock_or_recover(&self.inner.diagnostics)
            .iter()
            .cloned()
            .collect()
    }

    pub async fn initialize(&self, timeout: Duration) -> Result<McpInitialize, McpError> {
        let params = json!({
            "protocolVersion": wire::PREFERRED_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": wire::CLIENT_NAME, "version": wire::CLIENT_VERSION },
        });
        let result = self
            .request(wire::METHOD_INITIALIZE, params, timeout)
            .await?;
        let handshake = wire::parse_initialize(self.server_id(), &result)?;
        *lock_or_recover(&self.inner.protocol_version) = Some(handshake.protocol_version.clone());
        self.notify(wire::METHOD_INITIALIZED_NOTIFICATION, json!({}))
            .await?;
        *lock_or_recover(&self.inner.handshake) = Some(handshake.clone());
        self.start_get_stream();
        Ok(handshake)
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        if let Some(exit) = self.last_exit() {
            return Err(self.exited_error(exit));
        }
        if !self.is_running() {
            return Err(self.not_running_error());
        }
        self.post_frame(
            &wire::notification_frame(method, params),
            None,
            self.request_timeout(),
        )
        .await
    }

    pub async fn list_tools(&self, timeout: Duration) -> Result<Vec<McpToolDescriptor>, McpError> {
        let mut tools: Vec<McpToolDescriptor> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut cursor: Option<String> = None;
        let mut pages = 0usize;

        loop {
            if pages >= TOOLS_LIST_MAX_PAGES {
                return Err(McpError::Protocol {
                    server_id: self.server_id().to_string(),
                    reason: format!(
                        "tools/list still answered with a cursor after {TOOLS_LIST_MAX_PAGES} pages"
                    ),
                });
            }
            pages += 1;

            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let result = self
                .request(wire::METHOD_TOOLS_LIST, params, timeout)
                .await?;
            let (page, next) = wire::parse_tools_page(self.server_id(), &result)?;
            for tool in page {
                if !seen.insert(tool.name.clone()) {
                    return Err(McpError::ToolNameCollision {
                        server_id: self.server_id().to_string(),
                        name: tool.name,
                    });
                }
                tools.push(tool);
            }

            match next {
                None => {
                    *lock_or_recover(&self.inner.tools) = tools.clone();
                    return Ok(tools);
                }
                Some(next) => cursor = Some(next),
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

    pub async fn call_tool_with_cancel(
        &self,
        tool: &str,
        arguments: Value,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<McpToolResult, McpError> {
        let descriptor = self
            .tools()
            .into_iter()
            .find(|descriptor| descriptor.name == tool)
            .ok_or_else(|| McpError::UnknownTool {
                server_id: self.server_id().to_string(),
                tool: truncated(tool),
            })?;
        if !arguments.is_object() {
            return Err(McpError::InvalidArguments {
                server_id: self.server_id().to_string(),
                tool: descriptor.name,
            });
        }

        let params = json!({ "name": descriptor.call_name(), "arguments": arguments });
        let result = self
            .request_with_cancel(wire::METHOD_TOOLS_CALL, params, timeout, cancel)
            .await?;
        wire::parse_tools_call(self.server_id(), &result)
    }

    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, McpError> {
        self.request_with_cancel(method, params, timeout, &CancellationToken::new())
            .await
    }

    pub async fn request_with_cancel(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<Value, McpError> {
        if let Some(exit) = self.last_exit() {
            return Err(self.exited_error(exit));
        }
        if !self.is_running() {
            return Err(self.not_running_error());
        }
        if cancel.is_cancelled() {
            return Err(McpError::Cancelled {
                server_id: self.server_id().to_string(),
                method: method.to_string(),
            });
        }

        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        lock_or_recover(&self.inner.pending).insert(
            id,
            PendingHttp {
                sender,
                method: method.to_string(),
            },
        );

        let send_and_wait = async {
            self.post_frame(&wire::request_frame(id, method, params), Some(id), timeout)
                .await?;
            receiver.await.map_err(|_| self.not_running_error())?
        };
        tokio::pin!(send_and_wait);

        let outcome = tokio::select! {
            _ = cancel.cancelled() => {
                self.forget(id);
                let _ = tokio::time::timeout(
                    Duration::from_secs(2),
                    self.notify(
                        wire::METHOD_CANCELLED_NOTIFICATION,
                        json!({
                            "requestId": id,
                            "reason": "cancelled by the Yukinal caller",
                        }),
                    ),
                )
                .await;
                return Err(McpError::Cancelled {
                    server_id: self.server_id().to_string(),
                    method: method.to_string(),
                });
            }
            result = tokio::time::timeout(timeout, &mut send_and_wait) => result,
        };

        match outcome {
            Err(_) => {
                self.forget(id);
                Err(McpError::Timeout {
                    server_id: self.server_id().to_string(),
                    method: method.to_string(),
                    timeout,
                })
            }
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => {
                self.forget(id);
                Err(error)
            }
        }
    }

    pub async fn shutdown(&self) -> ShutdownReport {
        let was_running = self.inner.running.swap(false, Ordering::Relaxed);
        self.inner.shutdown.cancel();
        self.inner.fail_pending(|method| McpError::Cancelled {
            server_id: self.server_id().to_string(),
            method,
        });

        let task = lock_or_recover(&self.inner.get_task).take();
        if let Some(task) = task {
            let _ = tokio::time::timeout(SHUTDOWN_GRACE, task).await;
        }

        if was_running && self.session_id().is_some() {
            let _ = self.delete_session().await;
        }
        ShutdownReport {
            was_running,
            killed: false,
            unreaped: false,
        }
    }

    fn not_running_error(&self) -> McpError {
        McpError::NotRunning {
            server_id: self.server_id().to_string(),
        }
    }

    fn exited_error(&self, exit: McpExitRecord) -> McpError {
        let reason = exit.reason();
        McpError::Exited {
            server_id: self.server_id().to_string(),
            code: exit.code,
            signal: exit.signal,
            reason,
        }
    }

    fn session_id(&self) -> Option<String> {
        lock_or_recover(&self.inner.session_id).clone()
    }

    fn protocol_version(&self) -> Option<String> {
        lock_or_recover(&self.inner.protocol_version).clone()
    }

    async fn post_frame(
        &self,
        frame: &Value,
        wait_for: Option<i64>,
        timeout: Duration,
    ) -> Result<(), McpError> {
        let method = frame
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("JSON-RPC response");
        let payload = encode_json_frame(self.server_id(), frame)?;
        let response = self
            .inner
            .send_authenticated(method, || {
                let mut request = self
                    .inner
                    .client
                    .post(self.inner.endpoint.clone())
                    .header(CONTENT_TYPE, "application/json")
                    .header(ACCEPT, "application/json, text/event-stream")
                    .timeout(timeout)
                    .body(payload.clone());
                if let Some(session_id) = self.session_id() {
                    request = request.header(SESSION_HEADER, session_id);
                }
                if let Some(version) = self.protocol_version() {
                    request = request.header(PROTOCOL_HEADER, version);
                }
                request
            })
            .await?;
        self.capture_session_id(&response)?;
        let status = response.status();

        if status == StatusCode::NOT_FOUND && self.session_id().is_some() {
            let reason = format!("HTTP {status}: the MCP session is no longer known");
            self.inner.expire_session(&reason);
            return Err(self.http_error(method, Some(status), reason));
        }
        if !status.is_success() {
            let detail = match read_bounded_body(response, MAX_HTTP_ERROR_BYTES).await {
                Ok(body) => String::from_utf8_lossy(&body).into_owned(),
                Err(error) => error.to_string(),
            };
            return Err(self.http_error(
                method,
                Some(status),
                format!("HTTP {status}: {}", redacted(&truncated(&detail))),
            ));
        }

        if status == StatusCode::ACCEPTED || status == StatusCode::NO_CONTENT {
            return Ok(());
        }

        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| {
                value
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase()
            })
            .unwrap_or_default();
        match content_type.as_str() {
            "application/json" => {
                let body = read_bounded_body(response, wire::MAX_FRAME_BYTES)
                    .await
                    .map_err(|error| self.http_error(method, Some(status), error.to_string()))?;
                let payload: Value =
                    serde_json::from_slice(&body).map_err(|error| McpError::Protocol {
                        server_id: self.server_id().to_string(),
                        reason: format!("HTTP JSON response is invalid: {error}"),
                    })?;
                self.inner.dispatch_payload(&payload);
                Ok(())
            }
            "text/event-stream" => {
                self.spawn_sse(response, wait_for);
                Ok(())
            }
            other if wait_for.is_none() && other.is_empty() => Ok(()),
            other => Err(McpError::Protocol {
                server_id: self.server_id().to_string(),
                reason: format!("HTTP response has unsupported content type \"{other}\""),
            }),
        }
    }

    fn capture_session_id(&self, response: &Response) -> Result<(), McpError> {
        let Some(value) = response.headers().get(SESSION_HEADER) else {
            return Ok(());
        };
        let value = value.to_str().map_err(|_| McpError::Protocol {
            server_id: self.server_id().to_string(),
            reason: "Mcp-Session-Id is not valid ASCII".to_string(),
        })?;
        if value.is_empty()
            || value.len() > MAX_SESSION_ID_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(McpError::Protocol {
                server_id: self.server_id().to_string(),
                reason: "Mcp-Session-Id is empty, too long, or contains control characters"
                    .to_string(),
            });
        }
        *lock_or_recover(&self.inner.session_id) = Some(value.to_string());
        Ok(())
    }

    fn start_get_stream(&self) {
        let weak = Arc::downgrade(&self.inner);
        let client = self.inner.client.clone();
        let endpoint = self.inner.endpoint.clone();
        let session_id = self.session_id();
        let protocol_version = self.protocol_version();
        let shutdown = self.inner.shutdown.clone();
        let task = tokio::spawn(async move {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let proxy = inner.config.proxy.clone();
            let response = tokio::select! {
                _ = shutdown.cancelled() => return,
                response = tokio::time::timeout(
                    GET_START_TIMEOUT,
                    inner.send_authenticated("GET", || {
                        let mut request = client
                            .get(endpoint.clone())
                            .header(ACCEPT, "text/event-stream");
                        if let Some(session_id) = &session_id {
                            request = request.header(SESSION_HEADER, session_id.clone());
                        }
                        if let Some(version) = &protocol_version {
                            request = request.header(PROTOCOL_HEADER, version.clone());
                        }
                        request
                    }),
                ) => response,
            };
            let response = match response {
                Err(_) => {
                    inner.remember_diagnostic(yukinal_net::route_context(
                        &proxy,
                        "timed out opening the optional MCP GET event stream",
                    ));
                    return;
                }
                Ok(Err(error)) => {
                    inner.remember_diagnostic(format!(
                        "could not open the optional MCP GET event stream: {error}"
                    ));
                    return;
                }
                Ok(Ok(response)) => response,
            };
            if response.status() == StatusCode::METHOD_NOT_ALLOWED {
                return;
            }
            if response.status() == StatusCode::NOT_FOUND && session_id.is_some() {
                let reason = "HTTP 404: the MCP session is no longer known";
                if let Some(inner) = weak.upgrade() {
                    inner.expire_session(reason);
                }
                return;
            }
            if !response.status().is_success() {
                if let Some(inner) = weak.upgrade() {
                    let reason = format!(
                        "optional MCP GET event stream returned HTTP {}",
                        response.status()
                    );
                    inner.remember_diagnostic(yukinal_net::route_context(&proxy, &reason));
                }
                return;
            }
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(|value| {
                    value
                        .split(';')
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .to_ascii_lowercase()
                });
            if content_type.as_deref() != Some("text/event-stream") {
                if let Some(inner) = weak.upgrade() {
                    inner.remember_diagnostic(
                        "optional MCP GET stream did not return text/event-stream".to_string(),
                    );
                }
                return;
            }
            consume_sse_stream(weak, response, None, shutdown, proxy).await;
        });
        if let Some(previous) = lock_or_recover(&self.inner.get_task).replace(task) {
            previous.abort();
        }
    }

    fn spawn_sse(&self, response: Response, wait_for: Option<i64>) {
        let weak = Arc::downgrade(&self.inner);
        let shutdown = self.inner.shutdown.clone();
        let proxy = self.inner.config.proxy.clone();
        tokio::spawn(async move {
            consume_sse_stream(weak, response, wait_for, shutdown, proxy).await;
        });
    }

    fn forget(&self, id: i64) {
        lock_or_recover(&self.inner.pending).remove(&id);
    }

    async fn delete_session(&self) -> Result<(), McpError> {
        let Some(session_id) = self.session_id() else {
            return Ok(());
        };
        let session = session_id.clone();
        let response = self
            .inner
            .send_authenticated("DELETE", || {
                let mut request = self
                    .inner
                    .client
                    .delete(self.inner.endpoint.clone())
                    .header(SESSION_HEADER, session.clone())
                    .timeout(SHUTDOWN_GRACE);
                if let Some(version) = self.protocol_version() {
                    request = request.header(PROTOCOL_HEADER, version);
                }
                request
            })
            .await?;
        if response.status().is_success()
            || response.status() == StatusCode::METHOD_NOT_ALLOWED
            || response.status() == StatusCode::NOT_FOUND
        {
            *lock_or_recover(&self.inner.session_id) = None;
            Ok(())
        } else {
            Err(self.http_error(
                "session termination",
                Some(response.status()),
                "the server refused to terminate the MCP session".to_string(),
            ))
        }
    }

    fn http_error(&self, method: &str, status: Option<StatusCode>, reason: String) -> McpError {
        let reason = redacted(&truncated(&reason));
        let reason = yukinal_net::route_context(&self.inner.config.proxy, &reason);
        McpError::Http {
            server_id: self.server_id().to_string(),
            method: truncated(method),
            reason: match status {
                Some(status) => format!("HTTP {status}: {reason}"),
                None => reason,
            },
        }
    }
}

fn install_crypto_provider() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        // `install_default` only fails when another provider won the process-wide slot.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// The nonce a server challenged this request with, if any.
///
/// Two spellings are in the field and both mean the same thing: the `DPoP-Nonce` header
/// (RFC 9449 §8) and a `nonce="…"` parameter on a `WWW-Authenticate: DPoP` challenge. The
/// value comes straight off the network, so it is length- and control-character-checked
/// here; anything that fails those checks counts as "no challenge" and the caller treats
/// the response as an ordinary `401`.
fn dpop_nonce(response: &Response) -> Option<String> {
    let from_header = response
        .headers()
        .get(DPOP_NONCE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let value = from_header.or_else(|| {
        let header = response
            .headers()
            .get(reqwest::header::WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok())?;
        if !header.trim_start().to_ascii_lowercase().starts_with("dpop") {
            return None;
        }
        challenge_parameter(header, "nonce")
    })?;
    let value = value.trim();
    if value.is_empty() || value.len() > 1_024 || value.chars().any(char::is_control) {
        return None;
    }
    Some(value.to_string())
}

/// One `auth-param` out of a `WWW-Authenticate` value (RFC 7235 §2.1).
fn challenge_parameter(header: &str, name: &str) -> Option<String> {
    let lowered = header.to_ascii_lowercase();
    let needle = format!("{name}=");
    let mut from = 0;
    while let Some(offset) = lowered[from..].find(&needle) {
        let at = from + offset;
        from = at + needle.len();
        // A parameter name must start at a boundary: `xnonce=` is not `nonce=`.
        if at > 0 {
            let previous = header.as_bytes()[at - 1];
            if previous.is_ascii_alphanumeric() || previous == b'-' || previous == b'_' {
                continue;
            }
        }
        let rest = header[from..].trim_start();
        if let Some(quoted) = rest.strip_prefix('"') {
            if let Some(end) = quoted.find('"') {
                return Some(quoted[..end].to_string());
            }
            continue;
        }
        let end = rest.find([',', ' ', '\t']).unwrap_or(rest.len());
        return Some(rest[..end].to_string());
    }
    None
}

fn encode_json_frame(server_id: &str, frame: &Value) -> Result<Vec<u8>, McpError> {
    let payload = serde_json::to_vec(frame).map_err(|error| McpError::Protocol {
        server_id: truncated(server_id),
        reason: format!("could not encode an HTTP JSON-RPC frame: {error}"),
    })?;
    if payload.len() > wire::MAX_FRAME_BYTES {
        return Err(McpError::Protocol {
            server_id: truncated(server_id),
            reason: format!(
                "HTTP JSON-RPC request exceeds the {}-byte frame limit",
                wire::MAX_FRAME_BYTES
            ),
        });
    }
    Ok(payload)
}

impl HttpInner {
    /// Send one authenticated request, retrying a `401` a bounded number of times.
    ///
    /// The order is: as-is, then with the DPoP nonce the server just challenged with, then
    /// with a forcibly refreshed token — at most [`AUTH_ATTEMPTS`] sends in total. The last
    /// response is handed back when none of them worked, so the caller still reports the
    /// server's own status instead of a synthesised "too many retries".
    async fn send_authenticated<F>(&self, method: &str, build: F) -> Result<Response, McpError>
    where
        F: Fn() -> RequestBuilder,
    {
        let mut nonce: Option<String> = None;
        let mut force_refresh = false;
        for _ in 0..AUTH_ATTEMPTS {
            let request = self
                .authenticated(
                    build(),
                    method,
                    &self.endpoint,
                    force_refresh,
                    nonce.clone(),
                )
                .await?;
            let response = request.send().await.map_err(|error| {
                let reason = error.without_url().to_string();
                McpError::Http {
                    server_id: self.config.server_id.clone(),
                    method: truncated(method),
                    // 代理环境里「连不上」必须说清是谁的问题：目标还是代理（ADR 0022 第 7 条）。
                    reason: redacted(&truncated(&yukinal_net::route_context(
                        &self.config.proxy,
                        &reason,
                    ))),
                }
            })?;
            if response.status() != StatusCode::UNAUTHORIZED || self.config.oauth.is_none() {
                return Ok(response);
            }
            // A DPoP nonce challenge: the token is fine, the proof needs the server's fresh
            // nonce. Retry once with it; a nonce we already tried counts as a failure.
            if let Some(challenge) = dpop_nonce(&response) {
                if nonce.as_deref() != Some(challenge.as_str()) {
                    nonce = Some(challenge);
                    continue;
                }
            }
            if !force_refresh {
                force_refresh = true;
                continue;
            }
            return Ok(response);
        }
        // Only reachable if the two retry branches above were both taken on the last
        // attempt, which the counters make impossible; the honest answer is still an error.
        Err(McpError::Http {
            server_id: self.config.server_id.clone(),
            method: truncated(method),
            reason: "the authenticated request produced no response".to_string(),
        })
    }

    async fn authenticated(
        &self,
        mut request: RequestBuilder,
        method: &str,
        url: &Url,
        force_refresh: bool,
        nonce: Option<String>,
    ) -> Result<RequestBuilder, McpError> {
        for auth in &self.config.auth_headers {
            request = request.header(auth.name(), auth.value());
        }
        if let Some(source) = &self.config.oauth {
            let authorization = source
                .authorization(McpAuthorizationRequest {
                    method: method.to_ascii_uppercase(),
                    url: url.as_str().to_string(),
                    force_refresh,
                    nonce,
                })
                .await
                .map_err(|reason| McpError::OAuth {
                    server_id: self.config.server_id.clone(),
                    reason: redacted(&truncated(&reason)),
                })?;
            let token = authorization.token;
            if token.is_empty()
                || token.len() > 8 * 1024
                || token
                    .bytes()
                    .any(|byte| byte == b'\r' || byte == b'\n' || byte == 0)
            {
                return Err(McpError::OAuth {
                    server_id: self.config.server_id.clone(),
                    reason: "the OAuth token source returned an empty, oversized, or unsafe token"
                        .to_string(),
                });
            }
            match authorization.scheme {
                McpAuthScheme::Bearer => {
                    request = request.bearer_auth(token);
                }
                McpAuthScheme::Dpop => {
                    let proof = authorization.proof.ok_or_else(|| McpError::OAuth {
                        server_id: self.config.server_id.clone(),
                        reason: "the OAuth token source returned a DPoP token without a proof"
                            .to_string(),
                    })?;
                    if proof.is_empty()
                        || proof.len() > 8 * 1024
                        || proof
                            .bytes()
                            .any(|byte| byte == b'\r' || byte == b'\n' || byte == 0)
                    {
                        return Err(McpError::OAuth {
                            server_id: self.config.server_id.clone(),
                            reason: "the OAuth token source returned an unsafe DPoP proof"
                                .to_string(),
                        });
                    }
                    // `bearer_auth` would spell the scheme `Bearer`; RFC 9449 §7.1 requires the
                    // token to be presented with the `DPoP` scheme, so the header is built by
                    // hand (the token itself is already checked for CR/LF/NUL above).
                    request = request
                        .header(reqwest::header::AUTHORIZATION, format!("DPoP {token}"))
                        .header(DPOP_HEADER, proof);
                }
            }
        }
        Ok(request)
    }

    fn expire_session(&self, reason: &str) {
        if self.running.swap(false, Ordering::Relaxed) {
            self.shutdown.cancel();
            self.fail_pending(|method| McpError::Http {
                server_id: self.config.server_id.clone(),
                method,
                reason: redacted(&truncated(reason)),
            });
        }
    }

    fn fail_pending(&self, error: impl Fn(String) -> McpError) {
        let mut pending = lock_or_recover(&self.pending);
        for (_, entry) in pending.drain() {
            let _ = entry.sender.send(Err(error(entry.method)));
        }
    }

    fn dispatch_payload(&self, payload: &Value) {
        if let Some(frames) = payload.as_array() {
            for frame in frames {
                self.dispatch_frame(frame);
            }
        } else {
            self.dispatch_frame(payload);
        }
    }

    fn dispatch_frame(&self, frame: &Value) {
        match wire::classify(frame) {
            wire::Incoming::Response { id } => match wire::parse_response(frame) {
                Ok(result) => self.resolve(id, Ok(result)),
                Err(error) => {
                    let pending = lock_or_recover(&self.pending).remove(&id);
                    if let Some(pending) = pending {
                        let _ = pending.sender.send(Err(McpError::Remote {
                            server_id: self.config.server_id.clone(),
                            code: error.code,
                            message: error.message,
                        }));
                    }
                }
            },
            wire::Incoming::ServerRequest { id, method } => {
                self.remember_diagnostic(format!(
                    "ignored a request from the HTTP server (id {id}, method \"{}\")",
                    truncated(&method)
                ));
            }
            wire::Incoming::Notification { method } => {
                self.remember_diagnostic(format!(
                    "ignored a notification from the HTTP server (\"{}\")",
                    truncated(&method)
                ));
            }
            wire::Incoming::Noise(reason) => self.remember_diagnostic(reason),
        }
    }

    fn resolve(&self, id: i64, outcome: Result<Value, McpError>) {
        let pending = lock_or_recover(&self.pending).remove(&id);
        if let Some(pending) = pending {
            let _ = pending.sender.send(outcome);
        } else {
            self.remember_diagnostic(format!(
                "HTTP server answered unknown JSON-RPC request id {id}"
            ));
        }
    }

    fn remember_diagnostic(&self, line: String) {
        let mut tail = lock_or_recover(&self.diagnostics);
        if tail.len() >= DIAGNOSTIC_TAIL_LINES {
            tail.pop_front();
        }
        tail.push_back(redacted(&truncated(&line)));
    }
}

async fn consume_sse_stream(
    weak: StdWeak<HttpInner>,
    mut response: Response,
    wait_for: Option<i64>,
    shutdown: CancellationToken,
    proxy: yukinal_net::NetworkProxy,
) {
    let mut decoder = SseDecoder::default();
    loop {
        let chunk = tokio::select! {
            _ = shutdown.cancelled() => return,
            chunk = response.chunk() => chunk,
        };
        let chunk = match chunk {
            Ok(Some(chunk)) => chunk,
            Ok(None) => {
                if let Some(inner) = weak.upgrade() {
                    match decoder.finish() {
                        Ok(events) => {
                            for event in events {
                                dispatch_sse_event(&inner, &event);
                            }
                        }
                        Err(reason) => {
                            inner.remember_diagnostic(format!(
                                "MCP SSE stream was rejected: {reason}"
                            ));
                        }
                    }
                }
                return;
            }
            Err(error) => {
                if let Some(inner) = weak.upgrade() {
                    let reason = error.without_url().to_string();
                    inner.remember_diagnostic(format!(
                        "MCP SSE stream ended with a transport error: {}",
                        yukinal_net::route_context(&proxy, &reason)
                    ));
                }
                return;
            }
        };
        let events = match decoder.push(&chunk) {
            Ok(events) => events,
            Err(reason) => {
                if let Some(inner) = weak.upgrade() {
                    inner.remember_diagnostic(format!("MCP SSE stream was rejected: {reason}"));
                }
                return;
            }
        };
        let Some(inner) = weak.upgrade() else {
            return;
        };
        for event in events {
            dispatch_sse_event(&inner, &event);
            if let Some(id) = wait_for {
                if !lock_or_recover(&inner.pending).contains_key(&id) {
                    return;
                }
            }
        }
    }
}

fn dispatch_sse_event(inner: &HttpInner, event: &str) {
    let payload: Value = match serde_json::from_str(event) {
        Ok(payload) => payload,
        Err(error) => {
            inner.remember_diagnostic(format!(
                "dropped invalid JSON in an MCP SSE event: {error}: {}",
                truncated(event)
            ));
            return;
        }
    };
    inner.dispatch_payload(&payload);
}

async fn read_bounded_body(mut response: Response, limit: usize) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(format!("HTTP response exceeds the {limit}-byte limit"));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| error.without_url().to_string())?
    {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(format!("HTTP response exceeds the {limit}-byte limit"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Default)]
struct SseDecoder {
    line: Vec<u8>,
    data: Vec<u8>,
    discarding: bool,
}

impl SseDecoder {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, String> {
        let mut events = Vec::new();
        for &byte in bytes {
            if byte == b'\n' {
                self.finish_line(&mut events)?;
            } else {
                self.line.push(byte);
                if self.line.len() > wire::MAX_FRAME_BYTES {
                    return Err(format!(
                        "one SSE line exceeds the {}-byte limit",
                        wire::MAX_FRAME_BYTES
                    ));
                }
            }
        }
        Ok(events)
    }

    fn finish(mut self) -> Result<Vec<String>, String> {
        let mut events = Vec::new();
        if !self.line.is_empty() {
            self.finish_line(&mut events)?;
        }
        if !self.discarding && !self.data.is_empty() {
            events.push(String::from_utf8_lossy(&self.data).into_owned());
        }
        Ok(events)
    }

    fn finish_line(&mut self, events: &mut Vec<String>) -> Result<(), String> {
        if self.line.ends_with(b"\r") {
            self.line.pop();
        }
        let line = std::mem::take(&mut self.line);
        if line.is_empty() {
            if !self.data.is_empty() {
                events.push(String::from_utf8_lossy(&self.data).into_owned());
                self.data.clear();
            }
            self.discarding = false;
            return Ok(());
        }
        if self.discarding || line.starts_with(b":") {
            return Ok(());
        }
        let Some(colon) = line.iter().position(|byte| *byte == b':') else {
            return Ok(());
        };
        let field = &line[..colon];
        if field != b"data" {
            return Ok(());
        }
        let mut value = &line[colon + 1..];
        if value.first() == Some(&b' ') {
            value = &value[1..];
        }
        if self.data.len().saturating_add(value.len() + 1) > wire::MAX_FRAME_BYTES {
            self.data.clear();
            self.discarding = true;
            return Err(format!(
                "one SSE event exceeds the {}-byte limit",
                wire::MAX_FRAME_BYTES
            ));
        }
        if !self.data.is_empty() {
            self.data.push(b'\n');
        }
        self.data.extend_from_slice(value);
        Ok(())
    }
}

fn lock_or_recover<T>(mutex: &StdMutex<T>) -> StdMutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_decoder_handles_split_lines_comments_and_multiline_data() {
        let mut decoder = SseDecoder::default();
        assert!(decoder
            .push(b": keepalive\n\n")
            .expect("comment")
            .is_empty());
        let events = decoder
            .push(b"event: message\ndata: {\"a\":\ndata: 1}\n\n")
            .expect("two data lines");
        assert_eq!(events, vec!["{\"a\":\n1}"]);
    }

    #[test]
    fn sse_decoder_preserves_utf8_split_across_network_chunks() {
        let mut decoder = SseDecoder::default();
        let bytes = "data: 汉字\n\n".as_bytes();
        assert!(decoder.push(&bytes[..8]).expect("first chunk").is_empty());
        let events = decoder.push(&bytes[8..]).expect("second chunk");
        assert_eq!(events, vec!["汉字"]);
    }

    #[test]
    fn ring_provider_can_build_the_rustls_client_config_used_by_reqwest() {
        install_crypto_provider();
        let _ = rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
    }

    #[test]
    fn an_http_frame_is_bounded_before_it_reaches_the_network() {
        let oversized = json!({ "value": "x".repeat(wire::MAX_FRAME_BYTES) });
        let error = encode_json_frame("mcp-1", &oversized).expect_err("oversized frame");
        assert!(matches!(error, McpError::Protocol { .. }), "{error:?}");
    }
}
