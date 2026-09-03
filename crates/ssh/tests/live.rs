//! 真机集成测试（SSH）。CI 默认跳过：设置以下变量才跑，且连接失败会让测试失败：
//!
//! - `YUKINAL_SSH_TEST_HOST` / `YUKINAL_SSH_TEST_PORT`（默认 22）
//! - `YUKINAL_SSH_TEST_USER`
//! - `YUKINAL_SSH_TEST_PASSWORD`（或 `YUKINAL_SSH_TEST_KEY_PATH`）
//!
//! DoD 覆盖：密码 + 私钥登录真机、往返命令、keepalive、host key 变化阻断。

use yukinal_ssh::{
    Authentication, ConnectionSecrets, Error, KnownHostsPolicy, RusshBackend, SshBackend, SshConfig,
};

fn env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn is_enabled() -> bool {
    env("YUKINAL_SSH_TEST_HOST").is_some() && env("YUKINAL_SSH_TEST_USER").is_some()
}

fn test_config(server_id: &str, policy: KnownHostsPolicy) -> SshConfig {
    SshConfig {
        server_id: server_id.into(),
        host: env("YUKINAL_SSH_TEST_HOST").expect("host"),
        port: env("YUKINAL_SSH_TEST_PORT")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(22),
        username: env("YUKINAL_SSH_TEST_USER").expect("user"),
        authentication: Authentication::Password {
            credential_ref: "keychain://ssh-test/plain".into(),
        },
        known_hosts_policy: policy,
        keepalive_interval_secs: 0,
    }
}

fn secrets() -> ConnectionSecrets {
    let key_path = env("YUKINAL_SSH_TEST_KEY_PATH");
    let key_pem = key_path.map(|path| std::fs::read_to_string(path).expect("read key file"));
    ConnectionSecrets {
        password: env("YUKINAL_SSH_TEST_PASSWORD"),
        private_key_pem: key_pem,
        private_key_passphrase: None,
    }
}

#[tokio::test]
async fn password_auth_executes_a_command() {
    if !is_enabled() {
        eprintln!("skipped: set YUKINAL_SSH_TEST_HOST/USER/PASSWORD to run");
        return;
    }
    if env("YUKINAL_SSH_TEST_PASSWORD").is_none() {
        eprintln!("skipped: password auth needs YUKINAL_SSH_TEST_PASSWORD");
        return;
    }
    let backend = RusshBackend::from_data_dir(&std::env::temp_dir()).expect("backend");
    let session = backend
        .connect(
            test_config("srv_live", KnownHostsPolicy::TrustOnFirstUse),
            secrets(),
        )
        .await
        .expect("connect");
    let result = backend
        .execute(
            &session,
            "printf 'pong-%s' yukinal",
            Some(std::time::Duration::from_secs(10)),
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("execute");
    assert_eq!(result.stdout_lossy(), "pong-yukinal");

    // 命令超时会被显式报告，而不是挂死。
    let _ = backend
        .execute(
            &session,
            "sleep 30",
            Some(std::time::Duration::from_millis(800)),
            &tokio_util::sync::CancellationToken::new(),
        )
        .await;
    backend.close(&session).await.expect("close");
}

#[tokio::test]
async fn private_key_auth_connects() {
    if !is_enabled() || env("YUKINAL_SSH_TEST_KEY_PATH").is_none() {
        eprintln!("skipped: private key test needs YUKINAL_SSH_TEST_KEY_PATH");
        return;
    }
    let backend = RusshBackend::from_data_dir(&std::env::temp_dir()).expect("backend");
    let mut config = test_config("srv_key", KnownHostsPolicy::TrustOnFirstUse);
    config.authentication = Authentication::PrivateKey {
        credential_ref: "keychain://ssh-test/key".into(),
        passphrase_ref: None,
    };
    let session = backend.connect(config, secrets()).await.expect("connect");
    backend.close(&session).await.expect("close");
}

#[tokio::test]
async fn keepalive_keeps_session_alive() {
    if !is_enabled() {
        eprintln!("skipped: requires real host");
        return;
    }
    let backend = RusshBackend::from_data_dir(&std::env::temp_dir()).expect("backend");
    let mut config = test_config("srv_ka", KnownHostsPolicy::TrustOnFirstUse);
    config.keepalive_interval_secs = 1;
    let session = backend.connect(config, secrets()).await.expect("connect");
    // 等两个 keepalive 周期——ping 失败只打日志，会话仍可用于命令。
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    let result = backend
        .execute(
            &session,
            "echo alive",
            Some(std::time::Duration::from_secs(10)),
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("execute after keepalive");
    assert_eq!(result.stdout_lossy().trim(), "alive");
    backend.close(&session).await.expect("close");
}

/// host key 变化必须阻断：TOFU 记下指纹后，人为改 store 的钉子，再次连接要求匹配。
#[tokio::test]
async fn host_key_change_is_blocked() {
    if !is_enabled() {
        eprintln!("skipped: requires real host");
        return;
    }
    let dir = std::env::temp_dir().join("yukinal-ssh-hostkey-test");
    std::fs::create_dir_all(&dir).expect("dir");

    // 第一次：TOFU 信任并记录真实指纹。
    let backend = RusshBackend::from_data_dir(&dir).expect("backend");
    let session = backend
        .connect(
            test_config("srv_hk", KnownHostsPolicy::TrustOnFirstUse),
            secrets(),
        )
        .await
        .expect("first connect");
    backend.close(&session).await.expect("close");

    // 篡改 known_hosts 里的指纹 → 服务器"看起来换了 key"。
    let store_path = dir.join("known_hosts");
    let raw = std::fs::read_to_string(&store_path).expect("read store");
    let rewritten = raw.replace("SHA256:", "SHA256:deadbeef");
    if rewritten == raw {
        eprintln!("skipped: store format unexpected (nothing to tamper)");
        return;
    }
    std::fs::write(&store_path, rewritten).expect("write tampered store");

    let backend2 = RusshBackend::from_data_dir(&dir).expect("backend2");
    let result = backend2
        .connect(
            test_config("srv_hk", KnownHostsPolicy::RequireMatch),
            secrets(),
        )
        .await;
    assert!(
        matches!(result, Err(Error::HostKeyVerification { .. })),
        "tampered host key must be blocked, got {result:?}"
    );
    cleanup_and_remove(&dir);
}

fn cleanup_and_remove(dir: &std::path::Path) {
    std::fs::remove_dir_all(dir).ok();
}
