//! 真机集成测试（SSH）。CI 默认跳过：设置以下变量才跑，且连接失败会让测试失败：
//!
//! - `YUKINAL_SSH_TEST_HOST` / `YUKINAL_SSH_TEST_PORT`（默认 22）
//! - `YUKINAL_SSH_TEST_USER`
//! - `YUKINAL_SSH_TEST_PASSWORD`（或 `YUKINAL_SSH_TEST_KEY_PATH`）
//! - `YUKINAL_SSH_TEST_KEY_PASSPHRASE`（加密私钥；只给 `YUKINAL_SSH_TEST_KEY_PATH` 时
//!   视为该 key 不带口令）
//! - `YUKINAL_SSH_TEST_KEY_CERT`（该私钥的 `*-cert.pub` 用户证书）
//! - `YUKINAL_SSH_TEST_AGENT`（任意非空值 = 允许跑 agent 认证，用**本机正在跑的**
//!   agent；不设这个变量就跳过，因为 agent 里有什么 key 是环境的属性，不是仓库的属性）
//!
//! DoD 覆盖：密码 + 私钥（含加密私钥）+ 证书 + ssh-agent 登录真机、往返命令、
//! keepalive、host key 变化阻断。
//!
//! ## 关于与 `crates/collector/tests/live.rs` 的重复
//!
//! 那个文件里有一份逐字相同的 `env()` 和一份形状相近的 `SshConfig` 字面量。审计提过
//! 把它们抽成共享的测试支持模块，**这里有意不做**，理由如下：
//!
//! 集成测试是独立 crate，看不到 `yukinal-ssh` 里的 `#[cfg(test)]` 模块（依赖被编译时
//! 不设 `cfg(test)`）。要跨 crate 共享，只能给 `yukinal-ssh` 加一个 cargo feature 并
//! 把模块挂成 `pub`，再由 `yukinal-collector` 的 dev-dependency 打开它。为两个被环境
//! 变量门控的测试文件，在一个处理密钥与主机指纹的库的公开面上开一个 feature 口子，
//! 代价大于收益。
//!
//! 而且两处**并不是**同一份配置：本文件的 `is_enabled()` 允许私钥登录，collector 的
//! 要求必须有密码（它的采集链走密码认证）；`known_hosts_policy` 与 `server_id` 也
//! 各不相同。真正逐字相同的只有那 5 行 env 读取，为此引入上述机制不划算。
//!
//! 于是选择记录判断而非共享代码 —— 如果将来这第三份出现（`commands/terminal.rs`
//! 已有一份生产用的字面量），或者有人真的开始复制 `is_enabled()`,那说明调用点在变多，
//! 届时再加 feature 才是对的时机。

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
        private_key_passphrase: env("YUKINAL_SSH_TEST_KEY_PASSPHRASE"),
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

/// 加密私钥：口令经 `ConnectionSecrets` 传下来，key 在认证那一刻才解密。
#[tokio::test]
async fn passphrase_protected_key_auth_connects() {
    if !is_enabled() || env("YUKINAL_SSH_TEST_KEY_PATH").is_none() {
        eprintln!("skipped: encrypted key test needs YUKINAL_SSH_TEST_KEY_PATH");
        return;
    }
    if env("YUKINAL_SSH_TEST_KEY_PASSPHRASE").is_none() {
        eprintln!("skipped: encrypted key test needs YUKINAL_SSH_TEST_KEY_PASSPHRASE");
        return;
    }
    let backend = RusshBackend::from_data_dir(&std::env::temp_dir()).expect("backend");
    let mut config = test_config("srv_key_enc", KnownHostsPolicy::TrustOnFirstUse);
    config.authentication = Authentication::PrivateKey {
        credential_ref: "keychain://ssh-test/key".into(),
        passphrase_ref: Some("keychain://ssh-test/key-passphrase".into()),
    };
    let session = backend.connect(config, secrets()).await.expect("connect");
    backend.close(&session).await.expect("close");
}

/// 用户证书：私钥与 `<key>-cert.pub` 放同一目录，认证时按 OpenSSH 的 sibling 约定找证书。
///
/// 之所以先复制到临时目录：只有「私钥路径 + 约定」这一种输入，才能真的走一遍 sibling
/// 推导；直接指定 `certificate_path` 会把那条路径绕过去。
#[tokio::test]
async fn certificate_auth_connects() {
    if !is_enabled() || env("YUKINAL_SSH_TEST_KEY_PATH").is_none() {
        eprintln!("skipped: certificate test needs YUKINAL_SSH_TEST_KEY_PATH");
        return;
    }
    let Some(certificate) = env("YUKINAL_SSH_TEST_KEY_CERT") else {
        eprintln!("skipped: certificate test needs YUKINAL_SSH_TEST_KEY_CERT");
        return;
    };
    let dir = std::env::temp_dir().join(format!("yukinal-ssh-cert-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let key_path = dir.join("id_live");
    std::fs::copy(
        env("YUKINAL_SSH_TEST_KEY_PATH").expect("key path"),
        &key_path,
    )
    .expect("copy key");
    std::fs::copy(certificate, dir.join("id_live-cert.pub")).expect("copy certificate");

    let backend = RusshBackend::from_data_dir(&std::env::temp_dir()).expect("backend");
    let mut config = test_config("srv_cert", KnownHostsPolicy::TrustOnFirstUse);
    config.authentication = Authentication::Certificate {
        credential_ref: "keychain://ssh-test/key".into(),
        passphrase_ref: env("YUKINAL_SSH_TEST_KEY_PASSPHRASE")
            .map(|_| "keychain://ssh-test/key-passphrase".to_string()),
        private_key_path: Some(key_path.display().to_string()),
        certificate_path: None,
    };
    let session = backend.connect(config, secrets()).await.expect("connect");
    backend.close(&session).await.expect("close");
    cleanup_and_remove(&dir);
}

/// ssh-agent：用本机正在跑的那个 agent。要显式打开才跑 —— agent 里有哪些身份是环境的
/// 属性，让 CI 去断言它没有意义。
#[tokio::test]
async fn agent_auth_connects() {
    if !is_enabled() || env("YUKINAL_SSH_TEST_AGENT").is_none() {
        eprintln!("skipped: agent test needs YUKINAL_SSH_TEST_AGENT (any non-empty value)");
        return;
    }
    let backend = RusshBackend::from_data_dir(&std::env::temp_dir()).expect("backend");
    let mut config = test_config("srv_agent", KnownHostsPolicy::TrustOnFirstUse);
    config.authentication = Authentication::Agent { socket_path: None };
    let session = backend.connect(config, secrets()).await.expect("connect");
    let result = backend
        .execute(
            &session,
            "echo agent-auth",
            Some(std::time::Duration::from_secs(10)),
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("execute");
    assert_eq!(result.stdout_lossy().trim(), "agent-auth");
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
///
/// 这条真机测试同时验证 ADR 0012 第 3 条的**呈现**：错误里必须同时有被篡改的钉子与
/// 服务器真实的出示指纹。在真机上这才有意义 —— 只有真的握手过，`presented` 才是
/// 「服务器这次出示的」，而不是测试自己编的字符串。
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

    // 真指纹：探针拿到的那个（也不写任何东西）。
    let real = backend
        .probe_host_key(
            &env("YUKINAL_SSH_TEST_HOST").expect("host"),
            env("YUKINAL_SSH_TEST_PORT")
                .and_then(|raw| raw.parse().ok())
                .unwrap_or(22),
        )
        .await
        .expect("probe");

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
    match result {
        Err(Error::HostKeyVerification {
            pinned, presented, ..
        }) => {
            assert_ne!(pinned, presented);
            assert_eq!(
                presented, real.fingerprint,
                "出示的那个必须是服务器真的出示过的指纹，否则用户核对的是个假字符串",
            );
        }
        other => panic!("tampered host key must be blocked, got {other:?}"),
    }
    cleanup_and_remove(&dir);
}

/// 探针：真连一次拿指纹，**什么都不写**；用户确认后再钉住，连接才被放行。
///
/// 这条覆盖的是新入口的完整闭环（探针 → 钉住 → 连接），而它只有在真机上才有意义：
/// 本地没有服务器时，唯一能验证的是「未钉住被拒」和「不匹配被拒」。
#[tokio::test]
async fn probe_then_trust_unblocks_a_require_match_connection() {
    if !is_enabled() {
        eprintln!("skipped: requires real host");
        return;
    }
    let dir = std::env::temp_dir().join(format!("yukinal-ssh-probe-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let store_path = dir.join("known_hosts");
    let _ = std::fs::remove_file(&store_path);

    let backend = RusshBackend::from_data_dir(&dir).expect("backend");
    let host = env("YUKINAL_SSH_TEST_HOST").expect("host");
    let port = env("YUKINAL_SSH_TEST_PORT")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(22);

    // 探针不写任何东西 —— 包括不许顺手写下 pin。
    let probed = backend.probe_host_key(&host, port).await.expect("probe");
    assert!(!store_path.exists(), "探针不得落盘");
    assert_eq!(backend.host_key_pin(&host, port).expect("pin"), None);

    // 未钉住 + RequireMatch：拒绝，而且是本地拒绝。
    let refused = backend
        .connect(
            test_config("srv_probe", KnownHostsPolicy::RequireMatch),
            secrets(),
        )
        .await;
    assert!(
        matches!(refused, Err(Error::HostKeyNotPinned { .. })),
        "没核验过就该被挡在门外，got {refused:?}",
    );

    // 用户确认这个指纹 → 钉住 → 同样的连接放行。
    let decision = backend
        .trust_host(&host, port, &probed.fingerprint)
        .expect("trust");
    assert!(matches!(
        decision,
        yukinal_ssh::known_hosts::TrustDecision::Pin { .. }
    ));
    let session = backend
        .connect(
            test_config("srv_probe", KnownHostsPolicy::RequireMatch),
            secrets(),
        )
        .await
        .expect("pinned host must connect");
    backend.close(&session).await.expect("close");

    // 同一个指纹再确认一次不是错误，但也不是一次写入。
    assert!(matches!(
        backend
            .trust_host(&host, port, &probed.fingerprint)
            .expect("trust"),
        yukinal_ssh::known_hosts::TrustDecision::AlreadyPinned { .. }
    ));

    // 换一个指纹：拒绝，且钉子不变。
    assert!(matches!(
        backend
            .trust_host(
                &host,
                port,
                "SHA256:deadbeefdeadbeefdeadbeefdeadbeefdeadbeefde"
            )
            .expect("trust"),
        yukinal_ssh::known_hosts::TrustDecision::RefusedDifferentPin { .. }
    ));
    assert_eq!(
        backend.host_key_pin(&host, port).expect("pin"),
        Some(probed.fingerprint.clone())
    );

    // 遗忘 → 回到 TOFU 的起点（内存与文件都回到起点）。
    assert!(matches!(
        backend.forget_host(&host, port).expect("forget"),
        yukinal_ssh::known_hosts::ForgetOutcome::Removed { .. }
    ));
    assert_eq!(backend.host_key_pin(&host, port).expect("pin"), None);
    let reloaded = RusshBackend::from_data_dir(&dir).expect("reload");
    assert_eq!(reloaded.host_key_pin(&host, port).expect("pin"), None);

    cleanup_and_remove(&dir);
}

fn cleanup_and_remove(dir: &std::path::Path) {
    std::fs::remove_dir_all(dir).ok();
}
