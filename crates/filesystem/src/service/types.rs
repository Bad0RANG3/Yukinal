//! 传输 trait 与它交出 / 收回的数据形状。

use super::error::TransportResult;

/// 一个路径的属性，`lstat` 语义：symlink 不会被跟随。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteStat {
    pub kind: RemoteEntryKind,
    pub size: u64,
    /// 远端给出的修改时间（Unix 秒）。SFTP 的粒度是秒，守卫的粒度也就是秒。
    pub modified: Option<u32>,
}

/// `File` 是默认值：假传输与「还不知道是什么」的调用方都从这里出发，而它恰好也是最常见的答案。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RemoteEntryKind {
    #[default]
    File,
    Directory,
    Symlink,
    Other,
}

/// 替换前记下来的守卫：读取**之后**测到的大小与 mtime。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplaceGuard {
    pub size: u64,
    pub modified: Option<u32>,
}

/// 替换发布之后实测到的属性。调用方要拿它复核「发布出来的就是我们写进去的那一份」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplacedFile {
    pub size: u64,
    pub modified: Option<u32>,
}

/// 替换失败：调用方必须能区分「有人改了」与「远端做不到」。
///
/// 这三件事的下一步完全不同：并发修改要重读再试，能力缺失重试多少次都一样，metadata
/// 保不住则要用户决定是接受另一种写法还是别改这个文件。合成一句话就会把这些区别丢掉。
#[derive(Debug)]
pub enum ReplaceError {
    /// 文件在读取之后被改过，或发布之后发现不是我们写进去的那一份。
    ConcurrentChange(String),
    /// 远端不具备安全替换的能力（symlink、rename 被拒、staging 建不出来、无法确认硬链接）。
    Unsupported(String),
    /// metadata 保不住；`missing` 逐项点名。
    MetadataNotPreserved {
        message: String,
        missing: Vec<String>,
    },
    /// 传输失败；文案原样来自传输实现。
    Transport(super::error::TransportError),
}

/// 传输层交出的原始目录项：名字、类型（`file` / `dir` / …）、字节数。
///
/// 路径拼接等归一化不在传输上做：不同传输给出的名字形式不一样，而「父路径 + 名字」的规则
/// 只有一条，应该只有一份（见 [`RemoteFileService::list`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedEntry {
    pub name: String,
    pub file_type: String,
    pub size: u64,
}

/// 文件能力需要的最小传输面。
///
/// 实现方负责「能用」：桌面侧的 `TerminalFileTransport` 在每次操作前确认 SSH 连接已建立
/// （缓存命中时只是一次查找），因此调用方不必先自己连接。
///
/// 参数名是 `server_id` 而不是 `target`：本能力按契约**只有远端**一种目标（`srv_` id，
/// 见 crate 文档）。将来若要有本地实现，那意味着先有一套本机策略，而不是在这里加一个分支。
pub trait RemoteFileTransport: Send + Sync {
    /// 列出目录项（未归一化，返回传输拿到的原始形状）。
    fn list(
        &self,
        server_id: &str,
        path: &str,
    ) -> impl std::future::Future<Output = TransportResult<Vec<ListedEntry>>> + Send;

    /// 读取至多 `max_bytes` 字节。实现方可以多返回**一**个字节来示意「还有更多」，
    /// 解码由 [`decode_bounded`] 统一处理。
    fn read_bounded(
        &self,
        server_id: &str,
        path: &str,
        max_bytes: usize,
    ) -> impl std::future::Future<Output = TransportResult<Vec<u8>>> + Send;

    /// 覆盖写入（没有补丁/追加语义）。
    fn write(
        &self,
        server_id: &str,
        path: &str,
        data: &[u8],
    ) -> impl std::future::Future<Output = TransportResult<()>> + Send;

    /// One path's attributes (`lstat` semantics: a symlink is not followed).
    ///
    /// The edit guard needs this twice: once to learn what kind of thing the path is, and once
    /// to record the `size`/`mtime` that the replacement must still match.
    fn stat(
        &self,
        server_id: &str,
        path: &str,
    ) -> impl std::future::Future<Output = TransportResult<RemoteStat>> + Send;

    /// How many names point at this file, or `None` when the remote cannot say.
    ///
    /// SFTP has no link count in its attributes, so implementations ask a remote helper. `None`
    /// is a real answer — "unknown" — and the caller treats it as unsafe rather than as "one".
    fn link_count(
        &self,
        server_id: &str,
        path: &str,
    ) -> impl std::future::Future<Output = TransportResult<Option<u64>>> + Send;

    /// Replace a regular file through a same-directory staging file and a rename (ADR 0017).
    ///
    /// There is deliberately **no** fallback to an in-place write: a transport that cannot
    /// stage, verify metadata, or rename reports [`ReplaceError::Unsupported`]. The `guard` is
    /// the metadata recorded right after the read; implementations must re-check it immediately
    /// before the rename and report [`ReplaceError::ConcurrentChange`] instead of overwriting.
    fn replace_guarded(
        &self,
        server_id: &str,
        path: &str,
        guard: &ReplaceGuard,
        data: &[u8],
    ) -> impl std::future::Future<Output = Result<ReplacedFile, ReplaceError>> + Send;
}

/// 目录列表结果：请求的路径加上已归一化为绝对路径的条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteListing {
    pub path: String,
    pub entries: Vec<RemoteEntry>,
}

/// 一个目录项。`path` 是可直接再喂给 `read` 的绝对路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntry {
    pub name: String,
    pub path: String,
    pub file_type: String,
    pub size: u64,
}

/// 一次读取的结果（正文 + 是否被上限截断 + 内容 revision）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRead {
    pub path: String,
    pub content: String,
    pub truncated: bool,
    /// 读到的**原始字节**的 SHA-256（小写十六进制），算法与编码见 [`crate::revision`]。
    ///
    /// 注意它与 `content` 覆盖的范围不同：`content` 是有损解码后的正文，`revision` 是传输给出
    /// 的原始字节 —— 包括 `truncated` 时那多出来的一个字节被切掉之后的前缀。前缀的 revision
    /// 不会通过 `edit` 的校验，所以「读了一半的文件」无法被编辑授权，这是故意的。
    pub revision: String,
}

/// 一次编辑的结果：写回之后的内容 revision 与一个小结。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEdit {
    pub path: String,
    /// 写回**缓冲区**的 revision。
    ///
    /// 它不是「远端现在就是这个 revision」的证明：那是写回之后的一次额外读取才能回答的问题，
    /// 而 SFTP 没有比较并交换，多读一次也不构成保证（见 [`RemoteFileService::agent_edit`]）。
    pub revision: String,
    /// 编辑前的文件字节数（即读进来的字节数）。
    pub bytes_before: usize,
    /// 写回的字节数。
    pub bytes_after: usize,
    /// 行数变化（按 `\n` 计数，行尾风格不由本工具改写）。
    pub line_delta: i64,
}

/// 一次覆盖写的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteWrite {
    pub path: String,
    /// 写入的**字节**数（正文按 UTF-8 编码后的长度）。
    pub bytes_written: usize,
}
