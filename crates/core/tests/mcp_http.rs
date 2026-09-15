//! End-to-end Streamable HTTP coverage against a local, deterministic HTTP server.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use yukinal_core::mcp::{
    McpAuthScheme, McpAuthorization, McpAuthorizationRequest, McpHttpConfig, McpOAuthFuture,
    McpOAuthTokenSource, McpSupervisor, McpTransportConfig, NetworkProxy, ProxyCredential,
    ProxySource, DEFAULT_REQUEST_TIMEOUT,
};

const SESSION_ID: &str = "test-session";

#[derive(Debug, Clone)]
struct RecordedRequest {
    method: String,
    path: String,
    session_id: Option<String>,
    protocol_version: Option<String>,
    authorization: Option<String>,
    dpop: Option<String>,
    gateway_key: Option<String>,
    body: Value,
}

#[derive(Default)]
struct FakeOAuthSource {
    refreshes: AtomicUsize,
}

impl McpOAuthTokenSource for FakeOAuthSource {
    fn authorization<'a>(
        &'a self,
        request: McpAuthorizationRequest,
    ) -> McpOAuthFuture<'a, McpAuthorization> {
        Box::pin(async move {
            let token = if request.force_refresh {
                self.refreshes.fetch_add(1, Ordering::SeqCst);
                "fresh-oauth-token".to_string()
            } else if self.refreshes.load(Ordering::SeqCst) > 0 {
                "fresh-oauth-token".to_string()
            } else {
                "stale-oauth-token".to_string()
            };
            Ok(McpAuthorization {
                scheme: McpAuthScheme::Bearer,
                token,
                proof: None,
            })
        })
    }
}

/// 一个只会说 DPoP 的令牌源：proof 里带什么由 transport 传进来的 nonce 决定，
/// 这样测试能分辨「服务器挑战过的 nonce」有没有真的被送回去。
#[derive(Default)]
struct FakeDpopSource {
    proofs: AtomicUsize,
}

impl McpOAuthTokenSource for FakeDpopSource {
    fn authorization<'a>(
        &'a self,
        request: McpAuthorizationRequest,
    ) -> McpOAuthFuture<'a, McpAuthorization> {
        Box::pin(async move {
            self.proofs.fetch_add(1, Ordering::SeqCst);
            let proof = match request.nonce.as_deref() {
                Some(nonce) => format!("{}|nonce={nonce}", request.method),
                None => request.method.clone(),
            };
            Ok(McpAuthorization {
                scheme: McpAuthScheme::Dpop,
                token: "dpop-token".to_string(),
                proof: Some(proof),
            })
        })
    }
}

/// 用哪一套认证规则检查进来的请求。
struct AuthPolicy {
    token: Option<String>,
    scheme: &'static str,
    /// 需要 proof 携带的 nonce；不匹配就回 401 + 挑战，并计一次数。
    nonce: Option<String>,
    challenges: Arc<AtomicUsize>,
}

impl Default for AuthPolicy {
    fn default() -> Self {
        Self {
            token: None,
            scheme: "Bearer",
            nonce: None,
            challenges: Arc::new(AtomicUsize::new(0)),
        }
    }
}

struct TestHttpServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    sse: Arc<Mutex<Option<TcpStream>>>,
    policy: Arc<Mutex<AuthPolicy>>,
}

impl TestHttpServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test HTTP server");
        let address = listener.local_addr().expect("listener address");
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let sse = Arc::new(Mutex::new(None));
        let policy = Arc::new(Mutex::new(AuthPolicy::default()));
        let thread_stop = stop.clone();
        let thread_requests = requests.clone();
        let thread_sse = sse.clone();
        let thread_policy = policy.clone();
        let thread = thread::spawn(move || {
            for incoming in listener.incoming() {
                if thread_stop.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(stream) = incoming else {
                    break;
                };
                let requests = thread_requests.clone();
                let sse = thread_sse.clone();
                let policy = thread_policy.clone();
                let stop = thread_stop.clone();
                thread::spawn(move || {
                    let _ = handle_connection(stream, requests, sse, policy, stop);
                });
            }
        });
        Self {
            address,
            stop,
            thread: Some(thread),
            requests,
            sse,
            policy,
        }
    }

    fn url(&self) -> String {
        format!("http://{}/mcp", self.address)
    }

    fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().expect("requests").clone()
    }

    fn require_bearer(&self, token: &str) {
        let mut policy = self.policy.lock().expect("policy");
        policy.token = Some(token.to_string());
        policy.scheme = "Bearer";
    }

    /// 只接受 DPoP 方案（`Authorization: DPoP <token>`）的请求。
    fn require_dpop(&self, token: &str) {
        let mut policy = self.policy.lock().expect("policy");
        policy.token = Some(token.to_string());
        policy.scheme = "DPoP";
    }

    /// 要求每个请求的 proof 都带上这个 nonce，否则回一次挑战。
    fn challenge_nonce(&self, nonce: &str) -> Arc<AtomicUsize> {
        let mut policy = self.policy.lock().expect("policy");
        policy.nonce = Some(nonce.to_string());
        policy.challenges.clone()
    }

    fn wait_for_sse(&self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while self.sse.lock().expect("SSE stream").is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "the optional GET SSE stream was not opened"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for TestHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(mut stream) = self.sse.lock().expect("SSE stream").take() {
            let _ = write_sse_chunk(&mut stream, "");
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// 一次经过代理的请求：代理看到的是**绝对 URI**，而目标服务器看到的是普通路径。
#[derive(Debug, Clone)]
struct ProxiedRequest {
    method: String,
    target: String,
    authorization: Option<String>,
}

/// 一个最小的正向代理：只处理绝对 URI 的明文 HTTP，把请求转发给目标并双向中继响应。
///
/// 它**不是**通用代理实现，而是「客户端有没有真的按代理说话」的见证者：记录每条请求的方法、
/// 绝对 URI 与 `Proxy-Authorization`，并可以要求一个固定的凭据（否则回 407）。
struct ForwardProxy {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    requests: Arc<Mutex<Vec<ProxiedRequest>>>,
    required_authorization: Arc<Mutex<Option<String>>>,
}

impl ForwardProxy {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy");
        let address = listener.local_addr().expect("proxy address");
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let required_authorization = Arc::new(Mutex::new(None));
        let thread_stop = stop.clone();
        let thread_requests = requests.clone();
        let thread_required = required_authorization.clone();
        let thread = thread::spawn(move || {
            for incoming in listener.incoming() {
                if thread_stop.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(stream) = incoming else { break };
                let requests = thread_requests.clone();
                let required = thread_required.clone();
                thread::spawn(move || {
                    let _ = handle_proxied_connection(stream, requests, required);
                });
            }
        });
        Self {
            address,
            stop,
            thread: Some(thread),
            requests,
            required_authorization,
        }
    }

    fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn requests(&self) -> Vec<ProxiedRequest> {
        self.requests.lock().expect("proxied requests").clone()
    }

    fn require_credential(&self, value: &str) {
        *self.required_authorization.lock().expect("required") = Some(value.to_string());
    }
}

impl Drop for ForwardProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn handle_proxied_connection(
    mut client: TcpStream,
    requests: Arc<Mutex<Vec<ProxiedRequest>>>,
    required: Arc<Mutex<Option<String>>>,
) -> std::io::Result<()> {
    client.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let read = client.read(&mut buffer)?;
        if read == 0 {
            return Ok(());
        }
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(index) = find_header_end(&bytes) {
            break index;
        }
        if bytes.len() > 64 * 1024 {
            return Ok(());
        }
    };
    let head = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default().to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let version = parts.next().unwrap_or_default().to_string();
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut authorization = None;
    let mut content_length = 0_usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_string();
        if name == "proxy-authorization" {
            authorization = Some(value.clone());
        }
        if name == "content-length" {
            content_length = value.parse().unwrap_or(0);
        }
        headers.push((name, value));
    }
    let body_start = header_end + 4;
    while bytes.len() < body_start + content_length {
        let read = client.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    let body = bytes[body_start..body_start + content_length].to_vec();
    requests
        .lock()
        .expect("proxied requests")
        .push(ProxiedRequest {
            method: method.clone(),
            target: target.clone(),
            authorization: authorization.clone(),
        });

    if let Some(expected) = required.lock().expect("required").clone() {
        if authorization.as_deref() != Some(expected.as_str()) {
            client.write_all(
                b"HTTP/1.1 407 Proxy Authentication Required\r\n\
                  Proxy-Authenticate: Basic realm=\"yukinal-test\"\r\n\
                  Content-Length: 0\r\nConnection: close\r\n\r\n",
            )?;
            client.flush()?;
            return Ok(());
        }
    }

    // 绝对 URI → 源站形式：转发给目标时用的是普通路径（RFC 9110 §7.5）。
    let parsed = reqwest::Url::parse(&target).map_err(|error| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, error.to_string())
    })?;
    let origin = format!(
        "{}:{}",
        parsed.host_str().unwrap_or_default(),
        parsed.port_or_known_default().unwrap_or(80)
    );
    let path = match parsed.query() {
        Some(query) => format!("{}?{query}", parsed.path()),
        None => parsed.path().to_string(),
    };
    let mut upstream = TcpStream::connect(origin)?;
    let mut forwarded = format!("{method} {path} {version}\r\n");
    for (name, value) in &headers {
        // 代理凭据只到代理为止（RFC 9110 §11.7.1）。
        if name == "proxy-authorization" || name == "connection" {
            continue;
        }
        forwarded.push_str(&format!("{name}: {value}\r\n"));
    }
    forwarded.push_str("\r\n");
    upstream.write_all(forwarded.as_bytes())?;
    upstream.write_all(&body)?;
    upstream.flush()?;

    // 双向中继：GET 事件流是一条长期连接，必须在两个方向上都还在搬字节。
    let mut client_copy = client.try_clone()?;
    let mut upstream_copy = upstream.try_clone()?;
    let pump = thread::spawn(move || {
        let _ = std::io::copy(&mut client_copy, &mut upstream_copy);
        let _ = upstream_copy.shutdown(std::net::Shutdown::Both);
    });
    let mut upstream_read = upstream;
    let mut client_write = client;
    let _ = std::io::copy(&mut upstream_read, &mut client_write);
    let _ = client_write.shutdown(std::net::Shutdown::Both);
    let _ = pump.join();
    Ok(())
}

fn handle_connection(
    mut stream: TcpStream,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    sse: Arc<Mutex<Option<TcpStream>>>,
    policy: Arc<Mutex<AuthPolicy>>,
    stop: Arc<AtomicBool>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Ok(());
        }
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(index) = find_header_end(&bytes) {
            break index;
        }
        if bytes.len() > 64 * 1024 {
            return Ok(());
        }
    };

    let head = String::from_utf8_lossy(&bytes[..header_end]);
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let body_start = header_end + 4;
    while bytes.len() < body_start + content_length {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    let body = serde_json::from_slice::<Value>(&bytes[body_start..body_start + content_length])
        .unwrap_or(Value::Null);

    requests.lock().expect("requests").push(RecordedRequest {
        method: method.clone(),
        path: path.clone(),
        session_id: headers.get("mcp-session-id").cloned(),
        protocol_version: headers.get("mcp-protocol-version").cloned(),
        authorization: headers.get("authorization").cloned(),
        dpop: headers.get("dpop").cloned(),
        gateway_key: headers.get("x-gateway-key").cloned(),
        body: body.clone(),
    });

    {
        let policy = policy.lock().expect("policy");
        if policy.token.as_ref().is_some_and(|expected| {
            headers.get("authorization").map(String::as_str)
                != Some(format!("{} {expected}", policy.scheme).as_str())
        }) {
            return write_response(&mut stream, 401, "Unauthorized", &[], b"");
        }
        if let Some(nonce) = policy.nonce.as_deref() {
            let carries_nonce = headers
                .get("dpop")
                .is_some_and(|proof| proof.contains(&format!("nonce={nonce}")));
            if !carries_nonce {
                policy.challenges.fetch_add(1, Ordering::SeqCst);
                return write_response(
                    &mut stream,
                    401,
                    "Unauthorized",
                    &[
                        ("DPoP-Nonce", nonce),
                        ("WWW-Authenticate", "DPoP error=\"use_dpop_nonce\""),
                    ],
                    b"",
                );
            }
        }
    }

    match (method.as_str(), path.as_str()) {
        ("GET", "/mcp") => {
            stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                  Cache-Control: no-cache\r\nTransfer-Encoding: chunked\r\n\
                  Connection: keep-alive\r\n\r\n",
            )?;
            stream.flush()?;
            *sse.lock().expect("SSE stream") = Some(stream);
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(20));
            }
            Ok(())
        }
        ("DELETE", "/mcp") => {
            if let Some(mut writer) = sse.lock().expect("SSE stream").take() {
                let _ = write_sse_chunk(&mut writer, "");
                let _ = writer.shutdown(std::net::Shutdown::Both);
            }
            write_response(&mut stream, 204, "No Content", &[], b"")
        }
        ("POST", "/mcp") => {
            let rpc_method = body
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let id = body.get("id").cloned();
            match rpc_method {
                "initialize" => {
                    let result = json!({
                        "protocolVersion": "2025-11-25",
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "http-fixture", "version": "1.0.0" }
                    });
                    write_json_response(&mut stream, id, result, true)
                }
                "notifications/initialized" | "notifications/cancelled" => {
                    write_response(&mut stream, 202, "Accepted", &[], b"")
                }
                "tools/list" => write_json_response(
                    &mut stream,
                    id,
                    json!({
                        "tools": [
                            {
                                "name": "echo",
                                "description": "echoes text",
                                "inputSchema": {
                                    "type": "object",
                                    "properties": { "text": { "type": "string" } }
                                }
                            },
                            { "name": "slow", "inputSchema": { "type": "object" } },
                            { "name": "deferred", "inputSchema": { "type": "object" } }
                        ]
                    }),
                    false,
                ),
                "tools/call" => {
                    let tool = body
                        .get("params")
                        .and_then(|params| params.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if tool == "slow" {
                        write_response(&mut stream, 202, "Accepted", &[], b"")
                    } else if tool == "deferred" {
                        write_response(&mut stream, 202, "Accepted", &[], b"")?;
                        let response = json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{ "type": "text", "text": "via: get stream" }],
                                "isError": false
                            }
                        });
                        let mut writer = sse.lock().expect("SSE stream");
                        let writer = writer
                            .as_mut()
                            .expect("the client must have opened the GET stream");
                        write_sse_chunk(writer, &format!("data: {response}\n\n"))
                    } else {
                        let response = json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{ "type": "text", "text": "http: hello" }],
                                "isError": false
                            }
                        });
                        let event = format!("event: message\ndata: {response}\n\n");
                        write_response(
                            &mut stream,
                            200,
                            "OK",
                            &[("Content-Type", "text/event-stream")],
                            event.as_bytes(),
                        )
                    }
                }
                _ => write_response(&mut stream, 202, "Accepted", &[], b""),
            }
        }
        _ => write_response(&mut stream, 404, "Not Found", &[], b""),
    }
}

fn write_json_response(
    stream: &mut TcpStream,
    id: Option<Value>,
    result: Value,
    include_session: bool,
) -> std::io::Result<()> {
    let body = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "result": result
    }))
    .expect("encode response");
    let mut headers = vec![("Content-Type", "application/json")];
    if include_session {
        headers.push(("Mcp-Session-Id", SESSION_ID));
    }
    write_response(stream, 200, "OK", &headers, &body)
}

fn write_sse_chunk(stream: &mut TcpStream, payload: &str) -> std::io::Result<()> {
    if payload.is_empty() {
        stream.write_all(b"0\r\n\r\n")?;
        return stream.flush();
    }
    write!(stream, "{:X}\r\n", payload.len())?;
    stream.write_all(payload.as_bytes())?;
    stream.write_all(b"\r\n")?;
    stream.flush()
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<()> {
    let mut response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        response.push_str(name);
        response.push_str(": ");
        response.push_str(value);
        response.push_str("\r\n");
    }
    response.push_str("\r\n");
    stream.write_all(response.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

#[tokio::test]
async fn oauth_refreshes_after_401_and_reuses_the_token_for_stream_and_delete() {
    let server = TestHttpServer::start();
    server.require_bearer("fresh-oauth-token");
    let oauth = Arc::new(FakeOAuthSource::default());
    let config = McpHttpConfig::new(
        "mcp-oauth",
        "OAuth fixture",
        &server.url(),
        DEFAULT_REQUEST_TIMEOUT,
    )
    .expect("valid HTTP config")
    .with_auth_header("X-Gateway-Key", "gateway-secret")
    .expect("non-Authorization header")
    .with_oauth_source(oauth.clone())
    .expect("OAuth source");
    let supervisor = McpSupervisor::new();

    supervisor
        .start_transport(&McpTransportConfig::Http(config))
        .await
        .expect("the stale token must be refreshed and the startup sequence must succeed");
    server.wait_for_sse();
    assert_eq!(
        oauth.refreshes.load(Ordering::SeqCst),
        1,
        "exactly one forced refresh should be needed"
    );

    supervisor.shutdown("mcp-oauth").await;
    let requests = server.requests();
    assert!(
        requests
            .iter()
            .any(|request| request.authorization.as_deref() == Some("Bearer stale-oauth-token")),
        "the first initialize request must demonstrate the stale-token path"
    );
    for method in ["POST", "GET", "DELETE"] {
        let matching = requests
            .iter()
            .filter(|request| request.method == method)
            .collect::<Vec<_>>();
        assert!(!matching.is_empty(), "expected a {method} request");
        let successful_start = if method == "POST" {
            matching
                .iter()
                .position(|request| {
                    request.authorization.as_deref() == Some("Bearer stale-oauth-token")
                })
                .map_or(0, |index| index + 1)
        } else {
            0
        };
        assert!(
            matching[successful_start..].iter().all(|request| {
                request.authorization.as_deref() == Some("Bearer fresh-oauth-token")
                    && request.gateway_key.as_deref() == Some("gateway-secret")
            }),
            "all successful {method} requests must carry both dynamic and static auth"
        );
    }
}

#[tokio::test]
async fn dpop_proofs_go_out_on_every_method_and_a_nonce_challenge_is_retried_once() {
    let server = TestHttpServer::start();
    server.require_dpop("dpop-token");
    let challenges = server.challenge_nonce("n-1");
    let oauth = Arc::new(FakeDpopSource::default());
    let config = McpHttpConfig::new(
        "mcp-dpop",
        "DPoP fixture",
        &server.url(),
        DEFAULT_REQUEST_TIMEOUT,
    )
    .expect("valid HTTP config")
    .with_oauth_source(oauth.clone())
    .expect("OAuth source");
    let supervisor = McpSupervisor::new();

    supervisor
        .start_transport(&McpTransportConfig::Http(config))
        .await
        .expect("the startup sequence must succeed after the nonce challenge");
    server.wait_for_sse();
    supervisor.shutdown("mcp-dpop").await;

    let requests = server.requests();
    for method in ["POST", "GET", "DELETE"] {
        let matching = requests
            .iter()
            .filter(|request| request.method == method)
            .collect::<Vec<_>>();
        assert!(!matching.is_empty(), "expected a {method} request");
        assert!(
            matching.iter().all(|request| {
                request.authorization.as_deref() == Some("DPoP dpop-token")
                    && request.dpop.is_some()
            }),
            "every {method} request must carry the DPoP scheme and a proof"
        );
    }

    // 挑战次数必须等于「没带 nonce 的请求数」：带 nonce 的那次重发没有被再挑战一次，
    // 也就证明重试只有一轮（这是 ADR 0018 里那个「最多三次」的上限在传输层的形状）。
    let without_nonce = requests
        .iter()
        .filter(|request| {
            !request
                .dpop
                .as_deref()
                .is_some_and(|proof| proof.contains("nonce=n-1"))
        })
        .count();
    assert!(
        without_nonce > 0,
        "the fixture must have challenged something"
    );
    assert_eq!(
        challenges.load(Ordering::SeqCst),
        without_nonce,
        "each proof without the nonce is challenged exactly once, and the retry carries it"
    );
}

#[tokio::test]
async fn a_system_proxy_carries_every_method_and_no_proxy_bypasses_it() {
    // 经代理：POST、GET 事件流与 DELETE 三条都要在代理那里留下记录，而且是**绝对 URI**。
    let proxy = ForwardProxy::start();
    let server = TestHttpServer::start();
    let config = McpHttpConfig::new(
        "mcp-proxied",
        "proxied",
        &server.url(),
        DEFAULT_REQUEST_TIMEOUT,
    )
    .expect("valid HTTP config")
    .with_proxy(
        NetworkProxy::Proxy {
            url: proxy.url(),
            no_proxy: None,
            source: ProxySource::EnvironmentVariable,
        },
        None,
    );
    let supervisor = McpSupervisor::new();
    supervisor
        .start_transport(&McpTransportConfig::Http(config))
        .await
        .expect("the startup sequence must work through a proxy");
    server.wait_for_sse();
    supervisor.shutdown("mcp-proxied").await;

    let seen = proxy.requests();
    for method in ["POST", "GET", "DELETE"] {
        assert!(
            seen.iter().any(|request| request.method == method),
            "the proxy must see a {method}: {seen:?}"
        );
    }
    assert!(
        seen.iter()
            .all(|request| request.target.starts_with("http://127.0.0.1:")),
        "a request addressed to a proxy carries the absolute URI: {seen:?}"
    );

    // NO_PROXY 命中：同一个 endpoint 这次直连，代理一条都看不到。
    let bypass = ForwardProxy::start();
    let direct = TestHttpServer::start();
    let config = McpHttpConfig::new(
        "mcp-direct",
        "direct",
        &direct.url(),
        DEFAULT_REQUEST_TIMEOUT,
    )
    .expect("valid HTTP config")
    .with_proxy(
        NetworkProxy::Proxy {
            url: bypass.url(),
            no_proxy: Some("127.0.0.1".to_string()),
            source: ProxySource::EnvironmentVariable,
        },
        None,
    );
    let supervisor = McpSupervisor::new();
    supervisor
        .start_transport(&McpTransportConfig::Http(config))
        .await
        .expect("a NO_PROXY match must connect directly");
    direct.wait_for_sse();
    supervisor.shutdown("mcp-direct").await;
    assert!(
        bypass.requests().is_empty(),
        "a target matched by NO_PROXY must not go through the proxy"
    );
}

#[tokio::test]
async fn an_authenticating_proxy_gets_the_credential_and_a_dead_proxy_fails_closed() {
    // `Basic dXNlcjpwYXNz` 是 user:pass（RFC 7617），凭据只出现在 Proxy-Authorization 上。
    let proxy = ForwardProxy::start();
    proxy.require_credential("Basic dXNlcjpwYXNz");
    let server = TestHttpServer::start();
    let credential = ProxyCredential::new("user:pass".to_string()).expect("credential");
    let config = McpHttpConfig::new(
        "mcp-proxy-auth",
        "authenticated proxy",
        &server.url(),
        DEFAULT_REQUEST_TIMEOUT,
    )
    .expect("valid HTTP config")
    .with_proxy(
        NetworkProxy::Proxy {
            url: proxy.url(),
            no_proxy: None,
            source: ProxySource::WindowsInternetSettings,
        },
        Some(credential),
    );
    let supervisor = McpSupervisor::new();
    supervisor
        .start_transport(&McpTransportConfig::Http(config))
        .await
        .expect("an authenticating proxy must accept the configured credential");
    server.wait_for_sse();
    supervisor.shutdown("mcp-proxy-auth").await;
    assert!(
        proxy
            .requests()
            .iter()
            .all(|request| request.authorization.as_deref() == Some("Basic dXNlcjpwYXNz")),
        "every proxied request carries the credential, and only there"
    );

    // 代理不可达：启动失败，而且错误里点名是代理 —— 只说「连不上」在代理环境里等于没说。
    let unreachable = TestHttpServer::start();
    let closed = unused_local_port();
    let config = McpHttpConfig::new(
        "mcp-proxy-dead",
        "dead proxy",
        &unreachable.url(),
        DEFAULT_REQUEST_TIMEOUT,
    )
    .expect("valid HTTP config")
    .with_proxy(
        NetworkProxy::Proxy {
            url: format!("http://127.0.0.1:{closed}"),
            no_proxy: None,
            source: ProxySource::EnvironmentVariable,
        },
        None,
    );
    let error = McpSupervisor::new()
        .start_transport(&McpTransportConfig::Http(config))
        .await
        .expect_err("a dead proxy must fail closed");
    assert!(
        error.to_string().contains("via proxy"),
        "the failure must say it went through a proxy: {error}"
    );
    assert!(
        unreachable.requests().is_empty(),
        "the endpoint must not be reached directly after the proxy failed"
    );
}

#[tokio::test]
async fn an_https_endpoint_keeps_the_proxy_route_and_never_downgrades_to_direct() {
    let endpoint = TestHttpServer::start();
    let closed = unused_local_port();
    let https_url = endpoint.url().replacen("http://", "https://", 1);
    let config = McpHttpConfig::new(
        "mcp-https-proxy",
        "HTTPS proxy route",
        &https_url,
        DEFAULT_REQUEST_TIMEOUT,
    )
    .expect("HTTPS endpoints are valid remote MCP endpoints")
    .with_proxy(
        NetworkProxy::Proxy {
            url: format!("http://127.0.0.1:{closed}"),
            no_proxy: None,
            source: ProxySource::EnvironmentVariable,
        },
        None,
    );

    let error = McpSupervisor::new()
        .start_transport(&McpTransportConfig::Http(config))
        .await
        .expect_err("a failed proxy must not trigger a direct HTTPS attempt");
    assert!(
        error.to_string().contains("via proxy"),
        "the failure must identify the configured proxy: {error}"
    );
    assert!(
        endpoint.requests().is_empty(),
        "the HTTPS endpoint must not receive a downgraded direct request"
    );
}

/// 一个刚被释放的本地端口：连接它一定失败，而且不是「目标拒绝」。
fn unused_local_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    port
}

#[tokio::test]
async fn streamable_http_initializes_lists_calls_and_terminates_a_session() {
    let server = TestHttpServer::start();
    let config = McpHttpConfig::new(
        "mcp_http",
        "HTTP fixture",
        &server.url(),
        DEFAULT_REQUEST_TIMEOUT,
    )
    .expect("valid HTTP config")
    .with_auth_header("Authorization", "Bearer test-secret")
    .expect("valid authentication header")
    .with_auth_header("X-Gateway-Key", "gateway-secret")
    .expect("second authentication header");
    let supervisor = McpSupervisor::new();

    let started = supervisor
        .start_transport(&McpTransportConfig::Http(config))
        .await
        .expect("HTTP handshake and tool list");
    assert_eq!(started.tool_count, 3);
    assert!(started.info.pid.is_none());
    assert!(started.info.program.is_none());
    assert_eq!(
        started
            .info
            .handshake
            .as_ref()
            .map(|handshake| handshake.server_name.as_str()),
        Some("http-fixture")
    );

    let result = supervisor
        .call("mcp_http", "echo", json!({ "text": "hello" }))
        .await
        .expect("SSE tool response");
    assert_eq!(result.text(), "http: hello");

    server.wait_for_sse();
    let deferred = supervisor
        .call("mcp_http", "deferred", json!({}))
        .await
        .expect("response delivered on the GET SSE stream");
    assert_eq!(deferred.text(), "via: get stream");

    let cancel = CancellationToken::new();
    let cancellation = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancellation.cancel();
    });
    let error = supervisor
        .call_with_cancel("mcp_http", "slow", json!({}), &cancel)
        .await
        .expect_err("the server never answers the slow call");
    assert!(matches!(
        error,
        yukinal_core::mcp::McpError::Cancelled { .. }
    ));

    let shutdown = supervisor
        .shutdown("mcp_http")
        .await
        .expect("tracked HTTP session");
    assert!(shutdown.was_running);
    assert!(!shutdown.killed);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let requests = loop {
        let requests = server.requests();
        if requests.iter().any(|request| {
            request.body["method"] == "notifications/cancelled"
                && request.body["params"]["requestId"] == json!(5)
        }) {
            break requests;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the cancellation notification never reached the test server: {:?}; diagnostics: {:?}",
            requests,
            supervisor.status("mcp_http").await.diagnostics
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let initialize = requests
        .iter()
        .find(|request| request.body["method"] == "initialize")
        .expect("initialize request");
    assert_eq!(initialize.session_id, None, "session starts unassigned");

    let list = requests
        .iter()
        .find(|request| request.body["method"] == "tools/list")
        .expect("tools/list request");
    assert_eq!(list.session_id.as_deref(), Some(SESSION_ID));
    assert_eq!(list.protocol_version.as_deref(), Some("2025-11-25"));

    assert!(
        requests.iter().any(|request| {
            request.body["method"] == "notifications/cancelled"
                && request.body["params"]["requestId"] == json!(5)
        }),
        "cancellation must be sent as an MCP notification"
    );
    assert!(
        requests
            .iter()
            .any(|request| request.method == "DELETE" && request.path == "/mcp"),
        "shutdown must send the optional DELETE session termination"
    );
    for method in ["POST", "GET", "DELETE"] {
        assert!(
            requests
                .iter()
                .filter(|request| request.method == method)
                .all(|request| request.authorization.as_deref() == Some("Bearer test-secret")),
            "every {method} request must carry the configured authentication header"
        );
        assert!(
            requests
                .iter()
                .filter(|request| request.method == method)
                .all(|request| request.gateway_key.as_deref() == Some("gateway-secret")),
            "every {method} request must carry every configured authentication header"
        );
    }
}
