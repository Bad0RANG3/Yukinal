//! 真机集成测试：对一台 Linux 主机跑完整 MVP 采集链，断言数据与真实状态一致。
//!
//! 复用 SSH 门控变量：`YUKINAL_SSH_TEST_HOST/USER/PASSWORD`（或 `KEY_PATH`）。
//! 未设置时跳过，设置为失败让 CI 真红。

use std::sync::Arc;
use std::time::Duration;

use yukinal_collector::{CollectedData, CollectorContext, CollectorEngine};
use yukinal_ssh::{
    Authentication, ConnectionSecrets, KnownHostsPolicy, RusshBackend, SshBackend, SshConfig,
};

fn env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn is_enabled() -> bool {
    env("YUKINAL_SSH_TEST_HOST").is_some()
        && env("YUKINAL_SSH_TEST_USER").is_some()
        && env("YUKINAL_SSH_TEST_PASSWORD").is_some()
}

async fn connect() -> Option<(Arc<RusshBackend>, yukinal_ssh::Session)> {
    if !is_enabled() {
        eprintln!(
            "skipped: set YUKINAL_SSH_TEST_HOST/USER/PASSWORD to run the live collector test"
        );
        return None;
    }
    let backend = Arc::new(RusshBackend::from_data_dir(&std::env::temp_dir()).expect("backend"));
    let config = SshConfig {
        server_id: "srv_live".into(),
        host: env("YUKINAL_SSH_TEST_HOST").expect("host"),
        port: env("YUKINAL_SSH_TEST_PORT")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(22),
        username: env("YUKINAL_SSH_TEST_USER").expect("user"),
        authentication: Authentication::Password {
            credential_ref: "keychain://ssh-test/plain".into(),
        },
        known_hosts_policy: KnownHostsPolicy::TrustOnFirstUse,
        keepalive_interval_secs: 0,
    };
    let secrets = ConnectionSecrets {
        password: env("YUKINAL_SSH_TEST_PASSWORD"),
        private_key_pem: None,
        private_key_passphrase: None,
    };
    let session = backend.connect(config, secrets).await.expect("connect");
    Some((backend, session))
}

#[tokio::test]
async fn full_mvp_chain_on_a_real_linux_host() {
    let Some((backend, session)) = connect().await else {
        return;
    };

    let engine = CollectorEngine::with_mvp();
    let context = CollectorContext::new(
        "srv_live",
        yukinal_collector::runners::ssh(backend, session),
    );
    engine.detect_all(&context).await.expect("detect");
    {
        let caps = context.capabilities.lock().expect("lock");
        assert!(
            caps.iter().any(|(key, present)| key == "linux" && *present),
            "a Linux host must always report linux capability"
        );
    }

    let collected_at = format!(
        "{:?}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
    );
    let samples = engine.collect_all(&context, &collected_at).await;
    let failures: Vec<_> = samples
        .iter()
        .filter(|(sample, _)| !sample.ok)
        .map(|(s, _)| s.collector_id.clone())
        .collect();
    assert!(failures.is_empty(), "collectors failed: {failures:?}");

    for (_, data) in &samples {
        let Some(data) = data else { continue };
        match data {
            CollectedData::Os(os) => assert!(!os.distribution.is_empty(), "os.distribution"),
            CollectedData::Cpu(cpu) => {
                assert!(cpu.cores >= 1, "cpu cores");
                assert!(
                    cpu.usage_percent >= 0.0 && cpu.usage_percent <= 100.0,
                    "cpu usage"
                );
            }
            CollectedData::Memory(mem) => {
                assert!(mem.total_bytes > 0, "mem total");
            }
            CollectedData::Disks(disks) => {
                assert!(!disks.is_empty(), "df rows");
            }
            CollectedData::Uptime(seconds) => assert!(*seconds > 0, "uptime"),
            CollectedData::Network(interfaces) => {
                assert!(!interfaces.is_empty(), "net interfaces");
            }
            CollectedData::Docker(docker) => {
                // Docker 可能没装或没起——available=false 是合法结果。
                let _ = docker.available;
            }
        }
    }
}

#[tokio::test]
async fn command_timeout_is_surfaced_not_hung() {
    let Some((backend, session)) = connect().await else {
        return;
    };
    let context = CollectorContext::new(
        "srv_timeout",
        yukinal_collector::runners::ssh(backend, session),
    );
    // sleep 60 会撞上超时（2s）→ 返回 Timeout 错误，而不是挂死。
    let result = (context.runner)("sleep 60", Duration::from_secs(2)).await;
    assert!(matches!(
        result,
        Err(yukinal_collector::CollectorError::Timeout)
    ));
}
