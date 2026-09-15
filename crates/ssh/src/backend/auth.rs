//! 认证材料路径：password / private key / 口令 / OpenSSH 用户证书。
//!
//! 「一种方式失败绝不静默改试另一种」在这里落地：每一类材料问题都走各自成套的错误
//! （[`crate::PrivateKeyError`] / [`crate::CertificateError`]），而不是塌缩成一句
//! 「认证失败」。

use std::sync::Arc;

use russh::client::{AuthResult, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::{ssh_key, HashAlg};

use super::agent::authenticate_with_agent;
use super::error::map_send_err;
use super::hostkey::ConnHandler;
use crate::{
    Authentication, CertificateError, ConnectionSecrets, Error, KeyboardInteractiveChallenge,
    KeyboardInteractiveHandler, KeyboardInteractivePrompt, PrivateKeyError, Result, SshConfig,
};

const MAX_INTERACTIVE_ROUNDS: usize = 8;
const MAX_INTERACTIVE_PROMPTS: usize = 16;
const MAX_INTERACTIVE_NAME_CHARS: usize = 256;
const MAX_INTERACTIVE_INSTRUCTIONS_CHARS: usize = 4_096;
const MAX_INTERACTIVE_PROMPT_CHARS: usize = 1_024;

pub(super) enum PrimaryOutcome {
    Success,
    Failure {
        keyboard_interactive_available: bool,
        error: Error,
    },
}

impl PrimaryOutcome {
    fn from_auth_result(result: AuthResult, rejected: Error) -> Self {
        match result {
            AuthResult::Success => Self::Success,
            AuthResult::Failure {
                remaining_methods, ..
            } => Self::Failure {
                keyboard_interactive_available: remaining_methods
                    .contains(&russh::MethodKind::KeyboardInteractive),
                error: rejected,
            },
        }
    }
}

/// OpenSSH 的证书配对约定：私钥 `/path/id_ed25519` 的证书是同目录的
/// `/path/id_ed25519-cert.pub`。
///
/// 这是**约定**而不是协议要求：服务器只认「证书 + 与之对应的私钥」这一对，文件名
/// 与配对无关。`ssh-keygen -s`、`ssh-add`、`ssh -i` 都按这个后缀自动配对，所以它是
/// 本 crate 推导证书位置的默认方案；推导不出来时必须显式报
/// [`CertificateError::PathUndetermined`]，不能当成「这台服务器不用证书」而退回裸
/// key 认证。
const OPENSSH_CERT_SUFFIX: &str = "-cert.pub";

pub(super) async fn authenticate(
    handle: &mut Handle<ConnHandler>,
    config: &SshConfig,
    secrets: &ConnectionSecrets,
) -> Result<()> {
    let user = config.username.as_str();
    let outcome = match &config.authentication {
        Authentication::Password { .. } => {
            let password = secrets.password.as_deref().ok_or_else(|| {
                Error::Authentication("no password resolved at the call site".into())
            })?;
            let result = handle
                .authenticate_password(user, password)
                .await
                .map_err(map_send_err)?;
            PrimaryOutcome::from_auth_result(
                result,
                Error::Authentication("server rejected the password".into()),
            )
        }
        Authentication::PrivateKey { .. } => {
            let key = load_private_key(secrets)?;
            authenticate_with_key(handle, user, key).await?
        }
        Authentication::Certificate {
            certificate_path,
            private_key_path,
            ..
        } => {
            let key = load_private_key(secrets)?;
            let path =
                derive_certificate_path(certificate_path.as_deref(), private_key_path.as_deref())?;
            let certificate = load_certificate(&path, &key)?;
            let result = handle
                .authenticate_openssh_cert(user, Arc::new(key), certificate)
                .await
                .map_err(map_send_err)?;
            PrimaryOutcome::from_auth_result(
                result,
                Error::Authentication("server rejected the certificate".into()),
            )
        }
        Authentication::Agent { socket_path } => {
            authenticate_with_agent(handle, user, socket_path.as_deref()).await?
        }
    };
    finish_primary(handle, user, outcome, secrets).await
}

/// 单次 publickey 认证。
///
/// `key` 的所有权在这里结束：解密后的私钥**不缓存**，重连时会拿
/// `ConnectionSecrets` 重新解一次。这正是 `SessionHandle` 保存 `ConnectionSecrets`
/// 而不保存明文 key 的原因 —— 口令与明文私钥的生命周期跟着调用点，不跟着会话。
async fn authenticate_with_key(
    handle: &mut Handle<ConnHandler>,
    user: &str,
    key: ssh_key::PrivateKey,
) -> Result<PrimaryOutcome> {
    let hash = best_supported_rsa_hash(handle).await?;
    let result = handle
        .authenticate_publickey(
            user,
            russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), hash),
        )
        .await
        .map_err(map_send_err)?;
    Ok(PrimaryOutcome::from_auth_result(
        result,
        Error::Authentication("server rejected the public key".into()),
    ))
}

async fn finish_primary(
    handle: &mut Handle<ConnHandler>,
    user: &str,
    outcome: PrimaryOutcome,
    secrets: &ConnectionSecrets,
) -> Result<()> {
    let PrimaryOutcome::Failure {
        keyboard_interactive_available,
        error,
    } = outcome
    else {
        return Ok(());
    };
    if !keyboard_interactive_available {
        return Err(error);
    }
    let Some(handler) = secrets.keyboard_interactive.as_deref() else {
        return Err(error);
    };
    authenticate_keyboard_interactive(handle, user, handler).await
}

/// Complete a partially accepted login with bounded keyboard-interactive rounds.
///
/// The server controls both prompt count and text, so all three are validated before
/// crossing into the UI. Responses never enter errors or logs.
async fn authenticate_keyboard_interactive(
    handle: &mut Handle<ConnHandler>,
    user: &str,
    handler: &dyn KeyboardInteractiveHandler,
) -> Result<()> {
    let mut reply = handle
        .authenticate_keyboard_interactive_start(user, None)
        .await
        .map_err(map_send_err)?;
    for _round in 0..MAX_INTERACTIVE_ROUNDS {
        match reply {
            KeyboardInteractiveAuthResponse::Success => return Ok(()),
            KeyboardInteractiveAuthResponse::Failure {
                partial_success, ..
            } => {
                return Err(Error::Authentication(if partial_success {
                    "server accepted one keyboard-interactive round but still requires another method"
                        .into()
                } else {
                    "server rejected keyboard-interactive authentication".into()
                }));
            }
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                let challenge = bounded_challenge(name, instructions, prompts)?;
                let response_count = challenge.prompts.len();
                let responses = handler.respond(&challenge).await?;
                if responses.len() != response_count {
                    return Err(Error::Authentication(format!(
                        "keyboard-interactive response count mismatch: server asked for \
                         {response_count}, caller returned {}",
                        responses.len()
                    )));
                }
                reply = handle
                    .authenticate_keyboard_interactive_respond(responses)
                    .await
                    .map_err(map_send_err)?;
            }
        }
    }
    Err(Error::Authentication(format!(
        "server exceeded the {MAX_INTERACTIVE_ROUNDS}-round keyboard-interactive limit"
    )))
}

fn bounded_challenge(
    name: String,
    instructions: String,
    prompts: Vec<russh::client::Prompt>,
) -> Result<KeyboardInteractiveChallenge> {
    if name.chars().count() > MAX_INTERACTIVE_NAME_CHARS {
        return Err(Error::Authentication(
            "keyboard-interactive challenge name is too long".into(),
        ));
    }
    if instructions.chars().count() > MAX_INTERACTIVE_INSTRUCTIONS_CHARS {
        return Err(Error::Authentication(
            "keyboard-interactive instructions are too long".into(),
        ));
    }
    if prompts.len() > MAX_INTERACTIVE_PROMPTS {
        return Err(Error::Authentication(format!(
            "keyboard-interactive challenge has more than {MAX_INTERACTIVE_PROMPTS} prompts"
        )));
    }
    let prompts = prompts
        .into_iter()
        .map(|prompt| {
            if prompt.prompt.chars().count() > MAX_INTERACTIVE_PROMPT_CHARS {
                return Err(Error::Authentication(
                    "keyboard-interactive prompt text is too long".into(),
                ));
            }
            Ok(KeyboardInteractivePrompt {
                prompt: prompt.prompt,
                echo: prompt.echo,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(KeyboardInteractiveChallenge {
        name,
        instructions,
        prompts,
    })
}

/// RSA 的签名哈希由**服务器**的 `server-sig-algs` 决定（`ssh-rsa` / `rsa-sha2-*`
/// 是三把不同的「钥匙」）；非 RSA 的 key 忽略这个值。
pub(super) async fn best_supported_rsa_hash(
    handle: &Handle<ConnHandler>,
) -> Result<Option<HashAlg>> {
    Ok(handle
        .best_supported_rsa_hash()
        .await
        .map_err(map_send_err)?
        .flatten())
}

/// 把 `ConnectionSecrets` 里的私钥材料变成可用的 `ssh_key::PrivateKey`。
///
/// 四种失败各自成套报出（见 [`PrivateKeyError`]）。口令只在函数体内被借用一次：
/// 返回的错误里不含任何口令材料，只说明「需要口令 / 口令不对 / 本就没加密」这些事实。
///
/// 空口令按「没给口令」处理：界面上的口令框留空就是这个形状，把它算成一次「错误的
/// 口令」只会让本来能连的明文 key 连不上。非空口令配**未加密**的 key 则显式报
/// [`PrivateKeyError::NotEncrypted`] —— 那说明调用点把口令配到了别的 key 上，静默
/// 忽略等于把配置错误藏起来。
fn load_private_key(secrets: &ConnectionSecrets) -> Result<ssh_key::PrivateKey> {
    let pem = secrets
        .private_key_pem
        .as_deref()
        .ok_or_else(|| Error::Authentication("no private key resolved at the call site".into()))?;
    let key = ssh_key::PrivateKey::from_openssh(pem).map_err(|error| {
        Error::PrivateKey(PrivateKeyError::Unreadable {
            reason: error.to_string(),
        })
    })?;
    let passphrase = secrets
        .private_key_passphrase
        .as_deref()
        .filter(|value| !value.is_empty());

    // 加密的 key 在 OpenSSH 格式里是可解析的（密文原样保留），所以要靠
    // `is_encrypted()` 判断，而不是指望 `from_openssh` 报错 —— 后者对加密 key 是成功的。
    if !key.is_encrypted() {
        return match passphrase {
            Some(_) => Err(Error::PrivateKey(PrivateKeyError::NotEncrypted)),
            None => Ok(key),
        };
    }
    let passphrase = passphrase.ok_or(Error::PrivateKey(PrivateKeyError::PassphraseRequired))?;
    key.decrypt(passphrase)
        .map_err(|_| Error::PrivateKey(PrivateKeyError::PassphraseRejected))
}

/// 证书位置：显式路径优先，否则按 OpenSSH 的 `-cert.pub` 约定从私钥路径推导。
///
/// 抽成纯函数是为了让「推导」这件事能被单独测：sibling 命名是**约定**而不是协议，
/// 只靠注释守不住。两者都没有时返回 [`CertificateError::PathUndetermined`] ——
/// 缺配置要报出来，不能当成「这台服务器不用证书」而退回裸 key 认证。
fn derive_certificate_path(
    certificate_path: Option<&str>,
    private_key_path: Option<&str>,
) -> Result<std::path::PathBuf> {
    if let Some(explicit) = certificate_path {
        return Ok(std::path::PathBuf::from(explicit));
    }
    private_key_path
        .map(|base| std::path::PathBuf::from(format!("{base}{OPENSSH_CERT_SUFFIX}")))
        .ok_or(Error::Certificate(CertificateError::PathUndetermined))
}

/// 读取 OpenSSH 用户证书，并确认它签的正是同时提供的那把私钥。
fn load_certificate(
    path: &std::path::Path,
    key: &ssh_key::PrivateKey,
) -> Result<ssh_key::Certificate> {
    let displayed = path.display().to_string();
    let text = std::fs::read_to_string(path).map_err(|error| {
        Error::Certificate(CertificateError::Read {
            path: displayed.clone(),
            detail: error.to_string(),
        })
    })?;
    let certificate = ssh_key::Certificate::from_openssh(&text).map_err(|error| {
        Error::Certificate(CertificateError::Parse {
            path: displayed,
            detail: error.to_string(),
        })
    })?;
    // 证书里带公钥，所以「配错文件」在本地就能查出来。发出去再让服务器回一句
    // 认证失败，用户拿到的信息会少一大截。
    if certificate.public_key() != key.public_key().key_data() {
        return Err(Error::Certificate(CertificateError::KeyMismatch));
    }
    Ok(certificate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::test_support::{
        generated_key, pem, secrets_with, test_certificate, TempDir, TEST_PASSPHRASE,
    };

    /// 明文 key：不带口令就能用，且解出来的就是同一把 key。
    #[test]
    fn plain_private_key_loads_without_a_passphrase() {
        let key = generated_key();
        let loaded = load_private_key(&secrets_with(pem(&key), None)).expect("plain key loads");
        assert_eq!(loaded.public_key(), key.public_key());
        assert!(!loaded.is_encrypted());
    }

    /// 加密 key 的三种结局必须彼此可分：没给口令 / 口令不对 / 口令对了。
    ///
    /// 这条测试存在的理由：旧实现把前两种合并成一句「认证失败」并按「不支持」拒绝，
    /// 而用户要做的事完全不同 —— 一个是去拿口令，一个是重新输，还有一个本来就能连。
    #[test]
    fn encrypted_private_key_reports_which_passphrase_problem_it_is() {
        let key = generated_key();
        let encrypted = key
            .encrypt(&mut rand::rng(), TEST_PASSPHRASE)
            .expect("encrypt key");
        assert!(encrypted.is_encrypted(), "加密后的 key 必须自报加密");
        let encoded = pem(&encrypted);

        assert!(
            matches!(
                load_private_key(&secrets_with(encoded.clone(), None)),
                Err(Error::PrivateKey(PrivateKeyError::PassphraseRequired))
            ),
            "加密 key + 没有口令 → PassphraseRequired",
        );
        assert!(
            matches!(
                load_private_key(&secrets_with(encoded.clone(), Some("not-the-passphrase"))),
                Err(Error::PrivateKey(PrivateKeyError::PassphraseRejected))
            ),
            "加密 key + 错误口令 → PassphraseRejected",
        );

        let decrypted = load_private_key(&secrets_with(encoded, Some(TEST_PASSPHRASE)))
            .expect("right passphrase");
        assert!(!decrypted.is_encrypted());
        assert_eq!(
            decrypted.public_key(),
            key.public_key(),
            "解开之后必须是同一把 key",
        );
    }

    /// 空口令按「没给」处理：界面上的口令框留空就是这个形状。
    #[test]
    fn an_empty_passphrase_counts_as_no_passphrase() {
        let key = generated_key();
        assert!(
            load_private_key(&secrets_with(pem(&key), Some(""))).is_ok(),
            "明文 key + 空口令仍然是明文 key",
        );

        let encrypted = pem(&key
            .encrypt(&mut rand::rng(), TEST_PASSPHRASE)
            .expect("encrypt"));
        assert!(matches!(
            load_private_key(&secrets_with(encrypted, Some(""))),
            Err(Error::PrivateKey(PrivateKeyError::PassphraseRequired))
        ));
    }

    /// 口令配到了没加密的 key 上：显式报错，不静默忽略（那是配置错误，不是口令错误）。
    #[test]
    fn a_passphrase_for_a_plain_key_is_reported_not_ignored() {
        let secrets = secrets_with(pem(&generated_key()), Some(TEST_PASSPHRASE));
        assert!(matches!(
            load_private_key(&secrets),
            Err(Error::PrivateKey(PrivateKeyError::NotEncrypted))
        ));
    }

    /// 解析不出来的材料与「需要口令」是两件事，不能混为一谈。
    #[test]
    fn unreadable_key_material_is_not_a_passphrase_problem() {
        let truncated =
            "-----BEGIN OPENSSH PRIVATE KEY-----\nnope\n-----END OPENSSH PRIVATE KEY-----\n";
        assert!(matches!(
            load_private_key(&secrets_with(truncated.into(), Some(TEST_PASSPHRASE))),
            Err(Error::PrivateKey(PrivateKeyError::Unreadable { .. }))
        ));
    }

    /// 调用点根本没解析出私钥时保持既有的 `Authentication` 错误
    /// （其他 crate 已经在看这条路径的文本，不跟着这次改动变）。
    #[test]
    fn absent_key_material_keeps_the_existing_authentication_error() {
        assert!(matches!(
            load_private_key(&ConnectionSecrets::empty()),
            Err(Error::Authentication(_))
        ));
    }

    /// 口令不进 `Debug`、不进错误文本。这是「绝不泄漏」这条要求的可执行部分：
    /// 拿着口令的容器与每一个相关错误变体都会被检查。
    #[test]
    fn the_passphrase_never_reaches_debug_or_display() {
        let secrets = secrets_with(pem(&generated_key()), Some(TEST_PASSPHRASE));
        let debug = format!("{secrets:?}");
        assert!(
            !debug.contains(TEST_PASSPHRASE),
            "ConnectionSecrets 的 Debug 泄漏了口令：{debug}",
        );

        let errors = [
            Error::PrivateKey(PrivateKeyError::PassphraseRequired),
            Error::PrivateKey(PrivateKeyError::PassphraseRejected),
            Error::PrivateKey(PrivateKeyError::NotEncrypted),
        ];
        for error in errors {
            let rendered = format!("{error} {error:?}");
            assert!(
                !rendered.contains(TEST_PASSPHRASE),
                "错误文本泄漏了口令：{rendered}",
            );
        }
    }

    /// sibling 推导：`<private-key-path>` → `<private-key-path>-cert.pub`；
    /// 显式路径优先；两者都没有时是显式错误，而不是「没有证书」。
    #[test]
    fn certificate_path_follows_the_openssh_sibling_convention() {
        assert_eq!(
            derive_certificate_path(None, Some("/home/u/.ssh/id_ed25519")).expect("derive sibling"),
            std::path::PathBuf::from("/home/u/.ssh/id_ed25519-cert.pub"),
        );
        assert_eq!(
            derive_certificate_path(Some("/tmp/explicit.pub"), Some("/home/u/.ssh/id_ed25519"))
                .expect("explicit wins"),
            std::path::PathBuf::from("/tmp/explicit.pub"),
        );
        assert!(matches!(
            derive_certificate_path(None, None),
            Err(Error::Certificate(CertificateError::PathUndetermined))
        ));
    }

    /// 认证路径上的 sibling 查找：磁盘上只有 `<key>` 与 `<key>-cert.pub` 时找到证书。
    #[test]
    fn certificate_is_found_next_to_the_private_key_file() {
        let dir = TempDir::new("cert-sibling");
        let ca = generated_key();
        let user = generated_key();
        let key_path = dir.path().join("id_ed25519");
        let cert_path = dir.path().join("id_ed25519-cert.pub");
        std::fs::write(&key_path, pem(&user)).expect("write key file");
        std::fs::write(
            &cert_path,
            test_certificate(&ca, &user)
                .to_openssh()
                .expect("encode cert"),
        )
        .expect("write certificate");

        let resolved = derive_certificate_path(None, Some(&key_path.display().to_string()))
            .expect("derive sibling");
        assert_eq!(resolved, cert_path);
        let certificate = load_certificate(&resolved, &user).expect("load sibling certificate");
        assert_eq!(certificate.key_id(), "yukinal-test");
    }

    /// 证书必须签的就是同时提供的那把私钥；配错文件要在本地拦下。
    #[test]
    fn certificate_must_certify_the_offered_private_key() {
        let dir = TempDir::new("cert-mismatch");
        let ca = generated_key();
        let user = generated_key();
        let other = generated_key();
        let cert_path = dir.path().join("id_ed25519-cert.pub");
        std::fs::write(
            &cert_path,
            test_certificate(&ca, &user)
                .to_openssh()
                .expect("encode cert"),
        )
        .expect("write certificate");

        let loaded = load_certificate(&cert_path, &user).expect("matching pair loads");
        assert_eq!(loaded.public_key(), user.public_key().key_data());
        assert!(matches!(
            load_certificate(&cert_path, &other),
            Err(Error::Certificate(CertificateError::KeyMismatch))
        ));
    }

    /// 证书读不出 / 根本不是证书：`Read` 与 `Parse` 分开，且都带上路径。
    #[test]
    fn unreadable_and_unparsable_certificates_are_distinguishable() {
        let dir = TempDir::new("cert-errors");
        let key = generated_key();
        let missing = dir.path().join("absent-cert.pub");
        match load_certificate(&missing, &key) {
            Err(Error::Certificate(CertificateError::Read { path, .. })) => {
                assert_eq!(path, missing.display().to_string());
            }
            other => panic!("expected a read error, got {other:?}"),
        }

        let garbage = dir.path().join("garbage-cert.pub");
        std::fs::write(&garbage, "this is not a certificate\n").expect("write garbage");
        assert!(matches!(
            load_certificate(&garbage, &key),
            Err(Error::Certificate(CertificateError::Parse { .. }))
        ));
    }

    #[test]
    fn keyboard_interactive_challenges_are_bounded_before_reaching_the_ui() {
        let oversized_prompt = bounded_challenge(
            "name".into(),
            "instructions".into(),
            vec![russh::client::Prompt {
                prompt: "x".repeat(MAX_INTERACTIVE_PROMPT_CHARS + 1),
                echo: false,
            }],
        );
        assert!(matches!(
            oversized_prompt,
            Err(Error::Authentication(message)) if message.contains("prompt text is too long")
        ));

        let too_many = bounded_challenge(
            "name".into(),
            "instructions".into(),
            (0..=MAX_INTERACTIVE_PROMPTS)
                .map(|_| russh::client::Prompt {
                    prompt: "code".into(),
                    echo: false,
                })
                .collect(),
        );
        assert!(matches!(
            too_many,
            Err(Error::Authentication(message)) if message.contains("more than")
        ));
    }
}
