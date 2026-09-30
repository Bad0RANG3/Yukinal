//! 通过一台进程内回环 SSH 服务器验证真实命令通道。
//!
//! 这些测试跑完整的 SSH key exchange、认证、exec channel 和消息收发，不依赖真实
//! 服务器或环境变量。它们专门锁住运维命令最重要的边界：stdout/stderr 与非零退出码
//! 不丢失、输出受上限约束、超时和用户取消会结束等待。

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::keys::{ssh_key, HashAlg};
use russh::server::{Auth, RunningServerHandle, Server as _};
use russh::{Channel, ChannelId};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use yukinal_ssh::{
    Authentication, ConnectionSecrets, Error, KnownHostsPolicy, OutboundProxy, RusshBackend,
    SshBackend, SshConfig,
};

const EXPECTED_AUTH_USER: &str = "yukinal-test";
const EXPECTED_AUTH_PASSWORD: &str = "not-a-real-secret";
const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;

#[derive(Clone, Default)]
struct Observed {
    commands: Arc<Mutex<Vec<String>>>,
}

struct TestServer {
    address: SocketAddr,
    fingerprint: String,
    observed: Observed,
    shutdown: RunningServerHandle,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown
            .shutdown("SSH command test complete".to_string());
    }
}

#[derive(Clone)]
struct CommandServer {
    observed: Observed,
}

impl russh::server::Server for CommandServer {
    type Handler = CommandHandler;

    fn new_client(&mut self, _peer_addr: Option<SocketAddr>) -> Self::Handler {
        CommandHandler {
            observed: self.observed.clone(),
        }
    }
}

struct CommandHandler {
    observed: Observed,
}

impl russh::server::Handler for CommandHandler {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(
            if user == EXPECTED_AUTH_USER && password == EXPECTED_AUTH_PASSWORD {
                Auth::Accept
            } else {
                Auth::reject()
            },
        )
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<russh::server::Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        let command = String::from_utf8_lossy(data).into_owned();
        self.observed
            .commands
            .lock()
            .expect("command observations")
            .push(command.clone());

        // `channel.exec()` waits for this before the client begins reading output.
        session.channel_success(channel)?;
        let handle = session.handle();

        // Leaving these two requests unanswered gives the client-side timeout and
        // cancellation paths a real, authenticated SSH channel to interrupt.
        if matches!(command.as_str(), "wait-forever") {
            return Ok(());
        }

        tokio::spawn(async move {
            let (stdout, stderr, exit_status) = match command.as_str() {
                "result" => (
                    b"load average: 0.10\n".to_vec(),
                    b"fixture warning\n".to_vec(),
                    23,
                ),
                "large-output" => {
                    // Stay below russh's per-packet limit while exceeding the
                    // client's aggregate output limit by one complete chunk.
                    let chunk = vec![b'x'; 16 * 1024];
                    for _ in 0..(OUTPUT_LIMIT / chunk.len() + 1) {
                        if handle.data(channel, chunk.clone()).await.is_err() {
                            return;
                        }
                    }
                    (Vec::new(), Vec::new(), 0)
                }
                other => (
                    format!("unexpected command: {other}\n").into_bytes(),
                    Vec::new(),
                    127,
                ),
            };

            if !stdout.is_empty() && handle.data(channel, stdout).await.is_err() {
                return;
            }
            if !stderr.is_empty() && handle.extended_data(channel, 1, stderr).await.is_err() {
                return;
            }
            let _ = handle.exit_status_request(channel, exit_status).await;
            let _ = handle.eof(channel).await;
            let _ = handle.close(channel).await;
        });

        Ok(())
    }
}

async fn start_server() -> TestServer {
    let host_key = ssh_key::PrivateKey::random(&mut rand::rng(), ssh_key::Algorithm::Ed25519)
        .expect("generate test host key");
    let fingerprint = host_key
        .public_key()
        .fingerprint(HashAlg::Sha256)
        .to_string();
    let config = Arc::new(russh::server::Config {
        keys: vec![host_key],
        auth_rejection_time: Duration::from_millis(10),
        inactivity_timeout: None,
        ..Default::default()
    });

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test SSH server");
    let address = listener.local_addr().expect("test SSH address");
    let observed = Observed::default();
    let server_observed = observed.clone();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        let mut server = CommandServer {
            observed: server_observed,
        };
        let running = server.run_on_socket(config, &listener);
        ready_tx.send(running.handle()).ok();
        let _ = running.await;
    });

    let shutdown = ready_rx.await.expect("test SSH server started");
    TestServer {
        address,
        fingerprint,
        observed,
        shutdown,
    }
}

async fn connect(server: &TestServer) -> (tempfile::TempDir, RusshBackend, yukinal_ssh::Session) {
    let data_dir = tempfile::tempdir().expect("temporary SSH data directory");
    let backend = RusshBackend::from_data_dir(data_dir.path()).expect("SSH backend");
    backend
        .trust_host("127.0.0.1", server.address.port(), &server.fingerprint)
        .expect("pin the generated test server key");

    let session = backend
        .connect(
            SshConfig {
                server_id: "srv_command_test".into(),
                host: "127.0.0.1".into(),
                port: server.address.port(),
                username: EXPECTED_AUTH_USER.into(),
                authentication: Authentication::Password {
                    credential_ref: "keychain://ssh/command-test".into(),
                },
                host_certificate_authority: None,
                known_hosts_policy: KnownHostsPolicy::RequireMatch,
                outbound_proxy: OutboundProxy::default(),
                keepalive_interval_secs: 0,
            },
            ConnectionSecrets {
                password: Some(EXPECTED_AUTH_PASSWORD.into()),
                ..ConnectionSecrets::empty()
            },
        )
        .await
        .expect("authenticate to pinned loopback server");

    (data_dir, backend, session)
}

#[tokio::test]
async fn command_channel_preserves_streams_and_nonzero_exit_status_and_bounds_output() {
    let server = start_server().await;
    let (_data_dir, backend, session) = connect(&server).await;

    let result = backend
        .execute(
            &session,
            "result",
            Some(Duration::from_secs(5)),
            &CancellationToken::new(),
        )
        .await
        .expect("read command result");
    assert_eq!(
        result.exit_code, 23,
        "non-zero remote status is evidence, not a transport error"
    );
    assert_eq!(result.stdout_lossy(), "load average: 0.10\n");
    assert_eq!(result.stderr_lossy(), "fixture warning\n");

    let large = backend
        .execute(
            &session,
            "large-output",
            Some(Duration::from_secs(10)),
            &CancellationToken::new(),
        )
        .await
        .expect("drain output after reaching the cap");
    assert_eq!(large.exit_code, 0);
    assert_eq!(large.stdout.len(), OUTPUT_LIMIT);
    assert!(large.stderr.is_empty());

    backend.close(&session).await.expect("close SSH session");
    assert_eq!(
        server
            .observed
            .commands
            .lock()
            .expect("command observations")
            .as_slice(),
        ["result", "large-output"],
    );
}

#[tokio::test]
async fn command_timeout_and_cancellation_interrupt_a_real_open_ssh_channel() {
    let server = start_server().await;
    let (_data_dir, backend, session) = connect(&server).await;

    let timeout = backend
        .execute(
            &session,
            "wait-forever",
            Some(Duration::from_millis(50)),
            &CancellationToken::new(),
        )
        .await
        .expect_err("the unanswered command must time out");
    assert!(matches!(timeout, Error::Timeout), "got {timeout:?}");

    let cancel = CancellationToken::new();
    let execution = backend.execute(&session, "wait-forever", None, &cancel);
    tokio::pin!(execution);
    tokio::select! {
        result = &mut execution => panic!("unanswered command unexpectedly finished: {result:?}"),
        _ = tokio::time::sleep(Duration::from_millis(50)) => cancel.cancel(),
    }
    let cancelled = execution.await.expect_err("cancelled command must return");
    assert!(matches!(cancelled, Error::Cancelled), "got {cancelled:?}");

    backend.close(&session).await.expect("close SSH session");
    assert_eq!(
        server
            .observed
            .commands
            .lock()
            .expect("command observations")
            .as_slice(),
        ["wait-forever", "wait-forever"],
        "timeouts and cancellation must not cause the client to replay a command",
    );
}
