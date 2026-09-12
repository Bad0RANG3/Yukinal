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

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use russh::keys::{ssh_key, HashAlg};
use russh::server::{Auth, RunningServerHandle, Server as _};
use tokio::net::TcpListener;

use yukinal_ssh::known_hosts::{ForgetOutcome, TrustDecision};
use yukinal_ssh::{
    Authentication, ConnectionSecrets, Error, KnownHostsPolicy, RusshBackend, SshBackend, SshConfig,
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
}

struct TestServer {
    addr: SocketAddr,
    /// 服务器自己的 host key 指纹，由测试独立算出 —— 客户端报出来的必须与它一致。
    fingerprint: String,
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
    let host_key = ssh_key::PrivateKey::random(&mut rand::rng(), ssh_key::Algorithm::Ed25519)
        .expect("host key");
    let fingerprint = host_key
        .public_key()
        .fingerprint(HashAlg::Sha256)
        .to_string();

    let mut config = russh::server::Config::default();
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
        };
        let running = server.run_on_socket(config, &listener);
        ready_tx.send(running.handle()).ok();
        let _ = running.await;
    });
    let shutdown = ready_rx.await.expect("server ready");

    TestServer {
        addr,
        fingerprint,
        observed,
        shutdown,
    }
}

struct TestSshServer {
    observed: Observed,
    accept_auth: bool,
}

impl russh::server::Server for TestSshServer {
    type Handler = TestSshHandler;

    fn new_client(&mut self, _peer_addr: Option<SocketAddr>) -> Self::Handler {
        self.observed.connections.fetch_add(1, Ordering::SeqCst);
        TestSshHandler {
            observed: self.observed.clone(),
            accept_auth: self.accept_auth,
        }
    }
}

struct TestSshHandler {
    observed: Observed,
    accept_auth: bool,
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
            Auth::Accept
        } else {
            Auth::reject()
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
        known_hosts_policy: policy,
        keepalive_interval_secs: 0,
    }
}

fn secrets() -> ConnectionSecrets {
    ConnectionSecrets {
        password: Some("hunter2".into()),
        private_key_pem: None,
        private_key_passphrase: None,
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
