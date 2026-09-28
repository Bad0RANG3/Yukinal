//! `oauth.rs` 的单元测试。
//!
//! 从 `oauth.rs` 拆出来只为可读性：这里驱动的是真实回环 TCP 与假授权服务器，
//! 覆盖 discovery、注册、PKCE 回调、设备码轮询、DPoP nonce 重试与 token 刷新。

use std::collections::HashSet;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener as StdTcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde_json::{json, Value};
use yukinal_credentials::memory::MemoryCredentialStore;
use yukinal_database::models::{McpOAuthConfig, McpServerConfig};

use super::*;

/// One scripted answer to a `device_code` poll, consumed in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceStep {
    Pending,
    SlowDown,
    Denied,
    Expired,
    Grant,
}

/// How the test server expects a client to authenticate on each request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientAuthVia {
    /// Credentials in the request body (`client_secret_post`, or a public `client_id`).
    Body,
    /// Credentials in an `Authorization: Basic …` header (`client_secret_basic`).
    Header,
}

#[derive(Debug, Clone)]
struct ClientExpectation {
    client_id: String,
    secret: Option<String>,
    via: ClientAuthVia,
}

/// 服务器侧对 DPoP proof 的要求与它的检查结果。
///
/// 这是**真的**在验证：用 proof 头里的 JWK 验签、比对方法/URL、检查时间窗、拒绝重复的
/// `jti`。客户端只负责签对，服务器相信它的理由必须是它自己算过。
#[derive(Default)]
struct DpopPolicy {
    required: bool,
    /// 要求 proof 带上的 nonce；不带就回一次挑战。
    nonce: Option<String>,
    /// 挑战过几次：用来断言重试只有一轮。
    challenges: usize,
    /// 见过的 `jti`：重复即重放。
    seen_jti: HashSet<String>,
    /// 强制回答里的 `token_type`（不设就按是否要 DPoP 决定）。
    token_type: Option<String>,
}

/// 一次 proof 检查的结论。
enum DpopVerdict {
    NotRequired,
    Accepted,
    Challenge(String),
    Rejected(String),
}

/// The scripted behaviour of the test server: what it advertises, how it answers, and
/// which client it expects to hear from.
///
/// Scripted rather than dynamic because the interesting client behaviour *is* the
/// sequence — "pending, then slow_down, then granted" is the flow, and a fake that
/// answered at random could not assert that the client kept waiting.
struct ServerScript {
    steps: Mutex<Vec<DeviceStep>>,
    /// Unix milliseconds of each `device_code` poll, so a test can prove the client
    /// waited instead of hammering the endpoint.
    polls: Mutex<Vec<u64>>,
    interval: Mutex<Option<u64>>,
    supported: Mutex<bool>,
    /// `expires_in` the device authorization advertises.
    lifetime: Mutex<u64>,
    /// Defaults to the public `test-client`; a client-secret test replaces it.
    client: Mutex<ClientExpectation>,
    /// A secret to volunteer in the registration response, as some servers do. A public
    /// client must refuse the whole registration rather than start using it.
    registration_secret: Mutex<Option<String>>,
    dpop: Mutex<DpopPolicy>,
}

impl ServerScript {
    fn new() -> Self {
        Self {
            // A granted flow is the default: tests that care about the sequence set one.
            steps: Mutex::new(vec![DeviceStep::Grant]),
            polls: Mutex::new(Vec::new()),
            // `interval: 0` keeps the fixtures fast; the client's 250 ms floor is what
            // keeps that from being a busy loop.
            interval: Mutex::new(Some(0)),
            supported: Mutex::new(true),
            lifetime: Mutex::new(300),
            client: Mutex::new(ClientExpectation {
                client_id: "test-client".to_string(),
                secret: None,
                via: ClientAuthVia::Body,
            }),
            registration_secret: Mutex::new(None),
            dpop: Mutex::new(DpopPolicy::default()),
        }
    }

    /// 要求每个 token 请求带一个可验证的 proof，并在第一次挑战里给出 `nonce`。
    fn require_dpop(&self, nonce: Option<&str>) {
        let mut policy = self.dpop.lock().expect("dpop");
        policy.required = true;
        policy.nonce = nonce.map(str::to_string);
    }

    /// 强制回答里的 `token_type`，用来测「服务器回 Bearer」这条拒绝路径。
    fn force_token_type(&self, value: &str) {
        self.dpop.lock().expect("dpop").token_type = Some(value.to_string());
    }

    fn challenges(&self) -> usize {
        self.dpop.lock().expect("dpop").challenges
    }

    fn token_type(&self) -> String {
        let policy = self.dpop.lock().expect("dpop");
        policy.token_type.clone().unwrap_or_else(|| {
            if policy.required {
                "DPoP".to_string()
            } else {
                "Bearer".to_string()
            }
        })
    }

    /// 服务器侧校验一个 proof（RFC 9449 §4.3 的检查表，去掉我们不需要的项）。
    fn check_dpop(
        &self,
        headers: &HashMap<String, String>,
        method: &str,
        target: &str,
        address: SocketAddr,
    ) -> DpopVerdict {
        let mut policy = self.dpop.lock().expect("dpop");
        if !policy.required {
            return DpopVerdict::NotRequired;
        }
        let Some(proof) = headers.get("dpop") else {
            return DpopVerdict::Rejected("no proof was sent".to_string());
        };
        let parts: Vec<&str> = proof.split('.').collect();
        if parts.len() != 3 {
            return DpopVerdict::Rejected("the proof is not a JWS".to_string());
        }
        let Some(header) = crate::commands::mcp::dpop::decode_base64url(parts[0])
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        else {
            return DpopVerdict::Rejected("the proof header is not JSON".to_string());
        };
        if header["typ"] != "dpop+jwt" || header["alg"] != "EdDSA" {
            return DpopVerdict::Rejected(format!("unexpected proof header: {header}"));
        }
        if header["jwk"]["kty"] != "OKP" || header["jwk"]["crv"] != "Ed25519" {
            return DpopVerdict::Rejected(format!("unexpected proof key: {header}"));
        }
        let Some(public) = header["jwk"]["x"]
            .as_str()
            .and_then(crate::commands::mcp::dpop::decode_base64url)
        else {
            return DpopVerdict::Rejected("the proof key is not base64url".to_string());
        };
        let Some(signature) = crate::commands::mcp::dpop::decode_base64url(parts[2]) else {
            return DpopVerdict::Rejected("the proof signature is not base64url".to_string());
        };
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        if ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public)
            .verify(signing_input.as_bytes(), &signature)
            .is_err()
        {
            return DpopVerdict::Rejected("the proof signature does not verify".to_string());
        }
        let Some(claims) = crate::commands::mcp::dpop::decode_base64url(parts[1])
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        else {
            return DpopVerdict::Rejected("the proof claims are not JSON".to_string());
        };
        // htm/htu：proof 只对这一次请求有效，方法与 URL 都要对得上。
        if claims["htm"] != method {
            return DpopVerdict::Rejected(format!(
                "the proof was made for {} but this request is a {method}",
                claims["htm"]
            ));
        }
        let expected_target = format!("http://{address}{target}");
        if claims["htu"] != expected_target {
            return DpopVerdict::Rejected(format!(
                "the proof was made for {} but this request went to {expected_target}",
                claims["htu"]
            ));
        }
        // 时间窗：服务器允许两分钟的时钟偏差，超出就是过期或未来。
        let iat = claims["iat"].as_u64().unwrap_or_default();
        if iat.abs_diff(unix_now()) > 120 {
            return DpopVerdict::Rejected(format!("the proof is stale (iat={iat})"));
        }
        // token 端点请求不该带 `ath`：那里出示的不是 access token。
        if claims.get("ath").is_some() {
            return DpopVerdict::Rejected("a token request must not carry ath".to_string());
        }
        let Some(jti) = claims["jti"].as_str() else {
            return DpopVerdict::Rejected("the proof has no jti".to_string());
        };
        if !policy.seen_jti.insert(jti.to_string()) {
            return DpopVerdict::Rejected(format!("the proof was replayed (jti={jti})"));
        }
        if let Some(nonce) = policy.nonce.clone() {
            if claims["nonce"].as_str() != Some(nonce.as_str()) {
                policy.challenges += 1;
                return DpopVerdict::Challenge(nonce);
            }
        }
        DpopVerdict::Accepted
    }

    fn with_steps(steps: &[DeviceStep]) -> Self {
        let script = Self::new();
        *script.steps.lock().expect("steps") = steps.to_vec();
        script
    }

    fn next_step(&self) -> DeviceStep {
        let mut steps = self.steps.lock().expect("steps");
        if steps.len() > 1 {
            steps.remove(0)
        } else {
            steps.first().copied().unwrap_or(DeviceStep::Grant)
        }
    }

    fn record_poll(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.polls.lock().expect("polls").push(now);
        now
    }

    fn polls(&self) -> Vec<u64> {
        self.polls.lock().expect("polls").clone()
    }

    /// Require a specific client on every request that carries client authentication.
    fn expect_client(&self, client_id: &str, secret: Option<&str>, via: ClientAuthVia) {
        *self.client.lock().expect("client") = ClientExpectation {
            client_id: client_id.to_string(),
            secret: secret.map(str::to_string),
            via,
        };
    }

    /// The assertion the server makes on a request, so a missing or duplicated
    /// credential fails in the fixture rather than in a token response.
    fn assert_client(&self, client_id: &str, secret: Option<&str>, via: ClientAuthVia) {
        let expected = self.client.lock().expect("client").clone();
        assert_eq!(
            client_id, expected.client_id,
            "the request carried the wrong client id"
        );
        assert_eq!(
            secret,
            expected.secret.as_deref(),
            "the client secret was {} when it should have been {}",
            if secret.is_some() { "sent" } else { "absent" },
            if expected.secret.is_some() {
                "sent"
            } else {
                "absent"
            }
        );
        assert_eq!(
            via, expected.via,
            "the client credentials arrived in the wrong place (RFC 6749 §2.3 allows one \
                 method per request, and this server only accepts the configured one)"
        );
    }
}

struct TestAuthorizationServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<(String, String)>>>,
    registrations: Arc<Mutex<Vec<Value>>>,
    script: Arc<ServerScript>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl TestAuthorizationServer {
    fn start() -> Self {
        Self::with_script(Arc::new(ServerScript::new()))
    }

    fn with_script(script: Arc<ServerScript>) -> Self {
        let listener = StdTcpListener::bind("127.0.0.1:0").expect("bind OAuth test server");
        let address = listener.local_addr().expect("OAuth test address");
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let registrations = Arc::new(Mutex::new(Vec::new()));
        let thread_stop = stop.clone();
        let thread_requests = requests.clone();
        let thread_registrations = registrations.clone();
        let thread_script = script.clone();
        let thread = std::thread::spawn(move || {
            for incoming in listener.incoming() {
                if thread_stop.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(mut stream) = incoming else {
                    break;
                };
                let _ = handle_oauth_test_request(
                    &mut stream,
                    address,
                    &thread_requests,
                    &thread_registrations,
                    &thread_script,
                );
            }
        });
        Self {
            address,
            stop,
            requests,
            registrations,
            script,
            thread: Some(thread),
        }
    }

    fn issuer(&self) -> String {
        format!("http://{}", self.address)
    }

    fn requests(&self) -> Vec<(String, String)> {
        self.requests.lock().expect("requests").clone()
    }

    fn registrations(&self) -> Vec<Value> {
        self.registrations.lock().expect("registrations").clone()
    }

    fn script(&self) -> &ServerScript {
        &self.script
    }
}

impl Drop for TestAuthorizationServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn handle_oauth_test_request(
    stream: &mut TcpStream,
    address: SocketAddr,
    requests: &Arc<Mutex<Vec<(String, String)>>>,
    registrations: &Arc<Mutex<Vec<Value>>>,
    script: &ServerScript,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    let header_end = loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Ok(());
        }
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index;
        }
    };
    let head = String::from_utf8_lossy(&bytes[..header_end]);
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap_or_default().to_string();
    let target = request_parts.next().unwrap_or_default().to_string();
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
    let body =
        String::from_utf8_lossy(&bytes[body_start..body_start + content_length]).into_owned();
    requests
        .lock()
        .expect("requests")
        .push((method.clone(), target.clone()));

    let issuer = format!("http://{address}");
    if target == "/mcp" {
        write!(
                stream,
                "HTTP/1.1 401 Unauthorized\r\n\
                 WWW-Authenticate: Bearer resource_metadata=\"http://{address}/resource-metadata\"\r\n\
                 Content-Length: 0\r\nConnection: close\r\n\r\n"
            )?;
        stream.flush()?;
        return Ok(());
    }
    if target == "/resource-metadata" {
        let body = serde_json::to_vec(&json!({
            "resource": format!("http://{address}/mcp"),
            "authorization_servers": [issuer],
        }))
        .expect("protected resource JSON");
        return write_response(stream, 200, "OK", &body);
    }
    if target == "/.well-known/oauth-authorization-server" {
        let mut metadata = json!({
            "issuer": issuer,
            "authorization_endpoint": format!("http://{address}/authorize"),
            "token_endpoint": format!("http://{address}/token"),
            "registration_endpoint": format!("http://{address}/register"),
            "response_types_supported": ["code"],
            "code_challenge_methods_supported": ["S256"]
        });
        if *script.supported.lock().expect("supported") {
            metadata["device_authorization_endpoint"] = json!(format!("http://{address}/device"));
        }
        let body = serde_json::to_vec(&metadata).expect("metadata JSON");
        return write_response(stream, 200, "OK", &body);
    }
    if target == "/device" && method == "POST" {
        let form = Url::parse(&format!("http://127.0.0.1/?{body}"))
            .expect("form URL")
            .query_pairs()
            .into_owned()
            .collect::<HashMap<_, _>>();
        let credentials = client_credentials(&headers, &form);
        script.assert_client(
            &credentials.client_id,
            credentials.secret.as_deref(),
            credentials.via,
        );
        assert_eq!(
            form.get("resource").map(String::as_str),
            Some(format!("http://{address}/mcp").as_str()),
            "RFC 8707 requires the protected resource on the device authorization request"
        );
        assert_eq!(form.get("scope").map(String::as_str), Some("mcp.read"));
        let interval = *script.interval.lock().expect("interval");
        let lifetime = *script.lifetime.lock().expect("lifetime");
        let mut response = json!({
            "device_code": "device-code",
            "user_code": "WDJB-MJHT",
            "verification_uri": format!("http://{address}/activate"),
            "expires_in": lifetime,
        });
        response["verification_uri_complete"] =
            json!(format!("http://{address}/activate?user_code=WDJB-MJHT"));
        if let Some(interval) = interval {
            response["interval"] = json!(interval);
        }
        let body = serde_json::to_vec(&response).expect("device authorization JSON");
        return write_response(stream, 200, "OK", &body);
    }
    if target == "/register" && method == "POST" {
        if headers.get("content-type").map(String::as_str) != Some("application/json") {
            return write_response(stream, 400, "Bad Request", b"");
        }
        let registration: Value = serde_json::from_str(&body).expect("dynamic registration JSON");
        registrations
            .lock()
            .expect("registrations")
            .push(registration);
        let mut response = json!({
            "client_id": "dynamic-client",
            "token_endpoint_auth_method": "none",
        });
        if let Some(secret) = script
            .registration_secret
            .lock()
            .expect("registration secret")
            .clone()
        {
            response["client_secret"] = json!(secret);
        }
        let body = serde_json::to_vec(&response).expect("registration response JSON");
        return write_response(stream, 201, "Created", &body);
    }
    if target == "/token" && method == "POST" {
        let form = Url::parse(&format!("http://127.0.0.1/?{body}"))
            .expect("form URL")
            .query_pairs()
            .into_owned()
            .collect::<HashMap<_, _>>();
        let credentials = client_credentials(&headers, &form);
        script.assert_client(
            &credentials.client_id,
            credentials.secret.as_deref(),
            credentials.via,
        );
        // 服务器侧的 proof 检查在这一层：签名、htm/htu、时间窗、jti 重放、nonce 挑战。
        // 检查不过就是**测试失败**（fixture 是被信任的对手），而不是一个要被客户端
        // 处理的服务端行为 —— 所以直接 panic，让原因留在失败信息里。
        match script.check_dpop(&headers, &method, &target, address) {
            DpopVerdict::Challenge(nonce) => {
                return write_response_with(
                    stream,
                    401,
                    "Unauthorized",
                    &[
                        ("DPoP-Nonce", nonce.as_str()),
                        ("WWW-Authenticate", "DPoP error=\"use_dpop_nonce\""),
                    ],
                    b"",
                );
            }
            DpopVerdict::Rejected(reason) => {
                panic!("the fixture rejected the DPoP proof: {reason}")
            }
            DpopVerdict::NotRequired | DpopVerdict::Accepted => {}
        }
        // RFC 6749 §2.3: one method per request. A Basic header *and* a body secret
        // would send the same credential twice, which this asserts against.
        if credentials.via == ClientAuthVia::Header {
            assert!(
                !form.contains_key("client_secret"),
                "client_secret_basic must not repeat the secret in the body: {form:?}"
            );
            assert!(
                !form.contains_key("client_id"),
                "client_secret_basic carries the client id in the header, not twice: {form:?}"
            );
        }
        let response = match form.get("grant_type").map(String::as_str) {
            Some(grant) if grant == DEVICE_CODE_GRANT => {
                assert_eq!(
                    form.get("device_code").map(String::as_str),
                    Some("device-code")
                );
                assert_eq!(
                    form.get("resource").map(String::as_str),
                    Some(format!("http://{address}/mcp").as_str())
                );
                script.record_poll();
                match script.next_step() {
                    DeviceStep::Pending => {
                        let body = serde_json::to_vec(&json!({
                            "error": "authorization_pending"
                        }))
                        .expect("pending JSON");
                        return write_response(stream, 400, "Bad Request", &body);
                    }
                    DeviceStep::SlowDown => {
                        let body = serde_json::to_vec(&json!({ "error": "slow_down" }))
                            .expect("slow_down JSON");
                        return write_response(stream, 400, "Bad Request", &body);
                    }
                    DeviceStep::Denied => {
                        let body = serde_json::to_vec(&json!({
                            "error": "access_denied",
                            "error_description": "the user refused the request"
                        }))
                        .expect("denied JSON");
                        return write_response(stream, 400, "Bad Request", &body);
                    }
                    DeviceStep::Expired => {
                        let body = serde_json::to_vec(&json!({ "error": "expired_token" }))
                            .expect("expired JSON");
                        return write_response(stream, 400, "Bad Request", &body);
                    }
                    DeviceStep::Grant => json!({
                        "access_token": "device-access",
                        "refresh_token": "device-refresh",
                        "token_type": script.token_type(),
                        "expires_in": 1
                    }),
                }
            }
            Some("authorization_code") => {
                assert_eq!(form.get("code").map(String::as_str), Some("auth-code"));
                assert!(form
                    .get("code_verifier")
                    .is_some_and(|verifier| verifier.len() >= 43));
                assert_eq!(
                    form.get("resource").map(String::as_str),
                    Some(format!("http://{address}/mcp").as_str())
                );
                json!({
                    "access_token": "access-one",
                    "refresh_token": "refresh-one",
                    "token_type": script.token_type(),
                    "expires_in": 1
                })
            }
            Some("refresh_token") => {
                let refresh_token = form
                    .get("refresh_token")
                    .map(String::as_str)
                    .unwrap_or_default();
                assert!(
                    // The code flow and the device flow each mint their own refresh
                    // token; both must refresh through the same request shape.
                    ["refresh-one", "device-refresh"].contains(&refresh_token),
                    "unexpected OAuth refresh token: {refresh_token}"
                );
                json!({
                    "access_token": "access-two",
                    "token_type": script.token_type(),
                    "expires_in": 3600
                })
            }
            other => panic!("unexpected OAuth grant: {other:?}"),
        };
        let body = serde_json::to_vec(&response).expect("token JSON");
        return write_response(stream, 200, "OK", &body);
    }
    write_response(stream, 404, "Not Found", b"")
}

/// 带额外响应头的回答，DPoP 的 nonce 挑战要用（`DPoP-Nonce` 与 `WWW-Authenticate`）。
fn write_response_with(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<()> {
    let extra: String = headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect();
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
             {extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    body: &[u8],
) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

/// What a request claimed about its client, and where it said it.
struct ReceivedClient {
    client_id: String,
    secret: Option<String>,
    via: ClientAuthVia,
}

/// Read client credentials the way RFC 6749 §2.3 allows them to be sent.
///
/// The test server reads both places instead of trusting one, so a client that puts the
/// secret in the wrong place — or in both places — is visible to the fixture rather than
/// to a token response that quietly ignores it.
fn client_credentials(
    headers: &HashMap<String, String>,
    form: &HashMap<String, String>,
) -> ReceivedClient {
    if let Some(header) = headers.get("authorization") {
        let encoded = header
            .strip_prefix("Basic ")
            .unwrap_or_else(|| panic!("only Basic client authentication is expected: {header}"));
        let decoded =
            decode_base64(encoded).expect("the client's Basic credentials must be base64");
        let decoded = String::from_utf8(decoded).expect("Basic credentials must be UTF-8");
        let (client_id, secret) = decoded
            .split_once(':')
            .expect("Basic credentials are `client_id:secret`");
        return ReceivedClient {
            client_id: client_id.to_string(),
            secret: Some(secret.to_string()),
            via: ClientAuthVia::Header,
        };
    }
    ReceivedClient {
        client_id: form.get("client_id").cloned().unwrap_or_default(),
        secret: form.get("client_secret").cloned(),
        via: ClientAuthVia::Body,
    }
}

/// Standard base64 with padding, for reading what reqwest wrote.
///
/// Pinned by the RFC 7617 example below: a decoder that is wrong in a way that cancels
/// out the encoder it is checking would make every client-secret assertion meaningless.
fn decode_base64(value: &str) -> Option<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = 0_u32;
    let mut bits = 0_u32;
    for character in value.chars() {
        if character == '=' {
            break;
        }
        let digit = match character {
            'A'..='Z' => character as u32 - 'A' as u32,
            'a'..='z' => character as u32 - 'a' as u32 + 26,
            '0'..='9' => character as u32 - '0' as u32 + 52,
            '+' => 62,
            '/' => 63,
            _ => return None,
        };
        buffer = (buffer << 6) | digit;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    Some(output)
}

fn temp_db(name: &str) -> (PathBuf, Database) {
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "yukinal-mcp-oauth-{}-{}-{}.sqlite",
        std::process::id(),
        name,
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    cleanup(&path);
    let database = Database::open(&path).expect("open temp database");
    (path, database)
}

fn cleanup(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(PathBuf::from(format!("{}{suffix}", path.display())));
    }
}

/// The authorization-code fixtures never reach the device-code prompt, so they pass
/// this instead of a recorder.
fn noop_prompt() -> impl FnOnce(DeviceCodePrompt) -> Result<(), String> {
    |_| Ok(())
}

/// The device-flow fixtures that only care about the code open no page.
fn noop_launcher() -> impl FnOnce(&str) -> Result<(), String> {
    |_| Ok(())
}

fn browser_launcher() -> impl FnOnce(&str) -> Result<(), String> {
    |authorization_url| {
        let authorization = Url::parse(authorization_url).map_err(|error| error.to_string())?;
        let parameters = authorization
            .query_pairs()
            .into_owned()
            .collect::<HashMap<_, _>>();
        let redirect_uri = parameters
            .get("redirect_uri")
            .ok_or_else(|| "missing redirect URI".to_string())?
            .clone();
        let state = parameters
            .get("state")
            .ok_or_else(|| "missing state".to_string())?
            .clone();
        std::thread::spawn(move || {
            let callback = Url::parse(&redirect_uri).expect("callback URL");
            let target = format!("/callback?code=auth-code&state={state}");
            let mut stream = TcpStream::connect((
                callback.host_str().expect("callback host"),
                callback.port().expect("callback port"),
            ))
            .expect("connect callback");
            write!(
                stream,
                "GET {target} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
                callback.host_str().expect("callback host")
            )
            .expect("write callback");
            let _ = stream.shutdown(std::net::Shutdown::Write);
            let mut ignored = Vec::new();
            let _ = stream.read_to_end(&mut ignored);
        });
        Ok(())
    }
}

#[tokio::test]
async fn protected_resource_metadata_discovers_the_issuer() {
    let server = TestAuthorizationServer::start();
    let (path, database) = temp_db("resource-discovery");
    database
        .mcp_servers()
        .upsert(&McpServerConfig {
            id: "mcp_oauth".to_string(),
            label: "OAuth discovery fixture".to_string(),
            transport: "http".to_string(),
            command: None,
            args: None,
            url: Some(format!("http://{}/mcp", server.address)),
            http_auth_headers: Vec::new(),
            oauth: Some(oauth_config(
                String::new(),
                "test-client",
                McpOAuthFlow::AuthorizationCode,
                &["mcp.read"],
            )),
            enabled: true,
            allowed_tools: Vec::new(),
            trust_level: "unreviewed".to_string(),
            annotation_trust: Default::default(),
        })
        .expect("insert automatic OAuth row");
    let credentials = Arc::new(MemoryCredentialStore::new());
    let result = connect(
        &database,
        credentials,
        "mcp_oauth",
        OutboundProxy::default(),
        browser_launcher(),
        noop_prompt(),
        CancellationToken::new(),
    )
    .await
    .expect("automatic discovery must complete the same OAuth flow");
    assert_eq!(result.issuer, server.issuer());
    assert_eq!(
        database
            .mcp_servers()
            .get("mcp_oauth")
            .expect("stored row")
            .oauth
            .expect("OAuth config")
            .issuer,
        server.issuer()
    );
    assert_eq!(
        server.requests(),
        vec![
            ("GET".to_string(), "/mcp".to_string()),
            ("GET".to_string(), "/resource-metadata".to_string()),
            (
                "GET".to_string(),
                "/.well-known/oauth-authorization-server".to_string()
            ),
            ("POST".to_string(), "/token".to_string()),
        ]
    );
    cleanup(&path);
}

#[tokio::test]
async fn dynamic_registration_supplies_a_public_client_id() {
    let server = TestAuthorizationServer::start();
    // The registration response carries a client secret; the client still authenticates
    // as the public client it asked to be.
    server
        .script()
        .expect_client("dynamic-client", None, ClientAuthVia::Body);
    let (path, database) = temp_db("dynamic-registration");
    database
        .mcp_servers()
        .upsert(&McpServerConfig {
            id: "mcp_oauth".to_string(),
            label: "OAuth dynamic registration fixture".to_string(),
            transport: "http".to_string(),
            command: None,
            args: None,
            url: Some(format!("http://{}/mcp", server.address)),
            http_auth_headers: Vec::new(),
            oauth: Some(oauth_config(
                String::new(),
                "",
                McpOAuthFlow::AuthorizationCode,
                &["mcp.read", "mcp.tools"],
            )),
            enabled: true,
            allowed_tools: Vec::new(),
            trust_level: "unreviewed".to_string(),
            annotation_trust: Default::default(),
        })
        .expect("insert dynamic-registration OAuth row");
    let credentials = Arc::new(MemoryCredentialStore::new());

    connect(
        &database,
        credentials,
        "mcp_oauth",
        OutboundProxy::default(),
        browser_launcher(),
        noop_prompt(),
        CancellationToken::new(),
    )
    .await
    .expect("dynamic registration must complete the OAuth flow");

    let oauth = database
        .mcp_servers()
        .get("mcp_oauth")
        .expect("stored row")
        .oauth
        .expect("OAuth config");
    assert_eq!(oauth.client_id, "dynamic-client");
    assert_eq!(
        oauth.client_auth,
        McpOAuthClientAuth::None,
        "the registered client asked to be public"
    );
    assert!(
        oauth.client_secret_ref.is_none(),
        "a secret the server volunteered must not be adopted"
    );
    assert_eq!(
        server.requests(),
        vec![
            ("GET".to_string(), "/mcp".to_string()),
            ("GET".to_string(), "/resource-metadata".to_string()),
            (
                "GET".to_string(),
                "/.well-known/oauth-authorization-server".to_string()
            ),
            ("POST".to_string(), "/register".to_string()),
            ("POST".to_string(), "/token".to_string()),
        ]
    );

    let registrations = server.registrations();
    assert_eq!(registrations.len(), 1);
    let registration = &registrations[0];
    assert_eq!(
        registration["client_name"].as_str(),
        Some("Yukinal Desktop")
    );
    assert_eq!(
        registration["token_endpoint_auth_method"].as_str(),
        Some("none")
    );
    assert_eq!(
        registration["grant_types"],
        json!(["authorization_code", "refresh_token"])
    );
    assert_eq!(registration["response_types"], json!(["code"]));
    assert_eq!(registration["scope"].as_str(), Some("mcp.read mcp.tools"));
    assert!(
        registration["redirect_uris"][0]
            .as_str()
            .is_some_and(|uri| uri.starts_with("http://127.0.0.1:") && uri.ends_with("/callback")),
        "dynamic registration must use the exact loopback callback: {registration}"
    );
    cleanup(&path);
}

#[tokio::test]
async fn authorization_code_pkce_is_stored_and_refreshed() {
    let server = TestAuthorizationServer::start();
    let (path, database) = temp_db("flow");
    let endpoint = format!("http://{}/mcp", server.address);
    database
        .mcp_servers()
        .upsert(&McpServerConfig {
            id: "mcp_oauth".to_string(),
            label: "OAuth fixture".to_string(),
            transport: "http".to_string(),
            command: None,
            args: None,
            url: Some(endpoint.clone()),
            http_auth_headers: Vec::new(),
            oauth: Some(oauth_config(
                server.issuer(),
                "test-client",
                McpOAuthFlow::AuthorizationCode,
                &["mcp.read"],
            )),
            enabled: true,
            allowed_tools: Vec::new(),
            trust_level: "unreviewed".to_string(),
            annotation_trust: Default::default(),
        })
        .expect("insert OAuth row");
    let credentials = Arc::new(MemoryCredentialStore::new());
    let result = connect(
        &database,
        credentials.clone(),
        "mcp_oauth",
        OutboundProxy::default(),
        browser_launcher(),
        noop_prompt(),
        CancellationToken::new(),
    )
    .await
    .expect("complete OAuth flow");
    assert_eq!(result.server_id, "mcp_oauth");
    assert_eq!(
        result.token_endpoint,
        format!("http://{}/token", server.address)
    );

    let row = database.mcp_servers().get("mcp_oauth").expect("stored row");
    let oauth = row.oauth.expect("OAuth config");
    let credential_ref =
        CredentialRef::parse(oauth.credential_ref.as_deref().expect("token reference"))
            .expect("credential reference");
    let bundle: OAuthTokenBundle = serde_json::from_str(
        credentials
            .get(&credential_ref)
            .expect("stored token")
            .as_utf8()
            .expect("UTF-8 token bundle")
            .as_ref(),
    )
    .expect("valid token bundle");
    assert_eq!(bundle.access_token, "access-one");

    let source = source_from_config(
        credentials.clone(),
        &McpOAuthSourceConfig {
            server_id: "mcp_oauth".to_string(),
            resource: endpoint,
            token_endpoint: oauth.token_endpoint.expect("token endpoint"),
            client_id: oauth.client_id,
            client_auth: oauth.client_auth,
            client_secret_ref: oauth.client_secret_ref,
            dpop_key_ref: oauth.dpop_key_ref,
            proxy: OutboundProxy::default(),
            scopes: oauth.scopes,
            credential_ref: oauth.credential_ref.expect("credential reference"),
        },
    )
    .expect("token source");
    assert_eq!(token_of(&source, false).await, "access-two");
    let refreshed_bundle: OAuthTokenBundle = serde_json::from_str(
        credentials
            .get(&credential_ref)
            .expect("refreshed token")
            .as_utf8()
            .expect("UTF-8 refreshed token")
            .as_ref(),
    )
    .expect("valid refreshed bundle");
    assert_eq!(refreshed_bundle.access_token, "access-two");
    assert_eq!(
        refreshed_bundle.refresh_token.as_deref(),
        Some("refresh-one")
    );
    assert_eq!(token_of(&source, false).await, "access-two");
    assert_eq!(
        server.requests(),
        vec![
            (
                "GET".to_string(),
                "/.well-known/oauth-authorization-server".to_string()
            ),
            ("POST".to_string(), "/token".to_string()),
            ("POST".to_string(), "/token".to_string()),
        ]
    );
    cleanup(&path);
}

/* ── P0-1：设备码流程（RFC 8628） ────────────────────────────────────────── */

/// A device-flow row identical to the code-flow fixture except for `flow`.
fn insert_device_row(
    database: &Database,
    server: &TestAuthorizationServer,
    client_id: &str,
) -> McpServerConfig {
    insert_row(
        database,
        server,
        McpOAuthFlow::DeviceCode,
        client_id,
        McpOAuthClientAuth::None,
        None,
    )
}

/// One stored OAuth row, with the client-authentication choice laid out explicitly.
fn insert_row(
    database: &Database,
    server: &TestAuthorizationServer,
    flow: McpOAuthFlow,
    client_id: &str,
    client_auth: McpOAuthClientAuth,
    client_secret_ref: Option<&str>,
) -> McpServerConfig {
    let mut oauth = oauth_config(server.issuer(), client_id, flow, &["mcp.read"]);
    oauth.client_auth = client_auth;
    oauth.client_secret_ref = client_secret_ref.map(str::to_string);
    let row = McpServerConfig {
        id: "mcp_oauth".to_string(),
        label: "OAuth fixture".to_string(),
        transport: "http".to_string(),
        command: None,
        args: None,
        url: Some(format!("http://{}/mcp", server.address)),
        http_auth_headers: Vec::new(),
        oauth: Some(oauth),
        enabled: true,
        allowed_tools: Vec::new(),
        trust_level: "unreviewed".to_string(),
        annotation_trust: Default::default(),
    };
    database
        .mcp_servers()
        .upsert(&row)
        .expect("insert OAuth row");
    row
}

/// A code-flow row that authenticates with a client secret, plus the stored secret.
fn insert_secret_flow_row(
    database: &Database,
    server: &TestAuthorizationServer,
    credentials: &MemoryCredentialStore,
    flow: McpOAuthFlow,
    method: McpOAuthClientAuth,
    secret: &str,
) -> CredentialRef {
    let reference = credentials
        .set(
            "mcp",
            "oauth-client-secret",
            &Secret::from_utf8(secret.to_string()),
        )
        .expect("store the client secret");
    insert_row(
        database,
        server,
        flow,
        "confidential-client",
        method,
        Some(reference.to_string_ref().as_str()),
    );
    reference
}

type Prompts = Arc<Mutex<Vec<DeviceCodePrompt>>>;
type Opened = Arc<Mutex<Vec<String>>>;

/// 一行开启了 DPoP 的配置，外加一把真的存在凭据库里的密钥。
fn insert_dpop_row(
    database: &Database,
    server: &TestAuthorizationServer,
    credentials: &MemoryCredentialStore,
) -> CredentialRef {
    let (_, encoded) = DpopKey::generate().expect("generate a DPoP key");
    let reference = credentials
        .set("mcp", "dpop-key", &Secret::from_utf8(encoded))
        .expect("store the DPoP key");
    let mut oauth = oauth_config(
        server.issuer(),
        "test-client",
        McpOAuthFlow::DeviceCode,
        &["mcp.read"],
    );
    oauth.dpop = true;
    oauth.dpop_key_ref = Some(reference.to_string_ref());
    let row = McpServerConfig {
        id: "mcp_oauth".to_string(),
        label: "OAuth fixture".to_string(),
        transport: "http".to_string(),
        command: None,
        args: None,
        url: Some(format!("http://{}/mcp", server.address)),
        http_auth_headers: Vec::new(),
        oauth: Some(oauth),
        enabled: true,
        allowed_tools: Vec::new(),
        trust_level: "unreviewed".to_string(),
        annotation_trust: Default::default(),
    };
    database
        .mcp_servers()
        .upsert(&row)
        .expect("insert the DPoP row");
    reference
}

fn recording_prompt() -> (Prompts, impl FnOnce(DeviceCodePrompt) -> Result<(), String>) {
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let recorder = prompts.clone();
    let prompt = move |value: DeviceCodePrompt| {
        recorder.lock().expect("prompts").push(value);
        Ok(())
    };
    (prompts, prompt)
}

fn recording_launcher() -> (Opened, impl FnOnce(&str) -> Result<(), String>) {
    let opened = Arc::new(Mutex::new(Vec::new()));
    let recorder = opened.clone();
    let launcher = move |url: &str| {
        recorder.lock().expect("opened").push(url.to_string());
        Ok(())
    };
    (opened, launcher)
}

/// A stored OAuth configuration for the fixtures. Keeping it in one place means a new
/// non-secret field does not have to be added to four literals by hand.
fn oauth_config(
    issuer: String,
    client_id: &str,
    flow: McpOAuthFlow,
    scopes: &[&str],
) -> McpOAuthConfig {
    McpOAuthConfig {
        issuer,
        client_id: client_id.to_string(),
        flow,
        client_auth: McpOAuthClientAuth::None,
        client_secret_ref: None,
        dpop: false,
        dpop_key_ref: None,
        scopes: scopes.iter().map(|scope| scope.to_string()).collect(),
        token_endpoint: None,
        credential_ref: None,
    }
}

/// Build a token source from a stored row, so a test cannot forget a field the
/// production path passes (which is exactly how a client-auth method would go missing).
fn source_for_row(
    credentials: Arc<MemoryCredentialStore>,
    row: &McpServerConfig,
    resource: String,
) -> Arc<dyn McpOAuthTokenSource> {
    let oauth = row.oauth.clone().expect("OAuth config");
    source_from_config(
        credentials,
        &McpOAuthSourceConfig {
            server_id: row.id.clone(),
            resource,
            token_endpoint: oauth.token_endpoint.expect("token endpoint"),
            client_id: oauth.client_id,
            client_auth: oauth.client_auth,
            client_secret_ref: oauth.client_secret_ref,
            dpop_key_ref: oauth.dpop_key_ref,
            proxy: OutboundProxy::default(),
            scopes: oauth.scopes,
            credential_ref: oauth.credential_ref.expect("credential reference"),
        },
    )
    .expect("token source")
}

/// 取一次令牌。测试关心的是「拿到的是哪个 token」，不是 proof 的形状（那有专门的
/// 用例），所以这里按 bearer 的路子取一次就够。
async fn token_of(source: &Arc<dyn McpOAuthTokenSource>, force_refresh: bool) -> String {
    source
        .authorization(McpAuthorizationRequest {
            method: "POST".to_string(),
            url: "https://example.test/mcp".to_string(),
            force_refresh,
            nonce: None,
        })
        .await
        .expect("token")
        .token
}

#[tokio::test]
async fn a_dpop_server_gets_a_verifiable_proof_and_a_nonce_retry_on_every_token_request() {
    let script = Arc::new(ServerScript::new());
    // 服务器要求 proof，并挑战一个 nonce：客户端必须先被拒一次，再带着它重来。
    script.require_dpop(Some("server-nonce"));
    let server = TestAuthorizationServer::with_script(script.clone());
    let (path, database) = temp_db("dpop-flow");
    let credentials = Arc::new(MemoryCredentialStore::new());
    let key = insert_dpop_row(&database, &server, credentials.as_ref());
    let (_, prompt) = recording_prompt();
    let (_, launcher) = recording_launcher();

    let result = connect(
        &database,
        credentials.clone(),
        "mcp_oauth",
        OutboundProxy::default(),
        launcher,
        prompt,
        CancellationToken::new(),
    )
    .await
    .expect("the device flow must complete against a DPoP server");
    assert_eq!(result.issuer, server.issuer());

    let row = database.mcp_servers().get("mcp_oauth").expect("row");
    let oauth = row.oauth.expect("oauth config");
    assert_eq!(
        oauth.dpop_key_ref.as_deref(),
        Some(key.to_string_ref().as_str()),
        "the key that signed the requests must be the one the row points at"
    );
    let reference = CredentialRef::parse(
        oauth
            .credential_ref
            .as_deref()
            .expect("the token must be stored"),
    )
    .expect("token reference");
    let stored: OAuthTokenBundle = serde_json::from_str(
        credentials
            .get(&reference)
            .expect("stored bundle")
            .as_utf8()
            .expect("UTF-8 bundle")
            .as_ref(),
    )
    .expect("valid bundle");
    assert_eq!(
        stored.token_type, "DPoP",
        "a DPoP-bound token must be stored as one"
    );

    // 每个 token 请求都被挑战了一次，并且**只**被挑战一次：带 nonce 的重发通过了。
    assert_eq!(
        script.challenges(),
        1,
        "one poll, one challenge, one retry — a second challenge would mean a loop"
    );
    cleanup(&path);
}

#[tokio::test]
async fn a_bearer_token_is_refused_when_the_server_was_asked_for_dpop() {
    let script = Arc::new(ServerScript::new());
    script.require_dpop(None);
    script.force_token_type("Bearer");
    let server = TestAuthorizationServer::with_script(script.clone());
    let (path, database) = temp_db("dpop-bearer");
    let credentials = Arc::new(MemoryCredentialStore::new());
    insert_dpop_row(&database, &server, credentials.as_ref());
    let (_, prompt) = recording_prompt();
    let (_, launcher) = recording_launcher();

    let error = connect(
        &database,
        credentials.clone(),
        "mcp_oauth",
        OutboundProxy::default(),
        launcher,
        prompt,
        CancellationToken::new(),
    )
    .await
    .expect_err("a bearer token must not be adopted when DPoP was requested");
    assert!(
        error.contains("not a dpop token"),
        "the error must say what was wrong with the token, not just that it failed: {error}"
    );
    let row = database.mcp_servers().get("mcp_oauth").expect("row");
    assert!(
        row.oauth.expect("oauth config").credential_ref.is_none(),
        "a refused token must not be stored"
    );
    cleanup(&path);
}

#[test]
fn a_missing_dpop_key_refuses_instead_of_falling_back_to_bearer() {
    let server = TestAuthorizationServer::start();
    let (path, database) = temp_db("dpop-missing-key");
    let credentials = Arc::new(MemoryCredentialStore::new());
    // 配置说要 DPoP，但凭据库里没有那把钥匙（换机器、被清理）：这时令牌已经用不了。
    let mut oauth = oauth_config(
        server.issuer(),
        "test-client",
        McpOAuthFlow::DeviceCode,
        &["mcp.read"],
    );
    oauth.dpop = true;
    oauth.dpop_key_ref = Some("keychain://mcp/does-not-exist".to_string());
    oauth.token_endpoint = Some(format!("http://{}/token", server.address));
    oauth.credential_ref = Some("keychain://mcp/token".to_string());
    let row = McpServerConfig {
        id: "mcp_oauth".to_string(),
        label: "OAuth fixture".to_string(),
        transport: "http".to_string(),
        command: None,
        args: None,
        url: Some(format!("http://{}/mcp", server.address)),
        http_auth_headers: Vec::new(),
        oauth: Some(oauth),
        enabled: true,
        allowed_tools: Vec::new(),
        trust_level: "unreviewed".to_string(),
        annotation_trust: Default::default(),
    };
    database.mcp_servers().upsert(&row).expect("insert row");

    let config = McpOAuthSourceConfig {
        server_id: row.id.clone(),
        resource: row.url.clone().expect("endpoint"),
        token_endpoint: row
            .oauth
            .as_ref()
            .and_then(|oauth| oauth.token_endpoint.clone())
            .expect("token endpoint"),
        client_id: "test-client".to_string(),
        client_auth: McpOAuthClientAuth::None,
        client_secret_ref: None,
        dpop_key_ref: Some("keychain://mcp/does-not-exist".to_string()),
        proxy: OutboundProxy::default(),
        scopes: vec!["mcp.read".to_string()],
        credential_ref: "keychain://mcp/token".to_string(),
    };
    let error = source_from_config(credentials, &config)
        .err()
        .expect("a missing DPoP key must refuse the source");
    assert!(
        error.contains("DPoP key"),
        "the reason must name the key, not the token: {error}"
    );
    cleanup(&path);
}

#[tokio::test]
async fn the_device_flow_shows_a_user_code_then_stores_a_refreshable_bundle() {
    let device = Arc::new(ServerScript::with_steps(&[
        DeviceStep::Pending,
        DeviceStep::Grant,
    ]));
    let server = TestAuthorizationServer::with_script(device.clone());
    let (path, database) = temp_db("device-flow");
    let endpoint = format!("http://{}/mcp", server.address);
    insert_device_row(&database, &server, "test-client");
    let credentials = Arc::new(MemoryCredentialStore::new());
    let (prompts, prompt) = recording_prompt();
    let (opened, launcher) = recording_launcher();

    let result = connect(
        &database,
        credentials.clone(),
        "mcp_oauth",
        OutboundProxy::default(),
        launcher,
        prompt,
        CancellationToken::new(),
    )
    .await
    .expect("the device flow must complete");

    assert_eq!(result.issuer, server.issuer());
    assert_eq!(
        result.token_endpoint,
        format!("http://{}/token", server.address)
    );

    // The user code and where to type it: shown exactly once, with a real deadline.
    let prompts = prompts.lock().expect("prompts").clone();
    assert_eq!(prompts.len(), 1, "the code must be announced once");
    assert_eq!(prompts[0].user_code, "WDJB-MJHT");
    assert_eq!(
        prompts[0].verification_uri,
        format!("http://{}/activate", server.address)
    );
    assert!(
        prompts[0].expires_at > unix_now(),
        "an expiry in the past would tell the UI to stop waiting immediately"
    );
    assert_eq!(
        opened.lock().expect("opened").as_slice(),
        [format!(
            "http://{}/activate?user_code=WDJB-MJHT",
            server.address
        )],
        "the complete URI opens directly; the plain one is what the user types into"
    );

    // The stored bundle has the same shape the code flow stores, so the refresh path
    // cannot behave differently depending on which flow produced it.
    let oauth = database
        .mcp_servers()
        .get("mcp_oauth")
        .expect("stored row")
        .oauth
        .expect("OAuth config");
    assert_eq!(oauth.flow, McpOAuthFlow::DeviceCode);
    assert_eq!(oauth.client_id, "test-client");
    let credential_ref = CredentialRef::parse(oauth.credential_ref.as_deref().expect("reference"))
        .expect("credential reference");
    let bundle: OAuthTokenBundle = serde_json::from_str(
        credentials
            .get(&credential_ref)
            .expect("stored bundle")
            .as_utf8()
            .expect("UTF-8 bundle")
            .as_ref(),
    )
    .expect("valid bundle");
    assert_eq!(bundle.access_token, "device-access");
    assert_eq!(bundle.refresh_token.as_deref(), Some("device-refresh"));

    let row = database.mcp_servers().get("mcp_oauth").expect("row");
    let source = source_for_row(credentials.clone(), &row, endpoint);
    assert_eq!(token_of(&source, false).await, "access-two");

    let polls = device.polls();
    assert_eq!(polls.len(), 2, "one poll per scripted answer");
    assert!(
        polls[1] - polls[0] >= DEVICE_MIN_POLL.as_millis() as u64,
        "the client must wait between polls instead of spinning: {polls:?}"
    );
    assert_eq!(
        server.requests(),
        vec![
            (
                "GET".to_string(),
                "/.well-known/oauth-authorization-server".to_string()
            ),
            ("POST".to_string(), "/device".to_string()),
            ("POST".to_string(), "/token".to_string()),
            ("POST".to_string(), "/token".to_string()),
            ("POST".to_string(), "/token".to_string()),
        ],
        "device authorization, then one poll per answer, then the refresh"
    );
    cleanup(&path);
}

#[tokio::test]
async fn slow_down_keeps_waiting_and_the_deadline_ends_the_flow() {
    // Two properties in one fast fixture. `slow_down` must not be fatal: the flow polls
    // again instead of aborting. And the wait it asks for is *bounded by the code's own
    // lifetime*: a one-second `expires_in` means the shortened wait happens and then the
    // flow stops, rather than polling forever. The exact +5 s the RFC specifies is
    // pinned by `slow_down_adds_five_seconds` below, since asserting it here would mean
    // a five-second test for an arithmetic constant.
    let device = Arc::new(ServerScript::with_steps(&[
        DeviceStep::SlowDown,
        DeviceStep::Pending,
    ]));
    *device.lifetime.lock().expect("lifetime") = 1;
    let server = TestAuthorizationServer::with_script(device.clone());
    let (path, database) = temp_db("device-slow-down");
    insert_device_row(&database, &server, "test-client");
    let credentials = Arc::new(MemoryCredentialStore::new());

    let error = connect(
        &database,
        credentials,
        "mcp_oauth",
        OutboundProxy::default(),
        noop_launcher(),
        noop_prompt(),
        CancellationToken::new(),
    )
    .await
    .expect_err("a code that is never approved must end the flow");

    assert!(
        error.contains("expired"),
        "the deadline must be reported as expiry, not as a slow_down failure: {error}"
    );
    assert_eq!(
        device.polls().len(),
        2,
        "the poll after slow_down must happen; the one after the deadline must not"
    );
    assert!(database
        .mcp_servers()
        .get("mcp_oauth")
        .expect("row")
        .oauth
        .expect("OAuth config")
        .credential_ref
        .is_none());
    cleanup(&path);
}

#[tokio::test]
async fn a_refusal_and_an_expired_code_are_reported_as_themselves() {
    for (step, expected) in [
        (DeviceStep::Denied, "access_denied"),
        (DeviceStep::Expired, "expired_token"),
    ] {
        let device = Arc::new(ServerScript::with_steps(&[step]));
        let server = TestAuthorizationServer::with_script(device);
        let (path, database) = temp_db("device-terminal");
        insert_device_row(&database, &server, "test-client");
        let credentials = Arc::new(MemoryCredentialStore::new());

        let error = connect(
            &database,
            credentials.clone(),
            "mcp_oauth",
            OutboundProxy::default(),
            noop_launcher(),
            noop_prompt(),
            CancellationToken::new(),
        )
        .await
        .expect_err("a terminal device answer must fail the flow");

        assert!(error.contains(expected), "{step:?}: {error}");
        let oauth = database
            .mcp_servers()
            .get("mcp_oauth")
            .expect("row")
            .oauth
            .expect("OAuth config");
        assert!(
            oauth.credential_ref.is_none() && oauth.token_endpoint.is_none(),
            "{step:?}: a failed flow must not leave a half-connected row"
        );
        cleanup(&path);
    }
}

#[tokio::test]
async fn cancelling_a_device_flow_stops_the_polling_and_stores_nothing() {
    let device = Arc::new(ServerScript::with_steps(&[DeviceStep::Pending]));
    let server = TestAuthorizationServer::with_script(device.clone());
    let (path, database) = temp_db("device-cancel");
    insert_device_row(&database, &server, "test-client");
    let credentials = Arc::new(MemoryCredentialStore::new());
    let cancel = CancellationToken::new();

    let flow = connect(
        &database,
        credentials.clone(),
        "mcp_oauth",
        OutboundProxy::default(),
        noop_launcher(),
        noop_prompt(),
        cancel.clone(),
    );
    tokio::pin!(flow);
    let polled = async {
        while device.polls().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::select! {
        _ = polled => {}
        result = &mut flow => panic!("the flow ended before it could be cancelled: {result:?}"),
    }

    cancel.cancel();
    let error = flow
        .await
        .expect_err("a cancelled flow must not report success");
    assert_eq!(error, CANCELLED_FLOW);

    // The claim is "no background poller", so it has to survive a wait: were the loop
    // detached, this is where it would keep asking.
    let polls = device.polls().len();
    tokio::time::sleep(4 * DEVICE_MIN_POLL).await;
    assert_eq!(
        device.polls().len(),
        polls,
        "a cancelled flow must not keep polling"
    );
    // The row is the observable half: a credential nothing points at cannot be read
    // back (the store has no enumeration), so "no token was kept" means no reference.
    assert!(
        database
            .mcp_servers()
            .get("mcp_oauth")
            .expect("row")
            .oauth
            .expect("OAuth config")
            .credential_ref
            .is_none(),
        "a cancelled flow must not leave a token reference behind"
    );
    cleanup(&path);
}

#[tokio::test]
async fn a_server_without_a_device_endpoint_refuses_instead_of_falling_back() {
    let server = TestAuthorizationServer::start();
    *server.script().supported.lock().expect("supported") = false;
    let (path, database) = temp_db("device-unsupported");
    insert_device_row(&database, &server, "test-client");
    let credentials = Arc::new(MemoryCredentialStore::new());

    let error = connect(
        &database,
        credentials,
        "mcp_oauth",
        OutboundProxy::default(),
        noop_launcher(),
        noop_prompt(),
        CancellationToken::new(),
    )
    .await
    .expect_err("a server without the endpoint must refuse the device flow");

    assert!(error.contains("device_authorization_endpoint"), "{error}");
    assert!(
        error.contains("authorization code"),
        "the refusal must name the alternative: {error}"
    );
    assert_eq!(
        server.requests(),
        vec![(
            "GET".to_string(),
            "/.well-known/oauth-authorization-server".to_string()
        )],
        "no device request may be attempted against a server that cannot serve one"
    );
    cleanup(&path);
}

#[tokio::test]
async fn a_registration_that_answers_with_a_secret_is_refused_not_downgraded() {
    // The user configured no secret and left the client id empty, which asks for a public
    // client. A server that answers with a secret is telling us it did not register one —
    // adopting the value would silently make this a confidential client that cannot keep
    // its credential, so the flow stops instead.
    let script = Arc::new(ServerScript::new());
    *script.registration_secret.lock().expect("secret") = Some("server-issued".to_string());
    let server = TestAuthorizationServer::with_script(script);
    let (path, database) = temp_db("registration-secret");
    insert_row(
        &database,
        &server,
        McpOAuthFlow::AuthorizationCode,
        "",
        McpOAuthClientAuth::None,
        None,
    );
    let credentials = Arc::new(MemoryCredentialStore::new());

    let error = connect(
        &database,
        credentials,
        "mcp_oauth",
        OutboundProxy::default(),
        noop_launcher(),
        noop_prompt(),
        CancellationToken::new(),
    )
    .await
    .expect_err("a public client must refuse a secret it cannot protect");

    assert!(
        error.contains("client secret"),
        "the refusal must name what was issued: {error}"
    );
    assert_eq!(
        server.requests(),
        vec![
            (
                "GET".to_string(),
                "/.well-known/oauth-authorization-server".to_string()
            ),
            ("POST".to_string(), "/register".to_string()),
        ],
        "the refusal happens at registration: no token request may follow"
    );
    let oauth = database
        .mcp_servers()
        .get("mcp_oauth")
        .expect("row")
        .oauth
        .expect("OAuth config");
    assert_eq!(oauth.client_auth, McpOAuthClientAuth::None);
    assert!(oauth.client_secret_ref.is_none());
    cleanup(&path);
}

#[tokio::test]
async fn an_empty_client_id_registers_a_device_grant_public_client() {
    let server = TestAuthorizationServer::start();
    server
        .script()
        .expect_client("dynamic-client", None, ClientAuthVia::Body);
    let (path, database) = temp_db("device-registration");
    insert_device_row(&database, &server, "");
    let credentials = Arc::new(MemoryCredentialStore::new());

    connect(
        &database,
        credentials,
        "mcp_oauth",
        OutboundProxy::default(),
        noop_launcher(),
        noop_prompt(),
        CancellationToken::new(),
    )
    .await
    .expect("dynamic registration must complete the device flow");

    let registrations = server.registrations();
    assert_eq!(registrations.len(), 1);
    assert_eq!(
        registrations[0]["grant_types"],
        json!([DEVICE_CODE_GRANT, "refresh_token"])
    );
    assert!(
        registrations[0].get("redirect_uris").is_none(),
        "a device client has no callback to register: {}",
        registrations[0]
    );
    assert_eq!(
        registrations[0]["token_endpoint_auth_method"].as_str(),
        Some("none"),
        "the device flow is still a public client"
    );
    assert_eq!(
        database
            .mcp_servers()
            .get("mcp_oauth")
            .expect("row")
            .oauth
            .expect("OAuth config")
            .client_id,
        "dynamic-client"
    );
    cleanup(&path);
}

/* ── P0-2：客户端密钥（client_secret_post / client_secret_basic） ────────── */

#[test]
fn the_test_servers_base64_decoder_matches_the_rfc_7617_example() {
    // RFC 7617 §2: "Aladdin:open sesame" → QWxhZGRpbjpvcGVuIHNlc2FtZQ==
    assert_eq!(
        decode_base64("QWxhZGRpbjpvcGVuIHNlc2FtZQ==").expect("base64"),
        b"Aladdin:open sesame".to_vec()
    );
    assert_eq!(
        decode_base64("dGVzdC1jbGllbnQ6czNjcmV0").expect("base64"),
        b"test-client:s3cret".to_vec()
    );
    assert!(
        decode_base64("not base64!").is_none(),
        "a header that is not base64 must not decode into something plausible"
    );
}

/// Both secret methods run the same flow; only the place the credentials travel differs.
async fn assert_secret_flow(method: McpOAuthClientAuth, via: ClientAuthVia, name: &str) {
    let server = TestAuthorizationServer::start();
    server
        .script()
        .expect_client("confidential-client", Some("s3cret-value"), via);
    let (path, database) = temp_db(name);
    let credentials = Arc::new(MemoryCredentialStore::new());
    let secret_reference = insert_secret_flow_row(
        &database,
        &server,
        &credentials,
        McpOAuthFlow::AuthorizationCode,
        method,
        "s3cret-value",
    );

    connect(
        &database,
        credentials.clone(),
        "mcp_oauth",
        OutboundProxy::default(),
        browser_launcher(),
        noop_prompt(),
        CancellationToken::new(),
    )
    .await
    .expect("a confidential client must be able to complete the code flow");

    let row = database.mcp_servers().get("mcp_oauth").expect("stored row");
    let oauth = row.oauth.clone().expect("OAuth config");
    assert_eq!(oauth.client_auth, method);
    assert_eq!(
        oauth.client_secret_ref.as_deref(),
        Some(secret_reference.to_string_ref().as_str()),
        "the row must keep pointing at the stored secret"
    );

    // The refresh is a second, independent token request: it authenticates the same way,
    // or the server sees an anonymous renewal of a confidential client. The fixture fails
    // the request if the credentials are missing or in the wrong place.
    let source = source_for_row(
        credentials.clone(),
        &row,
        format!("http://{}/mcp", server.address),
    );
    assert_eq!(token_of(&source, false).await, "access-two");
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|(_, target)| target == "/token")
            .count(),
        2,
        "one code exchange and one refresh"
    );
    cleanup(&path);
}

#[tokio::test]
async fn client_secret_post_authenticates_the_exchange_and_the_refresh() {
    assert_secret_flow(
        McpOAuthClientAuth::ClientSecretPost,
        ClientAuthVia::Body,
        "client-secret-post",
    )
    .await;
}

#[tokio::test]
async fn client_secret_basic_authenticates_the_exchange_and_the_refresh() {
    assert_secret_flow(
        McpOAuthClientAuth::ClientSecretBasic,
        ClientAuthVia::Header,
        "client-secret-basic",
    )
    .await;
}

#[tokio::test]
async fn a_secret_method_without_a_stored_secret_refuses_before_any_token_request() {
    let server = TestAuthorizationServer::start();
    let (path, database) = temp_db("client-secret-missing");
    let credentials = Arc::new(MemoryCredentialStore::new());
    insert_row(
        &database,
        &server,
        McpOAuthFlow::AuthorizationCode,
        "confidential-client",
        McpOAuthClientAuth::ClientSecretPost,
        None,
    );

    let error = connect(
        &database,
        credentials,
        "mcp_oauth",
        OutboundProxy::default(),
        noop_launcher(),
        noop_prompt(),
        CancellationToken::new(),
    )
    .await
    .expect_err("a confidential client without its secret cannot authenticate");

    assert!(
        error.contains("secret") && error.contains("enter it again"),
        "the refusal must say what is missing and what to do: {error}"
    );
    assert_eq!(
        server.requests(),
        vec![(
            "GET".to_string(),
            "/.well-known/oauth-authorization-server".to_string()
        )],
        "discovery is the only request: nothing may be sent without the credential"
    );
    cleanup(&path);
}

#[tokio::test]
async fn the_device_flow_authenticates_the_device_authorization_and_every_poll() {
    let script = Arc::new(ServerScript::with_steps(&[
        DeviceStep::Pending,
        DeviceStep::Grant,
    ]));
    script.expect_client(
        "confidential-client",
        Some("s3cret-value"),
        ClientAuthVia::Body,
    );
    let server = TestAuthorizationServer::with_script(script.clone());
    let (path, database) = temp_db("device-client-secret");
    let credentials = Arc::new(MemoryCredentialStore::new());
    insert_secret_flow_row(
        &database,
        &server,
        &credentials,
        McpOAuthFlow::DeviceCode,
        McpOAuthClientAuth::ClientSecretPost,
        "s3cret-value",
    );

    connect(
        &database,
        credentials,
        "mcp_oauth",
        OutboundProxy::default(),
        noop_launcher(),
        noop_prompt(),
        CancellationToken::new(),
    )
    .await
    .expect("a confidential client must be able to complete the device flow");

    assert_eq!(
        server.requests(),
        vec![
            (
                "GET".to_string(),
                "/.well-known/oauth-authorization-server".to_string()
            ),
            ("POST".to_string(), "/device".to_string()),
            ("POST".to_string(), "/token".to_string()),
            ("POST".to_string(), "/token".to_string()),
        ],
        "the device authorization and each poll must carry the client credentials"
    );
    cleanup(&path);
}

#[test]
fn a_server_interval_is_honoured_but_bounded_at_both_ends() {
    assert_eq!(
        device_poll_interval(None),
        Duration::from_secs(DEVICE_DEFAULT_POLL_SECONDS),
        "RFC 8628's default is five seconds"
    );
    assert_eq!(device_poll_interval(Some(12)), Duration::from_secs(12));
    assert_eq!(
        device_poll_interval(Some(0)),
        DEVICE_MIN_POLL,
        "\"poll immediately\" must still not be a hot loop"
    );
    assert_eq!(
        device_poll_interval(Some(9_999)),
        DEVICE_MAX_POLL,
        "a server cannot make one click wait for hours"
    );
}

#[test]
fn slow_down_adds_five_seconds() {
    assert_eq!(
        apply_slow_down(Duration::from_millis(250)),
        Duration::from_millis(5_250)
    );
    assert_eq!(
        apply_slow_down(Duration::from_secs(5)),
        Duration::from_secs(10)
    );
    assert_eq!(
        apply_slow_down(DEVICE_MAX_POLL),
        DEVICE_MAX_POLL,
        "the back-off is capped, not unbounded"
    );
}

#[test]
fn only_the_complete_verification_uri_may_carry_a_query() {
    assert_eq!(
        validate_verification_uri_complete(
            "mcp_1",
            "https://auth.example.com/device?user_code=WDJB-MJHT"
        )
        .expect("the code lives in the query"),
        "https://auth.example.com/device?user_code=WDJB-MJHT"
    );
    // Everything else about the origin follows the rules every other OAuth URL follows.
    for rejected in [
        "http://auth.example.com/device?user_code=x",
        "https://user:pass@auth.example.com/device?user_code=x",
        "https://auth.example.com/device?user_code=x#fragment",
    ] {
        assert!(
            validate_verification_uri_complete("mcp_1", rejected).is_err(),
            "{rejected} must be refused"
        );
    }
}

async fn send_callback_request(address: SocketAddr, target: &str) -> Vec<u8> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpStream as TokioTcpStream;

    let mut stream = TokioTcpStream::connect(address)
        .await
        .expect("connect callback listener");
    let request = format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write callback request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("read callback response");
    response
}

#[tokio::test]
async fn authorization_error_callback_requires_matching_state() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind callback listener");
    let address = listener.local_addr().expect("callback address");
    let client = tokio::spawn(send_callback_request(
        address,
        "/callback?error=access_denied&state=attacker",
    ));

    let error = wait_for_callback(listener, "expected", &CancellationToken::new())
        .await
        .expect_err("a callback with the wrong state must be rejected");
    assert_eq!(error, "OAuth callback state did not match");
    let response = client.await.expect("callback client task");
    assert!(response.starts_with(b"HTTP/1.1 400"));
}

#[tokio::test]
async fn authorization_error_callback_sanitizes_untrusted_details() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind callback listener");
    let address = listener.local_addr().expect("callback address");
    let client = tokio::spawn(send_callback_request(
        address,
        "/callback?error=access_denied&error_description=bad%0Aline%00&state=expected",
    ));

    let error = wait_for_callback(listener, "expected", &CancellationToken::new())
        .await
        .expect_err("an OAuth error callback must finish the flow");
    assert_eq!(error, "OAuth authorization failed: access_denied: badline");
    assert!(error.chars().all(|character| !character.is_control()));
    let response = client.await.expect("callback client task");
    assert!(response.starts_with(b"HTTP/1.1 400"));
}
