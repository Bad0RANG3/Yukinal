//! `backend` 的测试夹具：一次性密钥材料、一次性目录。
//!
//! 这些辅助原先内联在 `backend.rs` 的 `mod tests` 里；拆成子模块之后 `auth` 与
//! `hostkey` 的测试都要用同一份，所以单独放一处，避免抄成两份再慢慢漂移。
//!
//! 需要真机的部分（真的把 key 交给服务器、agent 真的签名）留在 `tests/live.rs`，
//! 那些由环境变量门控。

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use russh::keys::ssh_key;

use crate::ConnectionSecrets;

/// 一次性目录：`std::env::temp_dir()` 下的唯一子目录，`Drop` 时删掉。
/// 不为此引入 tempfile —— 本 crate 只有测试需要临时文件，仓库既有测试
/// （`tests/known_hosts.rs`）也是手写临时路径。
pub(crate) struct TempDir(std::path::PathBuf);

impl TempDir {
    pub(crate) fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "yukinal-ssh-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) const TEST_PASSPHRASE: &str = "yukinal-test-passphrase";

pub(crate) fn generated_key() -> ssh_key::PrivateKey {
    ssh_key::PrivateKey::random(&mut rand::rng(), ssh_key::Algorithm::Ed25519)
        .expect("generate ed25519 key")
}

pub(crate) fn pem(key: &ssh_key::PrivateKey) -> String {
    key.to_openssh(ssh_key::LineEnding::LF)
        .expect("encode as openssh pem")
        .as_str()
        .to_owned()
}

pub(crate) fn secrets_with(key_pem: String, passphrase: Option<&str>) -> ConnectionSecrets {
    ConnectionSecrets {
        password: None,
        private_key_pem: Some(key_pem),
        private_key_passphrase: passphrase.map(str::to_owned),
    }
}

/// 用一把 CA key 给一把用户 key 签一张用户证书（测试用，参数固定）。
pub(crate) fn test_certificate(
    ca: &ssh_key::PrivateKey,
    user: &ssh_key::PrivateKey,
) -> ssh_key::Certificate {
    use ssh_key::certificate::{Builder, CertType};
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after the epoch")
        .as_secs();
    let mut builder = Builder::new_with_random_nonce(
        &mut rand::rng(),
        user.public_key(),
        now.saturating_sub(3600),
        now.saturating_add(86_400),
    )
    .expect("certificate builder");
    builder.serial(1).expect("serial");
    builder.key_id("yukinal-test").expect("key id");
    builder.cert_type(CertType::User).expect("cert type");
    builder.valid_principal("testuser").expect("principal");
    builder.sign(ca).expect("sign certificate")
}
