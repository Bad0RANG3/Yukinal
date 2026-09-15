//! host key：known_hosts 预检、握手、指纹表示与两个 `client::Handler`。
//!
//! 这里是「验证失败必须可见」的落点：不匹配在 [`ConnHandler::check_server_key`] 里
//! 当场变成一条带**两个**指纹的 [`super::error::HandshakeError`]，而不是一个 `false`。

use std::borrow::Cow;
use std::io::Read as _;
use std::net::IpAddr;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use russh::client::{self, Handle};
use russh::keys::{ssh_key, Algorithm, HashAlg, PublicKeyOrCertificate};

use super::auth::authenticate;
use super::error::{map_handshake_err, map_send_err, HandshakeError};
use crate::known_hosts::KnownHostsStore;
use crate::krl::RevocationList;
use crate::{
    ConnectionSecrets, Error, HostCertificateAuthority, HostKeyProbe, OutboundProxy, Result,
    SshConfig,
};

/// 一次完整建连：预检 host key → TCP+握手 → 认证 → （TOFU）记录指纹。
pub(crate) async fn establish(
    config: &SshConfig,
    secrets: &ConnectionSecrets,
    known_hosts: &Arc<StdMutex<KnownHostsStore>>,
) -> Result<Arc<Handle<ConnHandler>>> {
    if config.port == 0 {
        return Err(Error::Configuration("port must be 1..=65535".into()));
    }
    let trusted_host_ca = parse_trusted_host_ca(
        config.host_certificate_authority.as_ref(),
        &config.outbound_proxy,
    )
    .await?;

    let pinned = known_hosts
        .lock()
        .map_err(|_| Error::Transport("known_hosts lock poisoned".into()))?
        .pinned(&config.host, config.port);
    let (expected, accept_unknown) = match pinned {
        Some(pinned_fp) => (Some(pinned_fp), false),
        None if trusted_host_ca.is_some() => (None, false),
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

    let require_certificate_host_key = trusted_host_ca.is_some();
    let presented = Arc::new(StdMutex::new(None::<PresentedKey>));
    let handler = ConnHandler {
        host: config.host.clone(),
        expected,
        accept_unknown,
        trusted_host_ca,
        presented: Arc::clone(&presented),
    };
    let mut ssh_config = client::Config {
        inactivity_timeout: Some(std::time::Duration::from_secs(60)),
        ..<_>::default()
    };
    if require_certificate_host_key {
        ssh_config.preferred.host_key_certificates = host_certificate_algorithms(&ssh_config);
    }

    let mut handle = tokio::time::timeout(
        super::CONNECT_TIMEOUT,
        client::connect(
            Arc::new(ssh_config),
            (config.host.as_str(), config.port),
            handler,
        ),
    )
    .await
    .map_err(|_| Error::Timeout)?
    .map_err(map_handshake_err)?;

    let auth_timeout = if secrets.keyboard_interactive.is_some() {
        super::INTERACTIVE_AUTH_TIMEOUT
    } else {
        super::AUTH_TIMEOUT
    };
    tokio::time::timeout(auth_timeout, authenticate(&mut handle, config, secrets))
        .await
        .map_err(|_| Error::Timeout)??;

    // TOFU：认证通过后再钉指纹，认证失败不留下记录。
    if accept_unknown {
        if let Some(presented) = presented
            .lock()
            .map_err(|_| Error::Transport("lock poisoned".into()))?
            .clone()
        {
            known_hosts
                .lock()
                .map_err(|_| Error::Transport("known_hosts lock poisoned".into()))?
                .register(&config.host, config.port, presented.fingerprint())
                .map_err(|error| Error::Transport(error.to_string()))?;
        }
    }

    Ok(Arc::new(handle))
}

fn host_certificate_algorithms(config: &client::Config) -> Cow<'static, [Algorithm]> {
    Cow::Owned(config.preferred.key.iter().cloned().collect())
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
        Some(
            PresentedKey::Fingerprint(fingerprint) | PresentedKey::Certificate { fingerprint, .. },
        ) => Ok(HostKeyProbe {
            host: host.to_string(),
            port,
            fingerprint,
        }),
        // 探针只报告服务器出示了什么，不套用 per-server CA 策略，也不写 known_hosts。
        // 这里给一个编造的指纹比给一个错误更糟：用户会拿着一个不可能被接受的字符串去核对。
        // 握手成功却什么都没看到，只可能意味着 russh 换了「什么时候问客户端」的时机。
        // 那时候探针必须响亮失败，而不是返回一个空指纹。
        None => Err(Error::Transport(
            "the handshake completed without presenting a host key".into(),
        )),
    }
}

/// 服务器在握手时出示的 host key，收敛成一个可显式核验、可钉住的公钥指纹。
///
#[derive(Debug, Clone)]
struct TrustedHostCa {
    key: ssh_key::PublicKey,
    principals: Vec<String>,
    revocations: Option<RevocationList>,
}

const MAX_KRL_BYTES: u64 = 16 * 1024 * 1024;

async fn parse_trusted_host_ca(
    authority: Option<&HostCertificateAuthority>,
    proxy: &OutboundProxy,
) -> Result<Option<TrustedHostCa>> {
    let Some(authority) = authority else {
        return Ok(None);
    };
    let key = ssh_key::PublicKey::from_openssh(authority.ca_public_key.trim())
        .map_err(|error| Error::Configuration(format!("invalid host CA public key: {error}")))?;
    if authority.principals.is_empty() {
        return Err(Error::Configuration(
            "host certificate authority requires at least one principal".into(),
        ));
    }
    if authority
        .principals
        .iter()
        .any(|principal| principal.trim().is_empty() || principal.chars().count() > 253)
    {
        return Err(Error::Configuration(
            "host certificate principals must be non-empty and at most 253 characters".into(),
        ));
    }
    if authority.revocation_list_signers.len() > 8 {
        return Err(Error::Configuration(
            "host certificate KRL may trust at most 8 independent signing keys".into(),
        ));
    }
    let mut revocation_signers = vec![key.clone()];
    for signer in &authority.revocation_list_signers {
        let signer = ssh_key::PublicKey::from_openssh(signer.trim()).map_err(|error| {
            Error::Configuration(format!("invalid trusted KRL signer public key: {error}"))
        })?;
        if revocation_signers
            .iter()
            .any(|existing| existing.key_data() == signer.key_data())
        {
            return Err(Error::Configuration(
                "trusted KRL signer public keys must be distinct from the host CA and each other"
                    .into(),
            ));
        }
        revocation_signers.push(signer);
    }
    let path = authority
        .revocation_list_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty());
    let url = authority
        .revocation_list_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty());
    if path.is_some() && url.is_some() {
        return Err(Error::Configuration(
            "host certificate KRL path and URL are mutually exclusive".into(),
        ));
    }
    let revocations = match (path, url) {
        (Some(path), None) => Some(read_revocation_list(path, &revocation_signers)?),
        (None, Some(url)) => Some(download_revocation_list(url, &revocation_signers, proxy).await?),
        (None, None) => None,
        (Some(_), Some(_)) => unreachable!("the exclusive check above returned"),
    };
    Ok(Some(TrustedHostCa {
        key,
        principals: authority.principals.clone(),
        revocations,
    }))
}

async fn download_revocation_list(
    raw_url: &str,
    trusted_signers: &[ssh_key::PublicKey],
    proxy: &OutboundProxy,
) -> Result<RevocationList> {
    static CRYPTO_PROVIDER: std::sync::Once = std::sync::Once::new();

    let url = reqwest::Url::parse(raw_url).map_err(|error| {
        Error::Configuration(format!(
            "host certificate KRL URL `{raw_url}` is invalid: {error}"
        ))
    })?;
    let host = url
        .host_str()
        .ok_or_else(|| Error::Configuration("host certificate KRL URL has no host".to_string()))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::Configuration(
            "host certificate KRL URL must not contain credentials".into(),
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(Error::Configuration(
            "host certificate KRL URL must not contain query parameters or a fragment".into(),
        ));
    }
    match url.scheme() {
        "https" => {}
        "http" if is_loopback_host(host) => {}
        "http" => {
            return Err(Error::Configuration(
                "host certificate KRL URL must use HTTPS; plain HTTP is only allowed on loopback"
                    .into(),
            ))
        }
        scheme => {
            return Err(Error::Configuration(format!(
                "host certificate KRL URL scheme `{scheme}` is unsupported; use HTTPS"
            )))
        }
    }

    CRYPTO_PROVIDER.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    let builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10));
    // 与 MCP 的同一条规则（ADR 0022）：直连是显式的；读到了却用不了的代理配置在这里失败，
    // 而不是静默直连。
    let client = yukinal_net::apply(builder, &proxy.proxy, proxy.credential.as_ref())
        .and_then(|builder| {
            builder
                .build()
                .map_err(|error| format!("could not create KRL HTTP client: {error}"))
        })
        .map_err(Error::Configuration)?;
    let request = client.get(url).timeout(Duration::from_secs(15)).send();
    let mut response = tokio::time::timeout(Duration::from_secs(15), request)
        .await
        .map_err(|_| {
            Error::Transport(yukinal_net::route_context(
                &proxy.proxy,
                "host certificate KRL download timed out",
            ))
        })?
        .map_err(|error| {
            let reason = error.without_url().to_string();
            Error::Transport(format!(
                "could not download host certificate KRL `{raw_url}`: {}",
                yukinal_net::route_context(&proxy.proxy, &reason)
            ))
        })?;
    if !response.status().is_success() {
        return Err(Error::Transport(format!(
            "{}: host certificate KRL `{raw_url}` returned HTTP {}",
            yukinal_net::route_context(&proxy.proxy, "KRL request completed"),
            response.status()
        )));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_KRL_BYTES)
    {
        return Err(Error::Configuration(format!(
            "host certificate KRL `{raw_url}` exceeds the {MAX_KRL_BYTES}-byte limit"
        )));
    }

    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        let reason = error.without_url().to_string();
        Error::Transport(format!(
            "could not read host certificate KRL `{raw_url}`: {}",
            yukinal_net::route_context(&proxy.proxy, &reason)
        ))
    })? {
        if bytes.len().saturating_add(chunk.len()) as u64 > MAX_KRL_BYTES {
            return Err(Error::Configuration(format!(
                "host certificate KRL `{raw_url}` exceeds the {MAX_KRL_BYTES}-byte limit"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    RevocationList::parse(&bytes, trusted_signers).map_err(|reason| {
        Error::Configuration(format!(
            "invalid host certificate KRL downloaded from `{raw_url}`: {reason}"
        ))
    })
}

fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn read_revocation_list(
    path: &str,
    trusted_signers: &[ssh_key::PublicKey],
) -> Result<RevocationList> {
    if !std::path::Path::new(path).is_absolute() {
        return Err(Error::Configuration(format!(
            "host certificate KRL path `{path}` must be absolute"
        )));
    }
    let file = std::fs::File::open(path).map_err(|error| {
        Error::Configuration(format!(
            "could not open host certificate KRL `{path}`: {error}"
        ))
    })?;
    let mut bytes = Vec::new();
    file.take(MAX_KRL_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            Error::Configuration(format!(
                "could not read host certificate KRL `{path}`: {error}"
            ))
        })?;
    if bytes.len() as u64 > MAX_KRL_BYTES {
        return Err(Error::Configuration(format!(
            "host certificate KRL `{path}` exceeds the {MAX_KRL_BYTES}-byte limit"
        )));
    }
    RevocationList::parse(&bytes, trusted_signers).map_err(|reason| {
        Error::Configuration(format!("invalid host certificate KRL `{path}`: {reason}"))
    })
}

/// The host key or certificate presented during the handshake.
#[derive(Debug, Clone)]
pub(crate) enum PresentedKey {
    /// `SHA256:<base64 无填充>`.
    Fingerprint(String),
    Certificate {
        fingerprint: String,
        certificate: Box<ssh_key::Certificate>,
    },
}

/// 从 russh 交过来的 host key 取出本 crate 的表示。
fn presented_key(server_public_key: &PublicKeyOrCertificate) -> PresentedKey {
    match server_public_key {
        PublicKeyOrCertificate::PublicKey { key, .. } => {
            PresentedKey::Fingerprint(key.fingerprint(HashAlg::Sha256).to_string())
        }
        PublicKeyOrCertificate::Certificate(certificate) => PresentedKey::Certificate {
            fingerprint: certificate
                .public_key()
                .fingerprint(HashAlg::Sha256)
                .to_string(),
            certificate: Box::new(certificate.clone()),
        },
    }
}

/// 认证期 host key 校验：核对 against 已钉指纹；TOFU 下放行（记录在 establish）。
pub(crate) struct ConnHandler {
    /// 出错时要点名是哪台主机 —— 一条「指纹不一致」的错误如果不说是谁，用户
    /// 面对多台服务器时没法处置。
    host: String,
    expected: Option<String>,
    accept_unknown: bool,
    trusted_host_ca: Option<TrustedHostCa>,
    presented: Arc<StdMutex<Option<PresentedKey>>>,
}

impl PresentedKey {
    fn fingerprint(&self) -> &str {
        match self {
            Self::Fingerprint(fingerprint) => fingerprint,
            Self::Certificate { fingerprint, .. } => fingerprint,
        }
    }
}

impl client::Handler for ConnHandler {
    type Error = HandshakeError;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        let presented = presented_key(server_public_key);
        let fingerprint = presented.fingerprint().to_string();
        if let Ok(mut slot) = self.presented.lock() {
            *slot = Some(presented.clone());
        }

        if let Some(authority) = &self.trusted_host_ca {
            return match &presented {
                PresentedKey::Certificate { certificate, .. } => {
                    validate_host_certificate(&self.host, authority, certificate).map_err(
                        |reason| HandshakeError::Certificate {
                            host: self.host.clone(),
                            reason,
                        },
                    )?;
                    Ok(true)
                }
                PresentedKey::Fingerprint(_) => Err(HandshakeError::Certificate {
                    host: self.host.clone(),
                    reason: "a host CA is configured, but the server presented a plain host key"
                        .into(),
                }),
            };
        }

        match &self.expected {
            Some(pinned) if *pinned == fingerprint => Ok(true),
            // 不匹配在这里就变成一条带**两个**指纹的错误，而不是一个 `false`。
            Some(pinned) => Err(HandshakeError::Mismatch {
                host: self.host.clone(),
                pinned: pinned.clone(),
                presented: fingerprint,
            }),
            None => Ok(self.accept_unknown),
        }
    }
}

fn validate_host_certificate(
    host: &str,
    authority: &TrustedHostCa,
    certificate: &ssh_key::Certificate,
) -> std::result::Result<(), String> {
    if certificate.cert_type() != ssh_key::certificate::CertType::Host {
        return Err("certificate is not a host certificate".into());
    }
    certificate
        .verify_signature()
        .map_err(|error| format!("certificate signature is invalid: {error}"))?;
    if certificate.signature_key() != authority.key.key_data() {
        return Err("certificate is signed by a different CA".into());
    }
    if let Some(list) = &authority.revocations {
        if let Some(reason) = list
            .revoked_reason(certificate)
            .map_err(|error| format!("could not evaluate host certificate KRL: {error}"))?
        {
            return Err(reason.to_string());
        }
    }
    if !certificate.critical_options().is_empty() {
        return Err("certificate carries unsupported critical options".into());
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_string())?
        .as_secs();
    if now < certificate.valid_after() {
        return Err("certificate is not valid yet".into());
    }
    if now > certificate.valid_before() {
        return Err("certificate has expired".into());
    }
    if !certificate.valid_principals().iter().any(|principal| {
        authority
            .principals
            .iter()
            .any(|allowed| principal_matches(allowed, principal))
    }) {
        return Err(format!(
            "certificate principal does not match the configured host patterns for {host}"
        ));
    }
    Ok(())
}

fn principal_matches(pattern: &str, principal: &str) -> bool {
    fn matches(pattern: &[u8], value: &[u8]) -> bool {
        match pattern.split_first() {
            None => value.is_empty(),
            Some((b'*', rest)) => {
                matches(rest, value)
                    || value
                        .split_first()
                        .is_some_and(|(_, tail)| matches(pattern, tail))
            }
            Some((b'?', rest)) => value
                .split_first()
                .is_some_and(|(_, tail)| matches(rest, tail)),
            Some((expected, rest)) => value.split_first().is_some_and(|(actual, tail)| {
                expected.eq_ignore_ascii_case(actual) && matches(rest, tail)
            }),
        }
    }

    matches(pattern.trim().as_bytes(), principal.trim().as_bytes())
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
    use russh::client::Handler as _;

    use super::*;
    use crate::backend::test_support::{generated_key, test_certificate, test_host_certificate};
    use crate::backend::RusshBackend;
    use crate::{Authentication, KnownHostsPolicy, SshBackend};

    #[test]
    fn pinned_reports_unknown_for_new_hosts() {
        let store = KnownHostsStore::in_memory();
        assert_eq!(store.pinned("example.com", 22), None);
    }

    #[tokio::test]
    async fn online_krl_urls_are_https_credential_free_and_unambiguous() {
        let ca = generated_key();
        let ca_public_key = ca.public_key().to_openssh().expect("encode CA");
        for (url, expected) in [
            ("http://example.com/revoked.krl", "HTTPS"),
            (
                "https://user:secret@example.com/revoked.krl",
                "must not contain credentials",
            ),
            (
                "https://example.com/revoked.krl?token=secret",
                "query parameters",
            ),
            ("https://example.com/revoked.krl#fragment", "fragment"),
        ] {
            let authority = HostCertificateAuthority {
                ca_public_key: ca_public_key.clone(),
                principals: vec!["*.example.com".into()],
                revocation_list_path: None,
                revocation_list_url: Some(url.into()),
                revocation_list_signers: Vec::new(),
            };
            let error = parse_trusted_host_ca(Some(&authority), &OutboundProxy::default())
                .await
                .expect_err(url);
            assert!(
                error.to_string().contains(expected),
                "{url}: expected `{expected}`, got {error}"
            );
        }

        let authority = HostCertificateAuthority {
            ca_public_key,
            principals: vec!["*.example.com".into()],
            revocation_list_path: Some("/etc/ssh/revoked.krl".into()),
            revocation_list_url: Some("https://example.com/revoked.krl".into()),
            revocation_list_signers: Vec::new(),
        };
        let error = parse_trusted_host_ca(Some(&authority), &OutboundProxy::default())
            .await
            .expect_err("two KRL sources must be refused");
        assert!(error.to_string().contains("mutually exclusive"));
    }

    #[tokio::test]
    async fn krl_signer_keys_are_valid_bounded_and_distinct() {
        let ca = generated_key();
        let signer = generated_key();
        let ca_public_key = ca.public_key().to_openssh().expect("encode CA");
        let signer_public_key = signer.public_key().to_openssh().expect("encode KRL signer");
        let authority = |signers: Vec<String>| HostCertificateAuthority {
            ca_public_key: ca_public_key.clone(),
            principals: vec!["*.example.com".into()],
            revocation_list_path: None,
            revocation_list_url: None,
            revocation_list_signers: signers,
        };

        parse_trusted_host_ca(
            Some(&authority(vec![signer_public_key.clone()])),
            &OutboundProxy::default(),
        )
        .await
        .expect("an independent signer is valid");

        let error = parse_trusted_host_ca(
            Some(&authority(vec![ca_public_key.clone()])),
            &OutboundProxy::default(),
        )
        .await
        .expect_err("the host CA must not be duplicated as an independent signer");
        assert!(error.to_string().contains("distinct"), "{error}");

        let error = parse_trusted_host_ca(
            Some(&authority(vec![
                signer_public_key.clone(),
                signer_public_key.clone(),
            ])),
            &OutboundProxy::default(),
        )
        .await
        .expect_err("the same independent signer must not be duplicated");
        assert!(error.to_string().contains("distinct"), "{error}");

        let error = parse_trusted_host_ca(
            Some(&authority(vec!["not a public key".into()])),
            &OutboundProxy::default(),
        )
        .await
        .expect_err("invalid signer keys must be refused");
        assert!(
            error.to_string().contains("invalid trusted KRL signer"),
            "{error}"
        );

        let error = parse_trusted_host_ca(
            Some(&authority(vec![signer_public_key; 9])),
            &OutboundProxy::default(),
        )
        .await
        .expect_err("the signer list must be bounded");
        assert!(error.to_string().contains("at most 8"), "{error}");
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
            host_certificate_authority: None,
            outbound_proxy: OutboundProxy::default(),
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

    /// Host certificates expose the certified public key as the pin identity.
    ///
    /// This does not claim CA validation: it keeps the explicit probe -> trust flow
    /// usable instead of rejecting every certificate at the handshake.
    #[test]
    fn a_presented_key_always_becomes_a_pinnable_fingerprint() {
        let key = generated_key();
        let public = key.public_key().clone();
        let expected = public.fingerprint(HashAlg::Sha256).to_string();
        assert_eq!(
            presented_key(&PublicKeyOrCertificate::from(public.clone()))
                .fingerprint()
                .to_string(),
            expected
        );

        let ca = generated_key();
        let certificate = test_certificate(&ca, &key);
        let certified = key.public_key().fingerprint(HashAlg::Sha256).to_string();
        assert_eq!(
            presented_key(&PublicKeyOrCertificate::from(certificate))
                .fingerprint()
                .to_string(),
            certified
        );
    }

    #[test]
    fn a_trusted_ca_validates_signature_type_time_and_principal() {
        let ca = generated_key();
        let host_key = generated_key();
        let certificate = test_host_certificate(&ca, &host_key, &["api.example.test"]);
        let authority = TrustedHostCa {
            key: ca.public_key().clone(),
            principals: vec!["*.example.test".into()],
            revocations: None,
        };
        assert!(validate_host_certificate("api.example.test", &authority, &certificate).is_ok());

        let wrong_principal = TrustedHostCa {
            principals: vec!["db.example.test".into()],
            ..authority.clone()
        };
        assert!(
            validate_host_certificate("api.example.test", &wrong_principal, &certificate).is_err()
        );

        let wrong_ca = generated_key();
        let wrong_authority = TrustedHostCa {
            key: wrong_ca.public_key().clone(),
            principals: vec!["*".into()],
            revocations: None,
        };
        assert!(
            validate_host_certificate("api.example.test", &wrong_authority, &certificate).is_err()
        );
    }

    #[test]
    fn configured_principals_match_hostname_patterns_case_insensitively() {
        assert!(principal_matches("*.example.test", "API.EXAMPLE.TEST"));
        assert!(principal_matches(
            "api-??.example.test",
            "api-01.example.test"
        ));
        assert!(!principal_matches("api.example.test", "other.example.test"));
        assert!(!principal_matches("*.example.test", "example.test"));
    }

    #[tokio::test]
    async fn an_explicit_certificate_key_pin_is_accepted() {
        let key = generated_key();
        let ca = generated_key();
        let certificate = test_certificate(&ca, &key);
        let expected = key.public_key().fingerprint(HashAlg::Sha256).to_string();
        let presented = Arc::new(StdMutex::new(None));
        let mut handler = ConnHandler {
            host: "cert.example.test".into(),
            expected: Some(expected),
            accept_unknown: false,
            trusted_host_ca: None,
            presented,
        };
        assert!(
            handler
                .check_server_key(&PublicKeyOrCertificate::from(certificate))
                .await
                .expect("a matching certificate pin is valid"),
            "the certified public-key fingerprint is the explicit trust identity"
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
            host_certificate_authority: None,
            outbound_proxy: OutboundProxy::default(),
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
