//! 文件能力的失败类型与两个结果别名。
//!
//! 分类而不是码：`Error::InvalidInput` 对应宿主侧的 `invalid_input`，`Error::DeniedByPolicy`
//! 对应 `denied_by_policy`，`Error::Transport` 交给 `transport_or_cancel`。映射本身留在命令层。

/// 服务层结果。
pub type Result<T> = std::result::Result<T, Error>;

/// 传输层结果。
pub type TransportResult<T> = std::result::Result<T, TransportError>;

/// 传输层失败：连接 / SFTP channel / 取消等等。
///
/// 文案由实现方给出（桌面侧就是 `yukinal-core` 的错误字符串），本 crate 不解释、不包装它 ——
/// 宿主侧的 `transport_or_cancel` 依赖这段文字原样到达用户面前。
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct TransportError(String);

impl TransportError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// 文件能力的失败分类。
///
/// 分类而不是码：`Error::InvalidInput` 对应宿主侧的 `invalid_input`（可重试的入参问题），
/// `Error::DeniedByPolicy` 对应 `denied_by_policy`（策略拒绝，不可重试），
/// `Error::Transport` 交给 `transport_or_cancel`。
///
/// [`Error::RevisionMismatch`] 与 [`Error::FileTooLargeToEdit`] 是编辑特有的两种拒绝。它们都
/// 落在**已有的** `invalid_input` 码上（宿主侧映射见 `commands/host.rs`），因为这套词汇里
/// 没有「编辑」专属的码，而它们的文案自己就说清了下一步该做什么。新增一个码要同时改 Rust
/// 映射、`packages/shared` 的码表与文档，收益不如把话说明白。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 路径形状或上限不合法。
    #[error("{0}")]
    InvalidInput(String),
    /// 路径命中凭据/进程密钥黑名单。
    #[error("{0}")]
    DeniedByPolicy(String),
    /// 文件内容已经不是 `expectedRevision` 描述的那一份。
    ///
    /// 文案刻意同时说明「为什么」与「下一步」：这句话原样进入工具结果，而唯一正确的反应是
    /// 重新 `read` 一次再拿新的 revision 重试。
    #[error(
        "the file is not the revision that was read: expected {expected}, the file is now {actual}; re-read the file and retry with the revision the read returns"
    )]
    RevisionMismatch { expected: String, actual: String },
    /// 文件大到无法安全编辑。
    #[error(
        "the file is larger than the {limit}-byte cap this tool can read in full, so an edit would write back only the part that fits and truncate the file; filesystem.write must be used deliberately instead"
    )]
    FileTooLargeToEdit { limit: usize },
    /// 传输失败；文案原样来自传输实现。
    #[error("{0}")]
    Transport(#[from] TransportError),
}
