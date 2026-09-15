//! 握手期 host key 行为的**进程内**集成测试（不依赖真机、不需要环境变量）。
//!
//! ## 为什么要这个文件
//!
//! 探针、指纹不匹配、TOFU 记录这几条长期以来只有两条出路：要么在真机上测
//! （`tests/live.rs`，被 `YUKINAL_SSH_TEST_*` 门控，CI 里默认全部跳过），要么只测到
//! 边缘 —— 于是「探针真的会返回服务器出示的指纹吗」「不匹配真的会被 `check_server_key`
//! 拦下并且带着两个指纹吗」这类问题，在默认的 `cargo test` 里没有答案。
//!
//! 其实不需要真机：russh 自带 server 侧实现，所以这里在 127.0.0.1 上起一台**真的**
//! SSH 服务器（真的 key exchange、真的握手、真的认证），然后从客户端一侧验证这些行为。
//! 真机测试仍然有价值 —— 它们覆盖真实服务器的算法组合、真实的认证材料、keepalive 与
//! 网络行为 —— 但它们不该是这些问题的唯一答案。
//!
//! ## 边界
//!
//! 这里测的是**握手与指纹**，不测真实服务器会怎么装配（那是 `tests/live.rs`）。
//! 每台测试服务器只监听回环地址的一个临时端口，跑完就随进程结束。

use std::borrow::Cow;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use russh::keys::{ssh_key, HashAlg};
use russh::server::{Auth, Response, RunningServerHandle, Server as _};
use russh::{MethodKind, MethodSet};
use tokio::net::TcpListener;

use yukinal_ssh::known_hosts::{ForgetOutcome, TrustDecision};
use yukinal_ssh::{
    Authentication, ConnectionSecrets, Error, HostCertificateAuthority,
    KeyboardInteractiveChallenge, KeyboardInteractiveHandler, KnownHostsPolicy, OutboundProxy,
    Result as SshResult, RusshBackend, SshBackend, SshConfig,
};

/* ── 测试服务器 ─────────────────────────────────────────────────────────────── */

/// 服务器侧的观察点：探针/连接对服务器做了什么，只有服务器自己知道。
#[derive(Clone, Default)]
struct Observed {
    /// 每一次被接受的连接（`new_client`）。用来证明「本地就拒绝了」——
    /// 计数器为 0 意味着一个字节都没发出去。
    connections: Arc<AtomicUsize>,
    /// 认证回调被调用的用户。探针**不该**出现在这里。
    auth_attempts: Arc<Mutex<Vec<String>>>,
    interactive_rounds: Arc<AtomicUsize>,
}

struct TestServer {
    addr: SocketAddr,
    /// 服务器自己的 host key 指纹，由测试独立算出 —— 客户端报出来的必须与它一致。
    fingerprint: String,
    certificate_authority: Option<ssh_key::PublicKey>,
    certificate_authority_private: Option<ssh_key::PrivateKey>,
    observed: Observed,
    shutdown: RunningServerHandle,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown.shutdown("test over".to_string());
    }
}

/// 起一台真的 SSH 服务器：`accept_auth` 决定它是否接受密码认证。
async fn start_server(accept_auth: bool) -> TestServer {
    start_server_with_mode(accept_auth, false, false).await
}

async fn start_mfa_server() -> TestServer {
    start_server_with_mode(true, true, false).await
}

async fn start_certificate_server() -> TestServer {
    start_server_with_mode(true, false, true).await
}

async fn start_server_with_mode(
    accept_auth: bool,
    require_mfa: bool,
    issue_host_certificate: bool,
) -> TestServer {
    let host_key = ssh_key::PrivateKey::random(&mut rand::rng(), ssh_key::Algorithm::Ed25519)
        .expect("host key");
    let fingerprint = host_key
        .public_key()
        .fingerprint(HashAlg::Sha256)
        .to_string();

    let mut config = russh::server::Config::default();
    let certificate_authority_private = issue_host_certificate.then(|| {
        let ca = ssh_key::PrivateKey::random(&mut rand::rng(), ssh_key::Algorithm::Ed25519)
            .expect("CA key");
        config
            .certificates
            .push(host_certificate(&ca, &host_key, &["127.0.0.1"]));
        ca
    });
    let certificate_authority = certificate_authority_private
        .as_ref()
        .map(|ca| ca.public_key().clone());
    config.keys.push(host_key);
    // 一台只服务本测试的服务器：不做无谓的拒绝延迟，也不要因为空闲而被回收。
    config.auth_rejection_time = std::time::Duration::from_millis(10);
    config.inactivity_timeout = None;
    let config = Arc::new(config);

    // listener / 服务器实例都搬进后台任务：`run_on_socket` 借的是它们（还有 config），
    // 借用不能跨出 `spawn`，所以「起服务」与「读地址和关闭句柄」之间用一个 oneshot 交接。
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");

    let observed = Observed::default();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let server_observed = observed.clone();
    tokio::spawn(async move {
        let mut server = TestSshServer {
            observed: server_observed,
            accept_auth,
            require_mfa,
        };
        let running = server.run_on_socket(config, &listener);
        ready_tx.send(running.handle()).ok();
        let _ = running.await;
    });
    let shutdown = ready_rx.await.expect("server ready");

    TestServer {
        addr,
        fingerprint,
        certificate_authority,
        certificate_authority_private,
        observed,
        shutdown,
    }
}

fn host_certificate(
    ca: &ssh_key::PrivateKey,
    host: &ssh_key::PrivateKey,
    principals: &[&str],
) -> ssh_key::Certificate {
    use ssh_key::certificate::{Builder, CertType};
    let mut builder =
        Builder::new_with_random_nonce(&mut rand::rng(), host.public_key(), 0, u64::MAX)
            .expect("certificate builder");
    builder.serial(1).expect("serial");
    builder.key_id("yukinal-host-test").expect("key id");
    builder.cert_type(CertType::Host).expect("cert type");
    for principal in principals {
        builder.valid_principal(*principal).expect("principal");
    }
    builder.sign(ca).expect("sign certificate")
}

fn krl_revoking_host_serial(ca: &ssh_key::PublicKey, serial: u64) -> Vec<u8> {
    fn string(bytes: &[u8]) -> Vec<u8> {
        let mut encoded = (bytes.len() as u32).to_be_bytes().to_vec();
        encoded.extend_from_slice(bytes);
        encoded
    }

    let mut section = string(&ca.to_bytes().expect("CA blob"));
    section.extend_from_slice(&string(b""));
    section.push(0x20);
    section.extend_from_slice(&string(&serial.to_be_bytes()));

    let mut krl = b"SSHKRL\n\0".to_vec();
    krl.extend_from_slice(&1_u32.to_be_bytes());
    krl.extend_from_slice(&1_u64.to_be_bytes());
    krl.extend_from_slice(&0_u64.to_be_bytes());
    krl.extend_from_slice(&0_u64.to_be_bytes());
    krl.extend_from_slice(&string(b""));
    krl.extend_from_slice(&string(b"test"));
    krl.push(1);
    krl.extend_from_slice(&string(&section));
    krl
}

fn signed_krl_revoking_host_serial(
    certificate_ca: &ssh_key::PublicKey,
    signer: &ssh_key::PrivateKey,
    serial: u64,
) -> Vec<u8> {
    use signature::Signer as _;

    fn string(bytes: &[u8]) -> Vec<u8> {
        let mut encoded = (bytes.len() as u32).to_be_bytes().to_vec();
        encoded.extend_from_slice(bytes);
        encoded
    }

    let mut krl = krl_revoking_host_serial(certificate_ca, serial);
    krl.push(4);
    krl.extend_from_slice(&string(
        &signer
            .public_key()
            .to_bytes()
            .expect("signature public key"),
    ));
    let signed_len = krl.len();
    let signature: ssh_key::Signature = signer
        .try_sign(&krl[..signed_len])
        .expect("sign KRL fixture");
    let encoded = Vec::<u8>::try_from(signature).expect("encode signature");
    krl.extend_from_slice(&string(&encoded));
    krl
}

fn serve_krl_once(bytes: Vec<u8>) -> (String, std::thread::JoinHandle<String>) {
    use std::io::{Read as _, Write as _};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind KRL server");
    let address = listener.local_addr().expect("KRL server address");
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept KRL request");
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .expect("KRL request timeout");
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1_024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).expect("read KRL request");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
        }
        let request_line = String::from_utf8_lossy(&request)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            bytes.len()
        )
        .expect("write KRL response headers");
        stream.write_all(&bytes).expect("write KRL response body");
        stream.flush().expect("flush KRL response");
        request_line
    });
    (format!("http://{address}/revoked.krl"), handle)
}

struct TestSshServer {
    observed: Observed,
    accept_auth: bool,
    require_mfa: bool,
}

impl russh::server::Server for TestSshServer {
    type Handler = TestSshHandler;

    fn new_client(&mut self, _peer_addr: Option<SocketAddr>) -> Self::Handler {
        self.observed.connections.fetch_add(1, Ordering::SeqCst);
        TestSshHandler {
            observed: self.observed.clone(),
            accept_auth: self.accept_auth,
            require_mfa: self.require_mfa,
        }
    }
}

struct TestSshHandler {
    observed: Observed,
    accept_auth: bool,
    require_mfa: bool,
}

impl russh::server::Handler for TestSshHandler {
    type Error = russh::Error;

    async fn auth_password(
        &mut self,
        user: &str,
        _password: &str,
    ) -> std::result::Result<Auth, Self::Error> {
        self.observed
            .auth_attempts
            .lock()
            .expect("auth attempts")
            .push(user.to_string());
        Ok(if self.accept_auth {
            if self.require_mfa {
                Auth::Reject {
                    proceed_with_methods: Some(MethodSet::from(
                        &[MethodKind::KeyboardInteractive][..],
                    )),
                    partial_success: true,
                }
            } else {
                Auth::Accept
            }
        } else {
            Auth::reject()
        })
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        _user: &str,
        _submethods: &str,
        response: Option<Response<'a>>,
    ) -> std::result::Result<Auth, Self::Error> {
        self.observed
            .interactive_rounds
            .fetch_add(1, Ordering::SeqCst);
        let Some(mut response) = response else {
            return Ok(Auth::Partial {
                name: Cow::Borrowed("Two-factor authentication"),
                instructions: Cow::Borrowed("Enter the test verification code."),
                prompts: Cow::Borrowed(&[(Cow::Borrowed("Verification code"), false)]),
            });
        };
        let answer = response
            .next()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default();
        Ok(if answer == "654321" {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }
}

struct TestKeyboardInteractiveHandler;

impl KeyboardInteractiveHandler for TestKeyboardInteractiveHandler {
    fn respond<'a>(
        &'a self,
        challenge: &'a KeyboardInteractiveChallenge,
    ) -> Pin<Box<dyn Future<Output = SshResult<Vec<String>>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(challenge.name, "Two-factor authentication");
            assert_eq!(challenge.prompts.len(), 1);
            assert!(!challenge.prompts[0].echo);
            Ok(challenge
                .prompts
                .iter()
                .map(|_| "654321".to_string())
                .collect())
        })
    }
}

/* ── 客户端侧的固定装置 ─────────────────────────────────────────────────────── */

/// 一次性数据目录：`Drop` 时删掉（`known_hosts` 就在里面）。
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "yukinal-ssh-handshake-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn backend(&self) -> RusshBackend {
        RusshBackend::from_data_dir(&self.0).expect("backend")
    }

    fn store_path(&self) -> std::path::PathBuf {
        self.0.join("known_hosts")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn config(server: &TestServer, policy: KnownHostsPolicy) -> SshConfig {
    SshConfig {
        server_id: "srv_handshake".into(),
        host: "127.0.0.1".into(),
        port: server.addr.port(),
        username: "yukinal".into(),
        authentication: Authentication::Password {
            credential_ref: "keychain://ssh/handshake".into(),
        },
        host_certificate_authority: None,
        outbound_proxy: OutboundProxy::default(),
        known_hosts_policy: policy,
        keepalive_interval_secs: 0,
    }
}

fn secrets() -> ConnectionSecrets {
    ConnectionSecrets {
        password: Some("hunter2".into()),
        private_key_pem: None,
        private_key_passphrase: None,
        keyboard_interactive: None,
    }
}

/// 认一个与真实指纹不同的钉子，模拟「服务器换过 key 而本地还留着旧钉子」。
const WRONG_FINGERPRINT: &str = "SHA256:AAAABBBBCCCCDDDDEEEEFFFFGGGGHHHHIIIIJJJJKKK";

/* ── 探针 ──────────────────────────────────────────────────────────────────── */

/// **探针返回服务器真的出示过的指纹**，而且不写任何东西、不认证。
///
/// 三件事在同一条测试里，因为它们共同定义了「探针」：一个真的答案、零副作用、
/// 与认证无关。任何一件单独成立都不够 —— 探针写了 pin 就不再是「看一眼」，
/// 探针认证了就不再是「不需要凭据的操作」（ADR 0012 第 2、4 条）。
#[tokio::test]
async fn probe_reports_the_presented_fingerprint_and_persists_nothing() {
    let server = start_server(true).await;
    let dir = TempDir::new("probe");
    let backend = dir.backend();

    let probe = backend
        .probe_host_key("127.0.0.1", server.addr.port())
        .await
        .expect("probe");

    assert_eq!(
        probe.fingerprint, server.fingerprint,
        "探针给的必须是服务器这次真的出示的指纹",
    );
    assert_eq!(probe.host, "127.0.0.1");
    assert_eq!(probe.port, server.addr.port());

    // 不持久化：内存与磁盘都没有多出任何东西。
    assert_eq!(
        backend
            .host_key_pin("127.0.0.1", server.addr.port())
            .expect("pin"),
        None,
        "探针不得顺手把 pin 写进去",
    );
    assert!(
        !dir.store_path().exists(),
        "探针不得创建 known_hosts 文件：看一眼不应该有副作用",
    );

    // 不认证：服务器从没被要求验证过任何身份。
    assert!(
        server
            .observed
            .auth_attempts
            .lock()
            .expect("auth attempts")
            .is_empty(),
        "探针不得认证",
    );

    // 连了一次，而且只有一次。
    assert_eq!(server.observed.connections.load(Ordering::SeqCst), 1);
}

/// 探针不改动已有的钉子（连「顺手刷新一下」都不做）。
#[tokio::test]
async fn probe_leaves_an_existing_pin_alone() {
    let server = start_server(true).await;
    let dir = TempDir::new("probe-existing");
    let backend = dir.backend();
    let port = server.addr.port();

    backend
        .trust_host("127.0.0.1", port, &server.fingerprint)
        .expect("pin");
    let before = std::fs::read_to_string(dir.store_path()).expect("store file");

    let probe = backend
        .probe_host_key("127.0.0.1", port)
        .await
        .expect("probe");
    assert_eq!(probe.fingerprint, server.fingerprint);
    assert_eq!(
        std::fs::read_to_string(dir.store_path()).expect("store file"),
        before,
        "探针前后 known_hosts 必须逐字节相同",
    );
}

/* ── 不匹配：两个指纹都要有 ─────────────────────────────────────────────────── */

/// **指纹不匹配由真实握手报出，且错误里同时有钉住的与出示的指纹。**
///
/// 这是 ADR 0012 第 3 条在代码里的落点：`check_server_key` 只返回 `bool`，指纹在
/// 那里就消失了 —— 这条测试证明它现在不会消失，而且 `presented` 确实是服务器这次
/// 出示的那个（与服务器自己的 host key 指纹逐字节相同），不是本地编出来的。
#[tokio::test]
async fn a_pinned_mismatch_is_reported_with_both_fingerprints() {
    let server = start_server(true).await;
    let dir = TempDir::new("mismatch");
    let backend = dir.backend();
    let port = server.addr.port();

    // 记下一条与服务器不同的钉子（TOFU 之后服务器换 key 就是这个状态）。
    backend
        .trust_host("127.0.0.1", port, WRONG_FINGERPRINT)
        .expect("pin");

    let result = backend
        .connect(config(&server, KnownHostsPolicy::RequireMatch), secrets())
        .await;

    match result {
        Err(Error::HostKeyVerification {
            host,
            pinned,
            presented,
        }) => {
            assert_eq!(host, "127.0.0.1");
            assert_eq!(pinned, WRONG_FINGERPRINT);
            assert_eq!(
                presented, server.fingerprint,
                "出示的指纹必须是服务器真的出示的那个",
            );
            assert_ne!(pinned, presented);
        }
        other => panic!("钉住的和出示的不一致必须报 HostKeyVerification，got {other:?}"),
    }

    // 阻断发生在认证之前：即使服务器会接受密码，也没有任何身份被送出去。
    // （`check_server_key` 在 key exchange 里，握手没走完就没有认证可言。）
    assert!(
        server
            .observed
            .auth_attempts
            .lock()
            .expect("auth attempts")
            .is_empty(),
        "握手都中止了，不该有认证尝试",
    );

    // 钉子没有被那次失败改掉。
    assert_eq!(
        backend.host_key_pin("127.0.0.1", port).expect("pin"),
        Some(WRONG_FINGERPRINT.to_string()),
    );
}

/// 与钉子一致时连接继续走完认证 —— 不匹配那条错误不是「一律拒绝」的伪装。
#[tokio::test]
async fn a_matching_pin_lets_the_connection_authenticate() {
    let server = start_server(true).await;
    let dir = TempDir::new("matching");
    let backend = dir.backend();
    let port = server.addr.port();

    backend
        .trust_host("127.0.0.1", port, &server.fingerprint)
        .expect("pin");

    let session = backend
        .connect(config(&server, KnownHostsPolicy::RequireMatch), secrets())
        .await
        .expect("a pinned, matching host must connect");
    backend.close(&session).await.expect("close");

    assert_eq!(
        server
            .observed
            .auth_attempts
            .lock()
            .expect("auth attempts")
            .as_slice(),
        ["yukinal".to_string()],
    );
}

/* ── 未钉住：在 TCP 之前就拒绝 ──────────────────────────────────────────────── */

/// `RequireMatch` 下未钉住的主机在**一个字节发出去之前**就被拒绝。
///
/// 「TCP 之前」这句话以前只能靠一个不可路由的地址间接说明（连了会超时，所以得到
/// `HostKeyNotPinned` 说明没连）。这里直接问服务器：它一次 `new_client` 都没有发生过，
/// 所以客户端确实没有发起连接。
#[tokio::test]
async fn a_partial_password_login_completes_with_keyboard_interactive_mfa() {
    let server = start_mfa_server().await;
    let dir = TempDir::new("mfa");
    let backend = dir.backend();
    let mut secrets = secrets();
    secrets.keyboard_interactive = Some(Arc::new(TestKeyboardInteractiveHandler));

    let session = backend
        .connect(config(&server, KnownHostsPolicy::TrustOnFirstUse), secrets)
        .await
        .expect("the second factor must complete the login");
    backend.close(&session).await.expect("close");

    assert_eq!(
        server.observed.interactive_rounds.load(Ordering::SeqCst),
        2,
        "one InfoRequest and one response round are expected"
    );
}

#[tokio::test]
async fn an_explicit_host_ca_accepts_a_matching_certificate_without_a_leaf_pin() {
    let server = start_certificate_server().await;
    let dir = TempDir::new("host-ca");
    let backend = dir.backend();
    let mut config = config(&server, KnownHostsPolicy::RequireMatch);
    config.host_certificate_authority = Some(HostCertificateAuthority {
        ca_public_key: server
            .certificate_authority
            .as_ref()
            .expect("certificate authority")
            .to_openssh()
            .expect("encode CA"),
        principals: vec!["127.0.0.1".into()],
        revocation_list_path: None,
        revocation_list_url: None,
        revocation_list_signers: Vec::new(),
    });

    let session = backend
        .connect(config, secrets())
        .await
        .expect("a certificate signed by the configured CA must connect");
    backend.close(&session).await.expect("close");
}

#[tokio::test]
async fn an_explicit_host_ca_refuses_a_certificate_for_the_wrong_principal() {
    let server = start_certificate_server().await;
    let dir = TempDir::new("host-ca-wrong-principal");
    let backend = dir.backend();
    let mut config = config(&server, KnownHostsPolicy::RequireMatch);
    config.host_certificate_authority = Some(HostCertificateAuthority {
        ca_public_key: server
            .certificate_authority
            .as_ref()
            .expect("certificate authority")
            .to_openssh()
            .expect("encode CA"),
        principals: vec!["other.example.test".into()],
        revocation_list_path: None,
        revocation_list_url: None,
        revocation_list_signers: Vec::new(),
    });

    let error = backend
        .connect(config, secrets())
        .await
        .expect_err("a non-matching principal must be refused");
    assert!(
        matches!(error, Error::HostCertificate { ref host, .. } if host == "127.0.0.1"),
        "expected a host-certificate error, got {error:?}"
    );
}

#[tokio::test]
async fn an_explicit_host_ca_refuses_a_plain_host_key_downgrade() {
    let server = start_server(true).await;
    let dir = TempDir::new("host-ca-downgrade");
    let backend = dir.backend();
    let ca =
        ssh_key::PrivateKey::random(&mut rand::rng(), ssh_key::Algorithm::Ed25519).expect("CA key");
    let mut config = config(&server, KnownHostsPolicy::RequireMatch);
    config.host_certificate_authority = Some(HostCertificateAuthority {
        ca_public_key: ca.public_key().to_openssh().expect("encode CA"),
        principals: vec!["127.0.0.1".into()],
        revocation_list_path: None,
        revocation_list_url: None,
        revocation_list_signers: Vec::new(),
    });

    let error = backend
        .connect(config, secrets())
        .await
        .expect_err("a plain host key must not downgrade CA policy");
    assert!(
        matches!(error, Error::HostCertificate { ref host, .. } if host == "127.0.0.1"),
        "expected a host-certificate error, got {error:?}"
    );
}

#[tokio::test]
async fn an_explicit_host_ca_refuses_a_certificate_revoked_by_krl() {
    let server = start_certificate_server().await;
    let dir = TempDir::new("host-ca-krl");
    let backend = dir.backend();
    let krl_path = dir.path().join("revoked_hosts.krl");
    std::fs::write(
        &krl_path,
        krl_revoking_host_serial(
            server
                .certificate_authority
                .as_ref()
                .expect("certificate authority"),
            1,
        ),
    )
    .expect("write KRL");

    let mut config = config(&server, KnownHostsPolicy::RequireMatch);
    config.host_certificate_authority = Some(HostCertificateAuthority {
        ca_public_key: server
            .certificate_authority
            .as_ref()
            .expect("certificate authority")
            .to_openssh()
            .expect("encode CA"),
        principals: vec!["127.0.0.1".into()],
        revocation_list_path: Some(krl_path.to_string_lossy().into_owned()),
        revocation_list_url: None,
        revocation_list_signers: Vec::new(),
    });

    let error = backend
        .connect(config, secrets())
        .await
        .expect_err("a revoked host certificate must be refused");
    assert!(
        matches!(error, Error::HostCertificate { ref reason, .. } if reason.contains("revoked")),
        "expected a KRL revocation error, got {error:?}"
    );
}

#[tokio::test]
async fn an_explicit_host_ca_verifies_a_signed_krl_before_revoking_a_certificate() {
    let server = start_certificate_server().await;
    let dir = TempDir::new("host-ca-signed-krl");
    let backend = dir.backend();
    let krl_path = dir.path().join("signed_revoked_hosts.krl");
    let ca = server
        .certificate_authority_private
        .as_ref()
        .expect("certificate authority");
    std::fs::write(
        &krl_path,
        signed_krl_revoking_host_serial(
            server
                .certificate_authority
                .as_ref()
                .expect("certificate authority"),
            ca,
            1,
        ),
    )
    .expect("write signed KRL");

    let mut config = config(&server, KnownHostsPolicy::RequireMatch);
    config.host_certificate_authority = Some(HostCertificateAuthority {
        ca_public_key: server
            .certificate_authority
            .as_ref()
            .expect("certificate authority")
            .to_openssh()
            .expect("encode CA"),
        principals: vec!["127.0.0.1".into()],
        revocation_list_path: Some(krl_path.to_string_lossy().into_owned()),
        revocation_list_url: None,
        revocation_list_signers: Vec::new(),
    });

    let error = backend
        .connect(config, secrets())
        .await
        .expect_err("a valid signature must be verified before applying the revocation");
    assert!(
        matches!(error, Error::HostCertificate { ref reason, .. } if reason.contains("revoked")),
        "expected a signed-KRL revocation error, got {error:?}"
    );
}

#[tokio::test]
async fn an_independent_krl_signer_can_revoke_a_host_certificate() {
    let server = start_certificate_server().await;
    let dir = TempDir::new("host-ca-independent-krl-signer");
    let backend = dir.backend();
    let krl_path = dir.path().join("independently_signed_revoked_hosts.krl");
    let signer = ssh_key::PrivateKey::random(&mut rand::rng(), ssh_key::Algorithm::Ed25519)
        .expect("KRL key");
    std::fs::write(
        &krl_path,
        signed_krl_revoking_host_serial(
            server
                .certificate_authority
                .as_ref()
                .expect("certificate authority"),
            &signer,
            1,
        ),
    )
    .expect("write independently signed KRL");

    let mut config = config(&server, KnownHostsPolicy::RequireMatch);
    config.host_certificate_authority = Some(HostCertificateAuthority {
        ca_public_key: server
            .certificate_authority
            .as_ref()
            .expect("certificate authority")
            .to_openssh()
            .expect("encode CA"),
        principals: vec!["127.0.0.1".into()],
        revocation_list_path: Some(krl_path.to_string_lossy().into_owned()),
        revocation_list_url: None,
        revocation_list_signers: vec![signer
            .public_key()
            .to_openssh()
            .expect("encode independent KRL signer")],
    });

    let error = backend
        .connect(config, secrets())
        .await
        .expect_err("the independent signature must authorize the revocation");
    assert!(
        matches!(error, Error::HostCertificate { ref reason, .. } if reason.contains("revoked")),
        "expected an independently signed KRL revocation error, got {error:?}"
    );
}

#[tokio::test]
async fn an_explicit_host_ca_downloads_a_krl_over_loopback_http() {
    let server = start_certificate_server().await;
    let dir = TempDir::new("host-ca-krl-url");
    let backend = dir.backend();
    let krl = krl_revoking_host_serial(
        server
            .certificate_authority
            .as_ref()
            .expect("certificate authority"),
        1,
    );
    let (url, krl_server) = serve_krl_once(krl);

    let mut config = config(&server, KnownHostsPolicy::RequireMatch);
    config.host_certificate_authority = Some(HostCertificateAuthority {
        ca_public_key: server
            .certificate_authority
            .as_ref()
            .expect("certificate authority")
            .to_openssh()
            .expect("encode CA"),
        principals: vec!["127.0.0.1".into()],
        revocation_list_path: None,
        revocation_list_url: Some(url),
        revocation_list_signers: Vec::new(),
    });

    let error = backend
        .connect(config, secrets())
        .await
        .expect_err("a certificate revoked by the downloaded KRL must be refused");
    assert!(
        matches!(error, Error::HostCertificate { ref reason, .. } if reason.contains("revoked")),
        "expected a downloaded-KRL revocation error, got {error:?}"
    );
    assert_eq!(
        krl_server.join().expect("KRL server"),
        "GET /revoked.krl HTTP/1.1"
    );
}

#[tokio::test]
async fn an_unpinned_host_is_refused_before_any_connection_is_made() {
    let server = start_server(true).await;
    let dir = TempDir::new("unpinned");
    let backend = dir.backend();

    let result = backend
        .connect(config(&server, KnownHostsPolicy::RequireMatch), secrets())
        .await;

    assert!(
        matches!(
            result,
            Err(Error::HostKeyNotPinned { ref host, port }) if host == "127.0.0.1" && port == server.addr.port()
        ),
        "未钉住必须报 HostKeyNotPinned（不是 HostKeyVerification），got {result:?}",
    );

    // 给「万一它是异步的」留一点时间，再确认服务器确实没被打扰过。
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(
        server.observed.connections.load(Ordering::SeqCst),
        0,
        "未钉住是本地判断，不该去连一台我们本来就要拒绝的主机",
    );
}

/* ── TOFU 与遗忘 ───────────────────────────────────────────────────────────── */

/// TOFU 只在**认证成功之后**才记指纹：认证失败不留记录。
#[tokio::test]
async fn trust_on_first_use_records_only_after_a_successful_login() {
    let server = start_server(false).await;
    let dir = TempDir::new("tofu-refused");
    let backend = dir.backend();

    let result = backend
        .connect(
            config(&server, KnownHostsPolicy::TrustOnFirstUse),
            secrets(),
        )
        .await;
    assert!(
        matches!(result, Err(Error::Authentication(_))),
        "服务器拒绝认证就是认证失败，got {result:?}",
    );
    assert_eq!(
        backend
            .host_key_pin("127.0.0.1", server.addr.port())
            .expect("pin"),
        None,
        "认证没成功，什么都不该被记下来",
    );
    assert!(!dir.store_path().exists());
}

/// TOFU 成功连接后记下真实指纹；之后同样的连接按 pin 校验。
#[tokio::test]
async fn trust_on_first_use_records_the_presented_fingerprint() {
    let server = start_server(true).await;
    let dir = TempDir::new("tofu");
    let backend = dir.backend();
    let port = server.addr.port();

    let session = backend
        .connect(
            config(&server, KnownHostsPolicy::TrustOnFirstUse),
            secrets(),
        )
        .await
        .expect("first connect");
    backend.close(&session).await.expect("close");

    assert_eq!(
        backend.host_key_pin("127.0.0.1", port).expect("pin"),
        Some(server.fingerprint.clone()),
    );

    // 落盘了：换一个 backend 读同一个目录，同样看得到。
    let reloaded = dir.backend();
    assert_eq!(
        reloaded.host_key_pin("127.0.0.1", port).expect("pin"),
        Some(server.fingerprint.clone()),
    );
}

/// **遗忘之后回到 TOFU 的起点**：同样的 `RequireMatch` 连接重新变成「没钉子」。
#[tokio::test]
async fn forget_returns_the_host_to_trust_on_first_use() {
    let server = start_server(true).await;
    let dir = TempDir::new("forget");
    let backend = dir.backend();
    let port = server.addr.port();

    backend
        .trust_host("127.0.0.1", port, &server.fingerprint)
        .expect("pin");
    assert_eq!(
        backend.forget_host("127.0.0.1", port).expect("forget"),
        ForgetOutcome::Removed {
            fingerprint: server.fingerprint.clone()
        },
    );

    // 下一次连接回到 TOFU：RequireMatch 又拒绝了。
    let refused = backend
        .connect(config(&server, KnownHostsPolicy::RequireMatch), secrets())
        .await;
    assert!(
        matches!(refused, Err(Error::HostKeyNotPinned { .. })),
        "遗忘之后就没有钉子了，got {refused:?}",
    );

    // 而 TOFU 会重新记一遍 —— 这正是「遗忘是危险动作」的含义，界面上必须讲明白。
    let session = backend
        .connect(
            config(&server, KnownHostsPolicy::TrustOnFirstUse),
            secrets(),
        )
        .await
        .expect("tofu connect after forget");
    backend.close(&session).await.expect("close");
    assert_eq!(
        backend.host_key_pin("127.0.0.1", port).expect("pin"),
        Some(server.fingerprint.clone()),
    );
}

/// **`trust` 拒绝一个与已钉指纹不同的指纹，哪怕那个指纹确实是被服务器出示过的。**
///
/// 真实的服务器换了一次 host key 之后就是这个状态：探针拿回来的新指纹是**真的**
/// （这条测试里它来自一次真的握手），但「钉住它」这个动作仍然要被拒绝 —— 用户必须先
/// 遗忘旧钉子，再重新探针确认。ADR 0012 第 5 条要的正是这条：不接受任何「一次动作就
/// 接受新 key」的路径，哪怕新 key 是真的。
#[tokio::test]
async fn trust_refuses_a_different_fingerprint_even_when_it_was_really_presented() {
    let server = start_server(true).await;
    let dir = TempDir::new("trust-different");
    let backend = dir.backend();
    let port = server.addr.port();

    backend
        .trust_host("127.0.0.1", port, &server.fingerprint)
        .expect("pin");

    // 另一个真的出示指纹：来自另一台测试服务器的一次真探针，不是本地编的字符串。
    let other = start_server(true).await;
    let presented_elsewhere = backend
        .probe_host_key("127.0.0.1", other.addr.port())
        .await
        .expect("probe")
        .fingerprint;
    assert_ne!(presented_elsewhere, server.fingerprint);

    let decision = backend
        .trust_host("127.0.0.1", port, &presented_elsewhere)
        .expect("a refusal is not an IO failure");
    assert_eq!(
        decision,
        TrustDecision::RefusedDifferentPin {
            pinned: server.fingerprint.clone(),
            confirmed: presented_elsewhere.clone(),
        },
    );
    assert_eq!(
        backend.host_key_pin("127.0.0.1", port).expect("pin"),
        Some(server.fingerprint.clone()),
        "拒绝必须意味着什么都没写",
    );

    // 先遗忘再确认：这是唯一出路，而它现在通了。
    backend.forget_host("127.0.0.1", port).expect("forget");
    assert!(matches!(
        backend
            .trust_host("127.0.0.1", port, &presented_elsewhere)
            .expect("trust after forget"),
        TrustDecision::Pin { .. }
    ));
}
