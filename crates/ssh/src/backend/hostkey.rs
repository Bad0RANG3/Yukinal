//! host key：known_hosts 预检、握手、指纹表示与两个 `client::Handler`。
//!
//! 这里是「验证失败必须可见」的落点：不匹配在 [`ConnHandler::check_server_key`] 里
//! 当场变成一条带**两个**指纹的 [`super::error::HandshakeError`]，而不是一个 `false`。

use std::sync::{Arc, Mutex as StdMutex};

use russh::client::{self, Handle};
use russh::keys::{HashAlg, PublicKeyOrCertificate};

use super::auth::authenticate;
use super::error::{map_handshake_err, map_send_err, HandshakeError};
use crate::known_hosts::KnownHostsStore;
use crate::{ConnectionSecrets, Error, HostKeyProbe, Result, SshConfig};

/// 一次完整建连：预检 host key → TCP+握手 → 认证 → （TOFU）记录指纹。
pub(crate) async fn establish(
    config: &SshConfig,
    secrets: &ConnectionSecrets,
    known_hosts: &Arc<StdMutex<KnownHostsStore>>,
) -> Result<Arc<Handle<ConnHandler>>> {
    if config.port == 0 {
        return Err(Error::Configuration("port must be 1..=65535".into()));
    }

    let pinned = known_hosts
        .lock()
        .map_err(|_| Error::Transport("known_hosts lock poisoned".into()))?
        .pinned(&config.host, config.port);
    let (expected, accept_unknown) = match pinned {
        Some(pinned_fp) => (Some(pinned_fp), false),
        None => match config.known_hosts_policy {
            // 拒绝发生在 **TCP 之前**，这一点是有意的、也是被测试钉住的：
            // 「没有钉子」是本地就能回答的问题，为它去连一台我们本来就打算拒绝的主机，
            // 只会给中间人与服务器日志各送一次机会。换成先连再拒看起来只是位置不同，
            // 实际上把一个纯本地判断变成了一次网络活动。
            crate::KnownHostsPolicy::RequireMatch => {
                return Err(Error::HostKeyNotPinned {
                    host: config.host.clone(),
                    port: config.port,
                });
            }
            crate::KnownHostsPolicy::TrustOnFirstUse => (None, true),
        },
    };

    let presented = Arc::new(StdMutex::new(None::<PresentedKey>));
    let handler = ConnHandler {
        host: config.host.clone(),
        port: config.port,
        expected,
        accept_unknown,
        presented: Arc::clone(&presented),
    };
    let ssh_config = client::Config {
        inactivity_timeout: Some(std::time::Duration::from_secs(60)),
        ..<_>::default()
    };

    let mut handle = client::connect(
        Arc::new(ssh_config),
        (config.host.as_str(), config.port),
        handler,
    )
    .await
    .map_err(map_handshake_err)?;

    authenticate(&mut handle, config, secrets).await?;

    // TOFU：认证通过后再钉指纹，认证失败不留下记录。
    if accept_unknown {
        if let Some(PresentedKey::Fingerprint(fingerprint)) = presented
            .lock()
            .map_err(|_| Error::Transport("lock poisoned".into()))?
            .clone()
        {
            known_hosts
                .lock()
                .map_err(|_| Error::Transport("known_hosts lock poisoned".into()))?
                .register(&config.host, config.port, &fingerprint)
                .map_err(|error| Error::Transport(error.to_string()))?;
        }
    }

    Ok(Arc::new(handle))
}

/// 只做一次握手，返回服务器出示的指纹。见 [`crate::SshBackend::probe_host_key`] 的契约。
///
/// 它**不碰** `known_hosts`，一个锁都不借 —— 探针不持久化任何状态这件事因此是结构上
/// 成立，而不是靠「记得不要写」。它也不认证：`check_server_key` 一返回，握手就到此为止。
pub(super) async fn probe_server_key(host: &str, port: u16) -> Result<HostKeyProbe> {
    if port == 0 {
        return Err(Error::Configuration("port must be 1..=65535".into()));
    }

    let presented = Arc::new(StdMutex::new(None::<PresentedKey>));
    let handler = ProbeHandler {
        presented: Arc::clone(&presented),
    };
    let ssh_config = client::Config {
        inactivity_timeout: Some(std::time::Duration::from_secs(60)),
        ..<_>::default()
    };

    let handle = client::connect(Arc::new(ssh_config), (host, port), handler)
        .await
        .map_err(map_send_err)?;

    // 指纹先读出来，再关连接：`disconnect` 会消费掉这次握手。
    let seen = presented
        .lock()
        .map_err(|_| Error::Transport("probe lock poisoned".into()))?
        .clone();

    let _ = handle
        .disconnect(russh::Disconnect::ByApplication, "host key probe", "en")
        .await;

    match seen {
        Some(PresentedKey::Fingerprint(fingerprint)) => Ok(HostKeyProbe {
            host: host.to_string(),
            port,
            fingerprint,
        }),
        // 服务器把 host key 作为**证书**出示。本 crate 没有 host CA 信任库，所以既无法
        // 验证它、也无法把它钉住（见 `ConnHandler::check_server_key`）。这里给一个编造的
        // 指纹比给一个错误更糟：用户会拿着一个永远不可能被接受的字符串去核对。
        Some(PresentedKey::HostCertificate) => Err(Error::HostKeyUnsupported {
            host: host.to_string(),
            port,
            detail: "the server presented a host certificate, and this build has no host CA \
                     trust store to verify or pin certificates with"
                .into(),
        }),
        // 握手成功却什么都没看到，只可能意味着 russh 换了「什么时候问客户端」的时机。
        // 那时候探针必须响亮失败，而不是返回一个空指纹。
        None => Err(Error::Transport(
            "the handshake completed without presenting a host key".into(),
        )),
    }
}

/// 服务器在握手时出示的 host key，收敛成两种本 crate 能处理的情形。
///
/// 分开的理由：host 证书**没有**可钉的指纹（没有 host CA 信任库），把它编造成一个
/// `SHA256:…` 会让用户去核对一个永远不会被接受的字符串。
#[derive(Debug, Clone)]
pub(crate) enum PresentedKey {
    /// `SHA256:<base64 无填充>`，可以钉。
    Fingerprint(String),
    /// 服务器把 host key 作为证书出示。
    HostCertificate,
}

/// 从 russh 交过来的 host key 取出本 crate 的表示。
fn presented_key(server_public_key: &PublicKeyOrCertificate) -> PresentedKey {
    match server_public_key {
        PublicKeyOrCertificate::PublicKey { key, .. } => {
            PresentedKey::Fingerprint(key.fingerprint(HashAlg::Sha256).to_string())
        }
        PublicKeyOrCertificate::Certificate(_) => PresentedKey::HostCertificate,
    }
}

/// 认证期 host key 校验：核对 against 已钉指纹；TOFU 下放行（记录在 establish）。
pub(crate) struct ConnHandler {
    /// 出错时要点名是哪台主机 —— 一条「指纹不一致」的错误如果不说是谁，用户
    /// 面对多台服务器时没法处置。
    host: String,
    port: u16,
    expected: Option<String>,
    accept_unknown: bool,
    presented: Arc<StdMutex<Option<PresentedKey>>>,
}

impl client::Handler for ConnHandler {
    type Error = HandshakeError;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        let presented = match presented_key(server_public_key) {
            PresentedKey::Fingerprint(fingerprint) => fingerprint,
            // 这是**服务器**把 host key 作为证书出示（host certificate），和用户
            // 证书认证（`authenticate` 里的 `Authentication::Certificate`）是两件
            // 不同的事：后者已经支持。
            //
            // 这里仍然拒绝，因为本 crate 没有 host CA 信任库。要接受一张 host
            // 证书，得先知道「哪把 CA key 被信任、签名是否出自它、主机名是否在
            // principals 里」；而我们能做的只有「记住它」—— 那正是这里禁止的
            // 「先信再查」，known_hosts 的钉子会因此变成一句空话。
            //
            // 但拒绝要**说得出来由**：返回 `Ok(false)` 只会变成 russh 的
            // `UnknownKey`，界面上一句「握手失败」既不解释也不可操作。
            PresentedKey::HostCertificate => {
                return Err(HandshakeError::Unsupported {
                    host: self.host.clone(),
                    port: self.port,
                    detail: "the server presented a host certificate, and this build has no \
                             host CA trust store to verify or pin certificates with"
                        .into(),
                });
            }
        };
        if let Ok(mut slot) = self.presented.lock() {
            *slot = Some(PresentedKey::Fingerprint(presented.clone()));
        }

        match &self.expected {
            Some(pinned) if *pinned == presented => Ok(true),
            // 不匹配在这里就变成一条带**两个**指纹的错误，而不是一个 `false`。
            Some(pinned) => Err(HandshakeError::Mismatch {
                host: self.host.clone(),
                pinned: pinned.clone(),
                presented,
            }),
            None => Ok(self.accept_unknown),
        }
    }
}

/// 探针的 handler：接受任何出示的 key，**只**把看到的记下来。
///
/// 「接受」在这里不是一次信任判断，而恰恰是探针的定义：它不判断可信与否，它只回答
/// 「服务器出示了什么」。判断留给用户 —— 探针结果在用户确认并钉住之前不可信
/// （ADR 0012 第 4 条）。这个 handler 也没有 `accept_unknown` 那种开关，
/// 因为它根本不读 known_hosts（`probe_server_key` 里连 store 都不碰）。
struct ProbeHandler {
    presented: Arc<StdMutex<Option<PresentedKey>>>,
}

impl client::Handler for ProbeHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        if let Ok(mut slot) = self.presented.lock() {
            *slot = Some(presented_key(server_public_key));
        }
        // 永远是 `true`：探针问的是「你出示什么」，不是「我可不可以信你」。
        Ok(true)
    }
}

impl KnownHostsStore {
    /// 只看是否已钉过、钉子是什么（不比较 presented）。
    ///
    /// 这里直接查表，不再借道 `check(host, port, "")` —— 那是以「和空串比较」的形式
    /// 表达一次查找，读起来像在做校验，实际只是取值。真正的比对在
    /// `ConnHandler::check_server_key`（指纹只在该回调里才存在）；`check` 在生产路径上
    /// 的另一个使用者是探针（`RusshBackend::host_key_check`），它比的是**真的**出示过的
    /// 指纹，不是空串。
    fn pinned(&self, host: &str, port: u16) -> Option<String> {
        self.pinned_fingerprint(host, port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::test_support::{generated_key, test_certificate};
    use crate::backend::RusshBackend;
    use crate::{Authentication, KnownHostsPolicy, SshBackend};

    #[test]
    fn pinned_reports_unknown_for_new_hosts() {
        let store = KnownHostsStore::in_memory();
        assert_eq!(store.pinned("example.com", 22), None);
    }

    /// `RequireMatch` 下未钉过的主机：拒绝，而且是**在 TCP 之前**拒绝。
    ///
    /// 这条测试同时钉住两件事：
    ///
    /// 1. 不触网。地址 `10.255.255.1:2222` 是不可路由的，所以「没有触网」不是靠计时
    ///    猜的 —— 如果这条路径真的去连了，结果会是 `Error::Timeout`（15s）或
    ///    `Error::Transport`，而测试期待的是那条**本地**判断的错误。这也正是这条预检
    ///    存在的理由：要不要连一台我们本来就打算拒绝的主机，本地就能回答。
    /// 2. 错误种类。这个情形报 [`Error::HostKeyNotPinned`]，**不是**
    ///    [`Error::HostKeyVerification`]：后者意味着「钉住的与出示的不一致」，
    ///    而这里连出示都还没发生（ADR 0012 第 3 条）。
    #[tokio::test]
    async fn connect_refuses_untrusted_host_under_require_match() {
        let backend = RusshBackend::new(Arc::new(StdMutex::new(KnownHostsStore::in_memory())));
        let config = SshConfig {
            server_id: "srv_t".into(),
            host: "10.255.255.1".into(),
            port: 2222,
            username: "root".into(),
            authentication: Authentication::Password {
                credential_ref: "keychain://ssh/t".into(),
            },
            known_hosts_policy: KnownHostsPolicy::RequireMatch,
            keepalive_interval_secs: 0,
        };
        let result = backend.connect(config, ConnectionSecrets::empty()).await;
        assert!(
            matches!(
                result,
                Err(Error::HostKeyNotPinned { ref host, port: 2222 }) if host == "10.255.255.1"
            ),
            "未钉住必须是「没钉子」，不是「指纹不一致」：{result:?}",
        );
    }

    /// host key 有两种出示形状，本 crate 只认其中一种。
    ///
    /// 公钥 → 可钉的指纹；host 证书 → `HostCertificate`（没有可钉的指纹，因为它需要
    /// host CA 信任库，而本 crate 没有）。这条区分是纯函数，所以不需要服务器就能测。
    #[test]
    fn a_presented_key_is_either_a_pinnable_fingerprint_or_a_certificate() {
        let key = generated_key();
        let public = key.public_key().clone();
        let expected = public.fingerprint(HashAlg::Sha256).to_string();
        match presented_key(&PublicKeyOrCertificate::from(public.clone())) {
            PresentedKey::Fingerprint(fingerprint) => assert_eq!(fingerprint, expected),
            other => panic!("a public key must yield a fingerprint, got {other:?}"),
        }

        let ca = generated_key();
        let certificate = test_certificate(&ca, &key);
        assert!(
            matches!(
                presented_key(&PublicKeyOrCertificate::from(certificate)),
                PresentedKey::HostCertificate
            ),
            "host 证书没有可钉的指纹，不能编一个出来",
        );
    }

    #[tokio::test]
    async fn connect_to_unreachable_host_maps_to_transport() {
        let backend = RusshBackend::new(Arc::new(StdMutex::new(KnownHostsStore::in_memory())));
        let config = SshConfig {
            server_id: "srv_t".into(),
            host: "127.0.0.1".into(),
            port: 1, // nothing listens here
            username: "root".into(),
            authentication: Authentication::Password {
                credential_ref: "keychain://ssh/t".into(),
            },
            known_hosts_policy: KnownHostsPolicy::TrustOnFirstUse,
            keepalive_interval_secs: 0,
        };
        let result = backend.connect(config, ConnectionSecrets::empty()).await;
        assert!(matches!(result, Err(Error::Transport(_))));
    }

    /// 探针连不上时是**传输失败**，不是「握手没拿到指纹」也不是 panic。
    ///
    /// 这条与上面那条成对：探针是一条新的握路径，它必须和建连一样把「连不上」归类到
    /// 同一个地方 —— 否则界面会把「主机不可达」显示成「这台服务器的指纹有问题」，而
    /// 用户会去检查一个根本没参与这次失败的东西。
    #[tokio::test]
    async fn an_unreachable_probe_target_is_a_transport_failure() {
        let backend = RusshBackend::new(Arc::new(StdMutex::new(KnownHostsStore::in_memory())));
        let result = backend.probe_host_key("127.0.0.1", 1).await; // nothing listens here
        assert!(
            matches!(result, Err(Error::Transport(_))),
            "探针连不上就是传输失败，got {result:?}",
        );
    }

    /// 端口 0 不是一个可以探的端口：这是**配置**错误，在发起连接之前就拒绝。
    ///
    /// 值得一条测试，因为它是「这个值根本没被填过」与「这个值填错了」的分界：如果
    /// 端口 0 走进握手，用户拿到的会是一句关于网络的话，而真正要改的是服务器条目。
    #[tokio::test]
    async fn probing_port_zero_is_a_configuration_error() {
        let backend = RusshBackend::new(Arc::new(StdMutex::new(KnownHostsStore::in_memory())));
        let result = backend.probe_host_key("127.0.0.1", 0).await;
        assert!(
            matches!(result, Err(Error::Configuration(_))),
            "端口 0 是配置问题，got {result:?}",
        );
    }
}
