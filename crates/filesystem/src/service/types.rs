//! 传输 trait 与它交出 / 收回的数据形状。

use super::error::TransportResult;

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
