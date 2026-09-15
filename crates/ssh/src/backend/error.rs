//! `russh::Error` / [`HandshakeError`] → [`crate::Error`] 的映射。
//!
//! **这个映射是「验证失败必须可见」的落点**：不匹配在这里变成
//! [`crate::Error::HostKeyVerification`]，两个指纹都在里面。

use crate::Error;

pub(super) fn map_send_err(error: russh::Error) -> Error {
    Error::Transport(error.to_string())
}

/// 握手失败的原因，由 `check_server_key` **当场**给出。
///
/// 为什么需要这个类型，而不是照旧返回 `false`：`check_server_key` 只能回答「行」或
/// 「不行」，指纹在返回 `false` 之后就消失了 —— 不匹配于是一路变成一个通用握手失败，
/// 用户看到「连接断了」，看不到任何指纹。而两个指纹正是判断「服务器换了密钥」还是
/// 「有人在中间」的唯一依据（ADR 0012 第 3 条）。
///
/// 它同时是 handler 的错误类型（`russh::client::Handler::Error`），所以 `client::connect`
/// 会把它**原样**交回来，`establish` 只需把它翻译成 `crate::Error`。
#[derive(Debug)]
pub(crate) enum HandshakeError {
    /// 已钉指纹与出示的不一致。
    Mismatch {
        host: String,
        pinned: String,
        presented: String,
    },
    Certificate {
        host: String,
        reason: String,
    },
    /// 握手本身的传输层失败。
    Transport(russh::Error),
}

impl From<russh::Error> for HandshakeError {
    fn from(error: russh::Error) -> Self {
        Self::Transport(error)
    }
}

impl std::fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mismatch {
                host,
                pinned,
                presented,
            } => write!(
                f,
                "host key verification failed for {host}: pinned {pinned}, presented {presented}"
            ),
            Self::Certificate { host, reason } => {
                write!(
                    f,
                    "host certificate verification failed for {host}: {reason}"
                )
            }
            Self::Transport(error) => write!(f, "ssh transport error: {error}"),
        }
    }
}

impl std::error::Error for HandshakeError {}

/// `HandshakeError` → 公开错误。**这个映射是「验证失败必须可见」的落点**：不匹配在
/// 这里变成 [`Error::HostKeyVerification`]，两个指纹都在里面。
pub(super) fn map_handshake_err(error: HandshakeError) -> Error {
    match error {
        HandshakeError::Mismatch {
            host,
            pinned,
            presented,
        } => Error::HostKeyVerification {
            host,
            pinned,
            presented,
        },
        HandshakeError::Certificate { host, reason } => Error::HostCertificate { host, reason },
        HandshakeError::Transport(error) => Error::Transport(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 「没钉子」的文案必须读起来就不是一次指纹不匹配。
    ///
    /// 这条不是措辞洁癖：旧实现把占位说明塞进 `HostKeyVerification::fingerprint`，于是
    /// 界面把它当指纹显示、用户把它当「钥匙变了」处理。两件事要能一眼分开。
    #[test]
    fn the_not_pinned_message_does_not_read_like_a_fingerprint_mismatch() {
        let rendered = Error::HostKeyNotPinned {
            host: "api.example.com".into(),
            port: 22,
        }
        .to_string();
        assert!(rendered.contains("api.example.com:22"), "{rendered}");
        assert!(rendered.contains("not pinned"), "{rendered}");
        assert!(
            rendered.contains("policy precondition failure, not a key mismatch"),
            "文案必须自己说清这是策略前置条件而不是密钥不一致：{rendered}",
        );
        assert!(
            !rendered.contains("SHA256:"),
            "没钉子的时候没有任何指纹可显示，绝不能编一个：{rendered}",
        );
    }

    /// 不匹配的文案必须给出**两个**指纹 —— 这正是这条错误存在的理由。
    #[test]
    fn the_mismatch_message_carries_both_fingerprints() {
        let pinned = "SHA256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let presented = "SHA256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let rendered = Error::HostKeyVerification {
            host: "api.example.com".into(),
            pinned: pinned.into(),
            presented: presented.into(),
        }
        .to_string();
        assert!(rendered.contains(pinned), "{rendered}");
        assert!(rendered.contains(presented), "{rendered}");
        assert!(rendered.contains("api.example.com"), "{rendered}");
        assert!(
            rendered.contains("pinned") && rendered.contains("presented"),
            "要能看出哪一个是它、哪一个是它变了：{rendered}",
        );
    }
}
