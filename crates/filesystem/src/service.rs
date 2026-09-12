//! 远端文件服务：传输 trait + 套在它外面的策略与上限。
//!
//! 分两层是刻意的：
//! - [`RemoteFileTransport`] 是**能力对传输的最小需求**（一次目录列表、一次有上限读取、
//!   一次覆盖写）。桌面侧用 `TerminalService`（SFTP）实现它，测试用内存假传输实现它 ——
//!   策略与上限因此可以完全离线测试，不需要真机，也不需要 Tauri。
//! - [`RemoteFileService`] 是**唯一**允许上层调用的入口。它把路径策略、字节上限与有界解码
//!   摆在传输前面，失败是类型化的 [`Error`]。
//!
//! 失败码（`invalid_input` / `denied_by_policy` / `transport`）**不在**这里决定：那是 sidecar
//! 的 host 协议词汇，映射留在 `apps/desktop/src-tauri/src/commands/host.rs`。本模块只保证
//! 「哪一类失败」是确定的，以及文案与规则同源。

use crate::limits::{
    decode_bounded, BROWSER_READ_BYTES, DEFAULT_AGENT_READ_BYTES, MAX_AGENT_EDIT_BYTES,
    MAX_AGENT_READ_BYTES, MAX_AGENT_WRITE_BYTES,
};
use crate::policy::{self, AGENT_PATH_POLICY_MESSAGE};
use crate::revision::{content_revision, is_content_revision};

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

/// Agent `filesystem.read` 的请求：不可伪造地携带「策略与上限已通过」。
///
/// 字段私有、唯一构造入口是 [`AgentReadRequest::check`]，所以 `RemoteFileService::agent_read`
/// 不可能收到一个没查过黑名单的路径。
///
/// 为什么把纯校验从 async 的读取里拆出来：宿主侧的顺序是「校验 → 取消/传输」，而**非法输入
/// 即使已经按下停止也必须报 `invalid_input`**（今天的顺序：形状 → 黑名单 → 上限都排在
/// `ensure_session` 与 `select!` 之前）。校验是同步的、可先行的，把它留在 `select!` 里面就
/// 会把这条顺序交给分支竞争的运气；反过来在命令层再抄一份规则又会重新长出第二份黑名单。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentReadRequest {
    path: String,
    max_bytes: usize,
}

impl AgentReadRequest {
    /// 校验路径与 `maxBytes`，并把缺省的读取上限解析成具体字节数。
    pub fn check(path: &str, max_bytes: Option<usize>) -> Result<Self> {
        policy::validate_remote_path(path).map_err(Error::InvalidInput)?;
        if policy::is_agent_blocked_path(path) {
            return Err(Error::DeniedByPolicy(AGENT_PATH_POLICY_MESSAGE.to_string()));
        }
        let max_bytes = max_bytes.unwrap_or(DEFAULT_AGENT_READ_BYTES);
        if !(1..=MAX_AGENT_READ_BYTES).contains(&max_bytes) {
            return Err(Error::InvalidInput(format!(
                "maxBytes must be between 1 and {MAX_AGENT_READ_BYTES}"
            )));
        }
        Ok(Self {
            path: path.to_string(),
            max_bytes,
        })
    }
}

/// Agent `filesystem.write` 的请求，同 [`AgentReadRequest`]：只能经 [`AgentWriteRequest::check`]
/// 构造。`content` 按值收下，避免为最大 512 KiB 的正文多复制一次。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentWriteRequest {
    path: String,
    content: String,
}

impl AgentWriteRequest {
    pub fn check(path: &str, content: String) -> Result<Self> {
        policy::validate_remote_path(path).map_err(Error::InvalidInput)?;
        if policy::is_agent_blocked_path(path) {
            return Err(Error::DeniedByPolicy(AGENT_PATH_POLICY_MESSAGE.to_string()));
        }
        if content.len() > MAX_AGENT_WRITE_BYTES {
            return Err(Error::InvalidInput(format!(
                "content must be at most {MAX_AGENT_WRITE_BYTES} bytes"
            )));
        }
        Ok(Self {
            path: path.to_string(),
            content,
        })
    }
}

/// Agent `filesystem.edit` 的请求：一次**有守卫的精确替换**。
///
/// 同另外两个请求类型，只能经 [`AgentEditRequest::check`] 构造，所以到达传输的编辑一定已经
/// 过路径策略。形状校验（revision 是不是 64 个十六进制字符、oldString 非空）也在这里做，
/// 而且**同步先做**：这些问题的答案不需要网络，让它们排在会话与取消之前，意味着「Agent 传了
/// 半截 revision」永远报成入参问题，而不是在一次多余的读取之后才被发现。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentEditRequest {
    path: String,
    expected_revision: String,
    old_string: String,
    new_string: String,
}

impl AgentEditRequest {
    pub fn check(
        path: &str,
        expected_revision: &str,
        old_string: String,
        new_string: String,
    ) -> Result<Self> {
        policy::validate_remote_path(path).map_err(Error::InvalidInput)?;
        if policy::is_agent_blocked_path(path) {
            return Err(Error::DeniedByPolicy(AGENT_PATH_POLICY_MESSAGE.to_string()));
        }
        if !is_content_revision(expected_revision) {
            return Err(Error::InvalidInput(
                "expectedRevision must be the 64-character hex revision returned by filesystem.read"
                    .to_string(),
            ));
        }
        // 空 oldString 在每一份内容里都出现无数次，所以「恰好一次」这条规则先把它排除掉：
        // 这里给出的是原因，不是一个空匹配的奇怪计数。
        if old_string.is_empty() {
            return Err(Error::InvalidInput(
                "oldString must not be empty: it is the text to replace and must occur exactly once"
                    .to_string(),
            ));
        }
        // 两个字符串本身先各自设上限（比文件上限更早、更省的一次拒绝）：它们都长不过一份
        // 可编辑的文件，而超限的 newString 无论如何都会让写回结果超过上限。
        if old_string.len() > MAX_AGENT_EDIT_BYTES {
            return Err(Error::InvalidInput(format!(
                "oldString must be at most {MAX_AGENT_EDIT_BYTES} bytes"
            )));
        }
        if new_string.len() > MAX_AGENT_EDIT_BYTES {
            return Err(Error::InvalidInput(format!(
                "newString must be at most {MAX_AGENT_EDIT_BYTES} bytes"
            )));
        }
        Ok(Self {
            path: path.to_string(),
            expected_revision: expected_revision.to_string(),
            old_string,
            new_string,
        })
    }
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

/// 远端文件服务：策略 + 上限 + 有界解码，套在一个 [`RemoteFileTransport`] 外面。
pub struct RemoteFileService<T> {
    transport: T,
}

impl<T: RemoteFileTransport> RemoteFileService<T> {
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    /// 目录列表（UI 浏览器用；Agent 目前没有列表工具）。
    ///
    /// 刻意不套用凭据黑名单，也不做路径形状校验：浏览器的操作者是人，而 Agent 的策略不是
    /// 给人用的（见 crate 文档）。这里唯一生效的规则是「名字归一化成绝对路径」。
    pub async fn list(&self, server_id: &str, path: &str) -> Result<RemoteListing> {
        let entries = self.transport.list(server_id, path).await?;
        Ok(RemoteListing {
            path: path.to_string(),
            entries: entries
                .into_iter()
                .map(|entry| RemoteEntry {
                    path: join_remote_path(path, &entry.name),
                    name: entry.name,
                    file_type: entry.file_type,
                    size: entry.size,
                })
                .collect(),
        })
    }

    /// UI 浏览器的读取：上限固定为 [`BROWSER_READ_BYTES`]，调用方无法要求更多，也不查黑名单。
    ///
    /// 它同样会算出 revision（与 Agent 的读取共用 [`read_result`]，算法只有一份），但命令层不
    /// 发布它：浏览器没有编辑入口，多一个字段只是没人用的契约。
    pub async fn browse_read(&self, server_id: &str, path: &str) -> Result<RemoteRead> {
        let bytes = self
            .transport
            .read_bounded(server_id, path, BROWSER_READ_BYTES)
            .await?;
        Ok(read_result(path, &bytes, BROWSER_READ_BYTES))
    }

    /// Agent `filesystem.read`：请求已经带过策略与上限，这里只做有界读取。
    pub async fn agent_read(
        &self,
        server_id: &str,
        request: &AgentReadRequest,
    ) -> Result<RemoteRead> {
        let bytes = self
            .transport
            .read_bounded(server_id, &request.path, request.max_bytes)
            .await?;
        Ok(read_result(&request.path, &bytes, request.max_bytes))
    }

    /// Agent `filesystem.write`（覆盖写）。
    pub async fn agent_write(
        &self,
        server_id: &str,
        request: &AgentWriteRequest,
    ) -> Result<RemoteWrite> {
        self.transport
            .write(server_id, &request.path, request.content.as_bytes())
            .await?;
        Ok(RemoteWrite {
            path: request.path.clone(),
            bytes_written: request.content.len(),
        })
    }

    /// Agent `filesystem.edit`：一次**有守卫的精确替换**（先读后改）。
    ///
    /// # 顺序
    /// 读全文 → 拒绝超限文件 → 校验 revision → 定位 `oldString`（必须恰好一次）→ 写回。
    /// 每一步的失败都在写回**之前**返回，所以被拒绝的编辑不会碰到文件 —— 测试用假传输的调用
    /// 记录来证明这一点，而不只是看错误类型。
    ///
    /// # 它保证什么
    /// - 只有当文件的内容与 `expectedRevision` 完全一致时才写；
    /// - 只有当 `oldString` 恰好出现一次时才写（零次或多次都会拒绝，让模型重新读、给出更多
    ///   上下文，而不是让它猜）；
    /// - 只有整份文件都读进来了才写（见 [`MAX_AGENT_EDIT_BYTES`]）—— 前缀不足以写回；
    /// - 写回的是原始**字节**：读路径的有损 UTF-8 解码只用于「看一眼」，不参与编辑，所以
    ///   一份非 UTF-8 文件不会被编辑悄悄改写成替换字符。
    ///
    /// # 它不保证什么（必须说清楚）
    /// **这不是原子操作，也不是比较并交换。** SFTP 没有 compare-and-swap，所以这里的顺序
    /// 只能是「检查、然后写」，两次网络往返之间有一个窗口：另一个写入者在这段窗口里改了文件，
    /// 它会被这次写回覆盖，谁都不会知道。窗口被收窄到「读回来的那一份内容必须是刚刚读过的
    /// 那一份」，但没有被关上。真正需要互斥的场景不该靠这个工具来兜底。
    pub async fn agent_edit(
        &self,
        server_id: &str,
        request: &AgentEditRequest,
    ) -> Result<RemoteEdit> {
        let bytes = self
            .transport
            .read_bounded(server_id, &request.path, MAX_AGENT_EDIT_BYTES)
            .await?;
        // 传输用「多给一个字节」表示还有更多（见 `decode_bounded`），所以「超过上限」就是
        // `>`：等于上限的文件是完整的，可以编辑。
        if bytes.len() > MAX_AGENT_EDIT_BYTES {
            return Err(Error::FileTooLargeToEdit {
                limit: MAX_AGENT_EDIT_BYTES,
            });
        }

        let actual = content_revision(&bytes);
        // 大小写不敏感：Agent 可能把 revision 原样抄成大写，那不是「文件变了」。
        if !actual.eq_ignore_ascii_case(&request.expected_revision) {
            return Err(Error::RevisionMismatch {
                expected: request.expected_revision.to_ascii_lowercase(),
                actual,
            });
        }

        let old = request.old_string.as_bytes();
        let start = match byte_match_offsets(&bytes, old).as_slice() {
            [] => {
                return Err(Error::InvalidInput(
                    "oldString was not found in the file; re-read it and pass text that appears in it verbatim"
                        .to_string(),
                ))
            }
            [offset] => *offset,
            matches => {
                return Err(Error::InvalidInput(format!(
                    "oldString occurs {} times in the file; re-read it and include enough surrounding context to make it unique",
                    matches.len()
                )))
            }
        };

        let mut updated = Vec::with_capacity(bytes.len() + request.new_string.len());
        updated.extend_from_slice(&bytes[..start]);
        updated.extend_from_slice(request.new_string.as_bytes());
        updated.extend_from_slice(&bytes[start + old.len()..]);
        // 一次编辑不能产出 `write` 写不了的文件：否则它就成了绕过写入上限的路径。
        if updated.len() > MAX_AGENT_EDIT_BYTES {
            return Err(Error::InvalidInput(format!(
                "the edit would produce a file of {} bytes, over the {MAX_AGENT_EDIT_BYTES}-byte cap for one operation; filesystem.write must be used deliberately instead",
                updated.len()
            )));
        }

        self.transport
            .write(server_id, &request.path, &updated)
            .await?;
        Ok(RemoteEdit {
            path: request.path.clone(),
            revision: content_revision(&updated),
            bytes_before: bytes.len(),
            bytes_after: updated.len(),
            line_delta: count_lines(&updated) - count_lines(&bytes),
        })
    }
}

/// `needle` 在 `haystack` 里出现的每个起始偏移。
///
/// 字节级而不是字符级：编辑写回的是原始字节，`oldString` / `newString` 只是它们的 UTF-8
/// 编码。对合法 UTF-8 输入，字节匹配与字符串匹配等价；对不合法输入，字节匹配至多给出一个
/// 不落在字符边界上的匹配 —— 那仍然是一次精确的、可由 revision 复核的替换。
///
/// 空 `needle` 返回空列表而不是「每个位置都匹配」：`AgentEditRequest::check` 已经拒绝空
/// `oldString`，这里的存在只是不让一个防御漏掉的空串变成无穷匹配。
fn byte_match_offsets(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return Vec::new();
    }
    haystack
        .windows(needle.len())
        .enumerate()
        .filter_map(|(offset, window)| (window == needle).then_some(offset))
        .collect()
}

/// 行数按 `\n` 计数：这里要的是**变化量**，不是一个平台相关的「行」定义。
fn count_lines(bytes: &[u8]) -> i64 {
    bytes.iter().filter(|byte| **byte == b'\n').count() as i64
}

fn read_result(path: &str, bytes: &[u8], max_bytes: usize) -> RemoteRead {
    let decoded = decode_bounded(bytes, max_bytes);
    RemoteRead {
        path: path.to_string(),
        content: decoded.content,
        truncated: decoded.truncated,
        // revision 算在**传输给出的原始字节**上，而不是有损解码后的 `content` 上：编辑校验
        // 时重算的也是原始字节，两侧必须是同一份东西（见 `crate::revision`）。传输多给的那
        // 一个「还有更多」的字节不参与 —— 它没有进入正文，也就不属于这次读取的内容。
        revision: content_revision(&bytes[..decoded.bytes_kept]),
    }
}

/// 父路径 + 条目名 → 绝对路径。
///
/// 三个分支对应三种真实输入：根目录（`/` 已经带斜杠）、带尾斜杠的目录、普通目录。直接
/// `format!("{parent}/{name}")` 会在前两种情况下产出 `//hosts` 与 `/etc//hosts`，
/// 而 SFTP 对这些形式的解释依赖服务端实现。
fn join_remote_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else if parent.ends_with('/') {
        format!("{parent}{name}")
    } else {
        format!("{parent}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{
        byte_match_offsets, count_lines, join_remote_path, AgentEditRequest, AgentReadRequest,
        AgentWriteRequest, Error, ListedEntry, RemoteFileService, RemoteFileTransport,
        TransportError, TransportResult,
    };
    use crate::limits::{
        BROWSER_READ_BYTES, DEFAULT_AGENT_READ_BYTES, MAX_AGENT_EDIT_BYTES, MAX_AGENT_READ_BYTES,
        MAX_AGENT_WRITE_BYTES,
    };
    use crate::policy::AGENT_PATH_POLICY_MESSAGE;
    use crate::revision::content_revision;

    /// 传输调用记录。用 `Arc` 是因为传输移交给服务之后，测试还要能读到它 ——
    /// 「被拦下的路径没有到达传输」只能靠这份记录证明，光看错误类型看不出来。
    /// 编辑还多一层：它必须证明「被拒绝的编辑没有写」。
    #[derive(Clone, Default)]
    struct CallLog {
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl CallLog {
        fn record(&self, call: String) {
            self.calls.lock().expect("call log lock").push(call);
        }

        fn snapshot(&self) -> Vec<String> {
            self.calls.lock().expect("call log lock").clone()
        }

        /// 到目前为止发生过的写次数。编辑的每条拒绝路径都要断言它是 0。
        fn writes(&self) -> usize {
            self.snapshot()
                .iter()
                .filter(|call| call.starts_with("write "))
                .count()
        }
    }

    /// 内存假传输：一个假文件、一次调用记录，可选地失败。
    ///
    /// `read_bounded` 会像真传输那样多给**一个**字节来表示「还有更多」，`write` 会真的替换
    /// 假文件 —— 少了这两个性质，「编辑没有把文件截断」这类断言就没有意义。
    #[derive(Default)]
    struct FakeTransport {
        log: CallLog,
        entries: Vec<ListedEntry>,
        file: Arc<Mutex<Vec<u8>>>,
        failure: Option<&'static str>,
    }

    impl FakeTransport {
        fn with_content(content: &[u8]) -> Self {
            Self {
                file: Arc::new(Mutex::new(content.to_vec())),
                ..Self::default()
            }
        }

        fn log(&self) -> CallLog {
            self.log.clone()
        }

        /// 假文件当前的内容（传输移交之后，测试只能这样看它）。
        fn file(&self) -> Vec<u8> {
            self.file.lock().expect("file lock").clone()
        }
    }

    impl RemoteFileTransport for FakeTransport {
        async fn list(&self, server_id: &str, path: &str) -> TransportResult<Vec<ListedEntry>> {
            self.log.record(format!("list {server_id} {path}"));
            if let Some(message) = self.failure {
                return Err(TransportError::new(message));
            }
            Ok(self.entries.clone())
        }

        async fn read_bounded(
            &self,
            server_id: &str,
            path: &str,
            max_bytes: usize,
        ) -> TransportResult<Vec<u8>> {
            self.log
                .record(format!("read {server_id} {path} @{max_bytes}"));
            if let Some(message) = self.failure {
                return Err(TransportError::new(message));
            }
            let file = self.file();
            // 上限 + 1 是「还有更多」的信号，与 `decode_bounded` 的约定一致。
            let end = file.len().min(max_bytes.saturating_add(1));
            Ok(file[..end].to_vec())
        }

        async fn write(&self, server_id: &str, path: &str, data: &[u8]) -> TransportResult<()> {
            self.log
                .record(format!("write {server_id} {path} {}b", data.len()));
            if let Some(message) = self.failure {
                return Err(TransportError::new(message));
            }
            *self.file.lock().expect("file lock") = data.to_vec();
            Ok(())
        }
    }

    /// revision 的合法形状占位：真正的值由 `content_revision` 算，测试里只关心「它不是
    /// 文件现在的 revision 时会被拒绝」。
    fn other_revision() -> String {
        content_revision(b"some other content")
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().expect("runtime")
    }

    #[test]
    fn joins_posix_paths_without_double_slashes() {
        assert_eq!(join_remote_path("/etc", "hosts"), "/etc/hosts");
        assert_eq!(join_remote_path("/", "hosts"), "/hosts");
        assert_eq!(join_remote_path("/etc/", "hosts"), "/etc/hosts");
    }

    #[test]
    fn byte_matching_counts_every_occurrence_and_never_loops_on_an_empty_needle() {
        assert_eq!(byte_match_offsets(b"aaa", b"a"), vec![0, 1, 2]);
        // 重叠的出现也算多次：`aaaa` 里有三个 `aa`，所以它同样不是「恰好一次」。
        assert_eq!(byte_match_offsets(b"aaaa", b"aa"), vec![0, 1, 2]);
        assert!(byte_match_offsets(b"aaa", b"b").is_empty());
        assert!(byte_match_offsets(b"aa", b"aaa").is_empty());
        assert!(byte_match_offsets(b"", b"a").is_empty());
        // 空 needle 返回空表，而不是「每个位置都匹配」：否则一次空匹配会伪造出无穷多候选。
        assert!(byte_match_offsets(b"abc", b"").is_empty());
    }

    #[test]
    fn line_counting_is_newline_based() {
        assert_eq!(count_lines(b""), 0);
        assert_eq!(count_lines(b"a"), 0);
        assert_eq!(count_lines(b"a\nb\n"), 2);
        // CRLF 只算一次换行：这里要的是内容的变化量，不是某个平台的行定义。
        assert_eq!(count_lines(b"a\r\nb\r\n"), 2);
    }

    #[test]
    fn listing_normalises_entry_paths_against_the_directory_it_listed() {
        let transport = FakeTransport {
            entries: vec![
                ListedEntry {
                    name: "hosts".to_string(),
                    file_type: "file".to_string(),
                    size: 42,
                },
                ListedEntry {
                    name: "nginx".to_string(),
                    file_type: "dir".to_string(),
                    size: 4_096,
                },
            ],
            ..FakeTransport::default()
        };
        let log = transport.log();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let listing = service.list("srv_1", "/etc").await.expect("list /etc");
            assert_eq!(listing.path, "/etc");
            assert_eq!(listing.entries[0].path, "/etc/hosts");
            assert_eq!(listing.entries[0].file_type, "file");
            assert_eq!(listing.entries[1].path, "/etc/nginx");
            assert_eq!(listing.entries[1].size, 4_096);

            let root = service.list("srv_1", "/").await.expect("list /");
            assert_eq!(root.entries[0].path, "/hosts");

            let trailing = service.list("srv_1", "/etc/").await.expect("list /etc/");
            assert_eq!(trailing.entries[0].path, "/etc/hosts");
        });

        // 归一化发生在服务里，传输只看到调用方给的原文。
        assert_eq!(
            log.snapshot(),
            vec!["list srv_1 /etc", "list srv_1 /", "list srv_1 /etc/"]
        );
    }

    #[test]
    fn an_over_cap_read_request_is_refused_before_the_transport_is_touched() {
        let transport = FakeTransport::with_content(b"PORT=8080\n");
        let log = transport.log();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            for max_bytes in [0, MAX_AGENT_READ_BYTES + 1, usize::MAX] {
                match AgentReadRequest::check("/etc/app.env", Some(max_bytes)) {
                    // 这一支在一次通过的运行里不会成立；真成立就会在这里碰到传输，
                    // 下面那次日志断言会立刻失败。
                    Ok(request) => {
                        let _ = service.agent_read("srv_1", &request).await;
                        panic!("maxBytes = {max_bytes} must not yield a request");
                    }
                    Err(error) => assert_eq!(
                        error.to_string(),
                        format!("maxBytes must be between 1 and {MAX_AGENT_READ_BYTES}"),
                        "maxBytes = {max_bytes}"
                    ),
                }
            }
        });

        assert!(
            log.snapshot().is_empty(),
            "a rejected request reached the transport: {:?}",
            log.snapshot()
        );
    }

    #[test]
    fn agent_read_uses_the_documented_default_cap_and_truncates_at_it() {
        let transport = FakeTransport::with_content(b"0123456789abcdefX");
        let log = transport.log();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let request = AgentReadRequest::check("/etc/app.env", None).expect("request");
            let read = service.agent_read("srv_1", &request).await.expect("read");
            assert_eq!(read.path, "/etc/app.env");
            assert_eq!(read.content, "0123456789abcdefX");
            assert!(!read.truncated);
            // 完整读取的 revision 描述整份内容 —— 它正是 `edit` 会拿来比较的那份摘要。
            assert_eq!(read.revision, content_revision(b"0123456789abcdefX"));

            let request = AgentReadRequest::check("/etc/app.env", Some(16)).expect("request");
            let read = service.agent_read("srv_1", &request).await.expect("read");
            assert_eq!(read.content, "0123456789abcdef");
            assert!(read.truncated);
            // 截断读取的 revision 只描述**前缀**（那多出来的第 17 个字节没进正文，也就不算
            // 这次读取的内容），所以它永远不会等于整份文件的 revision。
            assert_eq!(read.revision, content_revision(b"0123456789abcdef"));
            assert_ne!(read.revision, content_revision(b"0123456789abcdefX"));
        });

        // 默认上限是**具体字节数**发给传输的，缺省不在传输里做。
        assert_eq!(
            log.snapshot(),
            vec![
                format!("read srv_1 /etc/app.env @{DEFAULT_AGENT_READ_BYTES}"),
                "read srv_1 /etc/app.env @16".to_string(),
            ]
        );
    }

    #[test]
    fn agent_write_enforces_the_write_cap_and_counts_bytes_not_characters() {
        let transport = FakeTransport::default();
        let log = transport.log();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let at_cap = "x".repeat(MAX_AGENT_WRITE_BYTES);
            let request =
                AgentWriteRequest::check("/var/app/out.txt", at_cap).expect("at-cap request");
            let write = service.agent_write("srv_1", &request).await.expect("write");
            assert_eq!(write.path, "/var/app/out.txt");
            assert_eq!(write.bytes_written, MAX_AGENT_WRITE_BYTES);

            // 上限按字节：两个字符的 "文a" 是 4 字节。
            let request = AgentWriteRequest::check("/var/app/uni.txt", "文a".to_string())
                .expect("unicode request");
            let write = service.agent_write("srv_1", &request).await.expect("write");
            assert_eq!(write.bytes_written, 4);

            let over_cap = "x".repeat(MAX_AGENT_WRITE_BYTES + 1);
            let error = AgentWriteRequest::check("/var/app/out.txt", over_cap)
                .expect_err("over-cap content");
            assert_eq!(
                error.to_string(),
                format!("content must be at most {MAX_AGENT_WRITE_BYTES} bytes")
            );
        });

        assert_eq!(
            log.snapshot(),
            vec![
                format!("write srv_1 /var/app/out.txt {MAX_AGENT_WRITE_BYTES}b"),
                "write srv_1 /var/app/uni.txt 4b".to_string(),
            ]
        );
    }

    #[test]
    fn a_successful_edit_writes_the_replacement_and_returns_the_new_revision() {
        const BEFORE: &[u8] = b"PORT=8080\nHOST=127.0.0.1\n";
        const AFTER_FIRST: &[u8] = b"PORT=9090\nexport PORT\nHOST=127.0.0.1\n";
        const AFTER_SECOND: &[u8] = b"PORT=9090\nexport PORT\nHOST=0.0.0.0\n";
        let transport = FakeTransport::with_content(BEFORE);
        let log = transport.log();
        let file = transport.file.clone();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let request = AgentEditRequest::check(
                "/etc/app.env",
                &content_revision(BEFORE),
                "PORT=8080".to_string(),
                "PORT=9090\nexport PORT".to_string(),
            )
            .expect("request");
            let edit = service.agent_edit("srv_1", &request).await.expect("edit");

            assert_eq!(edit.path, "/etc/app.env");
            assert_eq!(edit.bytes_before, BEFORE.len());
            assert_eq!(edit.bytes_after, AFTER_FIRST.len());
            // 行数变化：`newString` 多了一行。
            assert_eq!(edit.line_delta, 1);
            // 返回的 revision 是**写回的那份内容**的摘要：下一次编辑可以直接用它。
            assert_eq!(edit.revision, content_revision(AFTER_FIRST));
            assert_eq!(
                file.lock().expect("file lock").clone(),
                AFTER_FIRST.to_vec(),
                "the transport must have received exactly the replaced bytes"
            );

            // 拿新的 revision 再编辑一次：守卫依赖的正是「编辑返回的 revision 可以继续用」。
            let request = AgentEditRequest::check(
                "/etc/app.env",
                &edit.revision,
                "HOST=127.0.0.1".to_string(),
                "HOST=0.0.0.0".to_string(),
            )
            .expect("second request");
            let second = service.agent_edit("srv_1", &request).await.expect("edit");
            assert_eq!(second.line_delta, 0);
            assert_eq!(
                file.lock().expect("file lock").clone(),
                AFTER_SECOND.to_vec()
            );
        });

        // 恰好一次读 + 一次写，读取用的是编辑上限（传输自己会多给一个字节示意「还有更多」）。
        assert_eq!(
            log.snapshot(),
            vec![
                format!("read srv_1 /etc/app.env @{MAX_AGENT_EDIT_BYTES}"),
                format!("write srv_1 /etc/app.env {}b", AFTER_FIRST.len()),
                format!("read srv_1 /etc/app.env @{MAX_AGENT_EDIT_BYTES}"),
                format!("write srv_1 /etc/app.env {}b", AFTER_SECOND.len()),
            ]
        );
    }

    #[test]
    fn an_edit_is_refused_when_the_file_is_no_longer_the_expected_revision() {
        const BEFORE: &[u8] = b"PORT=8080\n";
        let transport = FakeTransport::with_content(BEFORE);
        let log = transport.log();
        let file = transport.file.clone();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let request = AgentEditRequest::check(
                "/etc/app.env",
                &other_revision(),
                "PORT=8080".to_string(),
                "PORT=9090".to_string(),
            )
            .expect("request");
            match service.agent_edit("srv_1", &request).await {
                Err(Error::RevisionMismatch { expected, actual }) => {
                    assert_eq!(expected, other_revision());
                    assert_eq!(actual, content_revision(BEFORE));
                }
                other => panic!("expected a revision mismatch, got {other:?}"),
            }
        });

        // 关键的一条：拒绝发生在写之前，而且文件一个字节都没动。
        assert_eq!(
            log.writes(),
            0,
            "a rejected edit wrote: {:?}",
            log.snapshot()
        );
        assert_eq!(file.lock().expect("file lock").clone(), BEFORE.to_vec());
    }

    #[test]
    fn an_edit_whose_read_was_truncated_can_never_pass_the_revision_check() {
        // 这条把「前缀 revision 不能授权编辑」的规则钉在服务层：文件 6000 字节，`read` 只要
        // 了 4096，于是 Agent 手里的 revision 描述的是前 4096 字节。它拿去编辑时，`edit` 读到
        // 的却是一份完整的 6000 字节文件，两者对不上 —— 拒绝，而不是把前 4096 字节写回去。
        let content = vec![b'a'; 6_000];
        let transport = FakeTransport::with_content(&content);
        let log = transport.log();
        let file = transport.file.clone();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let read_request =
                AgentReadRequest::check("/var/log/app.log", Some(4_096)).expect("read request");
            let read = service
                .agent_read("srv_1", &read_request)
                .await
                .expect("read");
            assert!(read.truncated);

            let request = AgentEditRequest::check(
                "/var/log/app.log",
                &read.revision,
                "aa".to_string(),
                "bb".to_string(),
            )
            .expect("request");
            match service.agent_edit("srv_1", &request).await {
                Err(Error::RevisionMismatch { .. }) => {}
                other => panic!("expected a revision mismatch, got {other:?}"),
            }
        });

        assert_eq!(
            log.writes(),
            0,
            "a rejected edit wrote: {:?}",
            log.snapshot()
        );
        assert_eq!(file.lock().expect("file lock").len(), content.len());
    }

    #[test]
    fn a_file_over_the_edit_cap_is_refused_instead_of_being_truncated() {
        // **这是本能力最重要的一条行为。** `read` 的上限是 1 MiB 而编辑上限是 512 KiB，因为
        // 编辑要把整份内容读进来再原样写回：只要文件比能读到的更多，「校验 revision → 写回
        // 缓冲区」就会把用户的文件截成缓冲区那么长。假的传输会像真 SFTP 一样返回上限 + 1 个
        // 字节，所以这里走的是「发现还有更多 → 拒绝」的真实路径。
        let content = vec![b'x'; MAX_AGENT_EDIT_BYTES + 1];
        let transport = FakeTransport::with_content(&content);
        let log = transport.log();
        let file = transport.file.clone();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let request = AgentEditRequest::check(
                "/var/log/big.log",
                &content_revision(&content),
                "xx".to_string(),
                "yy".to_string(),
            )
            .expect("request");
            match service.agent_edit("srv_1", &request).await {
                Err(error @ Error::FileTooLargeToEdit { limit }) => {
                    assert_eq!(limit, MAX_AGENT_EDIT_BYTES);
                    let message = error.to_string();
                    // 文案必须自己说清两件事：为什么不能编辑，以及该改用哪个工具。
                    assert!(message.contains("larger than"), "{message}");
                    assert!(message.contains("truncate"), "{message}");
                    assert!(message.contains("filesystem.write"), "{message}");
                }
                other => panic!("expected a too-large refusal, got {other:?}"),
            }
        });

        assert_eq!(
            log.writes(),
            0,
            "a refused edit wrote: {:?}",
            log.snapshot()
        );
        assert_eq!(
            file.lock().expect("file lock").len(),
            MAX_AGENT_EDIT_BYTES + 1,
            "the user's file must be untouched"
        );
    }

    #[test]
    fn a_file_exactly_at_the_edit_cap_is_editable() {
        // 边界另一侧：等于上限的文件是**完整**读进来的（「还有更多」的信号是上限 + 1 个字节），
        // 所以它必须可以编辑 —— 否则上限就变成了一个比它宣称的更小的数字。
        let mut content = vec![b'x'; MAX_AGENT_EDIT_BYTES];
        content[MAX_AGENT_EDIT_BYTES - 1] = b'y';
        let transport = FakeTransport::with_content(&content);
        let log = transport.log();
        let file = transport.file.clone();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let request = AgentEditRequest::check(
                "/var/log/at-cap.log",
                &content_revision(&content),
                "y".to_string(),
                "z".to_string(),
            )
            .expect("request");
            let edit = service.agent_edit("srv_1", &request).await.expect("edit");
            assert_eq!(edit.bytes_before, MAX_AGENT_EDIT_BYTES);
            assert_eq!(edit.bytes_after, MAX_AGENT_EDIT_BYTES);
            assert_eq!(edit.line_delta, 0);
        });

        assert_eq!(log.writes(), 1);
        let written = file.lock().expect("file lock").clone();
        assert_eq!(written.len(), MAX_AGENT_EDIT_BYTES);
        assert_eq!(written[MAX_AGENT_EDIT_BYTES - 1], b'z');
    }

    #[test]
    fn an_edit_that_would_grow_the_file_over_the_cap_is_refused() {
        // 编辑不能成为绕过写入上限的路径：`newString` 让结果超过上限时拒绝，并且要说清改用
        // `write`。这里 `newString` 自身合法（不超过上限），超限的是**结果**。
        let mut content = vec![b'x'; MAX_AGENT_EDIT_BYTES];
        content[0] = b'y';
        let transport = FakeTransport::with_content(&content);
        let log = transport.log();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let request = AgentEditRequest::check(
                "/var/log/at-cap.log",
                &content_revision(&content),
                "y".to_string(),
                "y".repeat(MAX_AGENT_EDIT_BYTES),
            )
            .expect("request");
            match service.agent_edit("srv_1", &request).await {
                Err(Error::InvalidInput(message)) => {
                    assert!(message.contains("filesystem.write"), "{message}");
                }
                other => panic!("expected an over-cap refusal, got {other:?}"),
            }
        });

        assert_eq!(
            log.writes(),
            0,
            "a refused edit wrote: {:?}",
            log.snapshot()
        );
    }

    #[test]
    fn an_absent_or_ambiguous_old_string_is_refused_before_the_write() {
        const BEFORE: &[u8] = b"port=8080\nport=8080\n";
        let transport = FakeTransport::with_content(BEFORE);
        let log = transport.log();
        let file = transport.file.clone();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let revision = content_revision(BEFORE);

            // 出现两次：模型必须重读并给出更多上下文，而不是让工具挑一个来改。
            let ambiguous = AgentEditRequest::check(
                "/etc/app.env",
                &revision,
                "port=8080\n".to_string(),
                "port=9090\n".to_string(),
            )
            .expect("request");
            match service.agent_edit("srv_1", &ambiguous).await {
                Err(Error::InvalidInput(message)) => {
                    assert!(message.contains("occurs 2 times"), "{message}");
                    assert!(message.contains("re-read"), "{message}");
                }
                other => panic!("expected an ambiguous-match refusal, got {other:?}"),
            }

            // 一次都没有：同样要重读。
            let absent = AgentEditRequest::check(
                "/etc/app.env",
                &revision,
                "port=3000".to_string(),
                "port=9090".to_string(),
            )
            .expect("request");
            match service.agent_edit("srv_1", &absent).await {
                Err(Error::InvalidInput(message)) => {
                    assert!(message.contains("was not found"), "{message}");
                    assert!(message.contains("re-read"), "{message}");
                }
                other => panic!("expected a missing-match refusal, got {other:?}"),
            }

            // 恰好一次：通过。三者的区别只有 `oldString`，所以上面两次拒绝确实是「匹配数」判的。
            let exact = AgentEditRequest::check(
                "/etc/app.env",
                &revision,
                "port=8080\nport=8080\n".to_string(),
                "port=9090\n".to_string(),
            )
            .expect("request");
            let edit = service.agent_edit("srv_1", &exact).await.expect("edit");
            assert_eq!(edit.line_delta, -1);
        });

        assert_eq!(log.writes(), 1, "log: {:?}", log.snapshot());
        assert_eq!(
            file.lock().expect("file lock").clone(),
            b"port=9090\n".to_vec()
        );
    }

    #[test]
    fn an_empty_old_string_is_an_input_problem_not_a_match_problem() {
        // 空串在每份内容里都出现无数次，所以它先被形状校验拒掉，理由说得出话。
        match AgentEditRequest::check(
            "/etc/app.env",
            &content_revision(b"x"),
            String::new(),
            "y".to_string(),
        ) {
            Err(Error::InvalidInput(message)) => {
                assert!(message.contains("oldString must not be empty"), "{message}")
            }
            other => panic!("expected an invalid-input error, got {other:?}"),
        }
    }

    #[test]
    fn a_revision_that_is_not_a_revision_is_refused_before_the_transport_is_touched() {
        // 「Agent 传了半截字符串」必须报成入参问题：如果让它当成 revision 不匹配，模型会被
        // 送去重读一个根本没变的文件。
        let transport = FakeTransport::with_content(b"PORT=8080\n");
        let log = transport.log();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let long_z = "z".repeat(64);
            let long_a = "a".repeat(65);
            for revision in ["", "e3b0c442", long_z.as_str(), long_a.as_str()] {
                match AgentEditRequest::check(
                    "/etc/app.env",
                    revision,
                    "PORT=8080".to_string(),
                    "PORT=9090".to_string(),
                ) {
                    Ok(request) => {
                        let _ = service.agent_edit("srv_1", &request).await;
                        panic!("revision {revision:?} must not yield a request");
                    }
                    Err(Error::InvalidInput(message)) => {
                        assert!(message.contains("expectedRevision"), "{message}")
                    }
                    other => panic!("expected an invalid-input error, got {other:?}"),
                }
            }
        });

        assert!(
            log.snapshot().is_empty(),
            "a malformed revision reached the transport: {:?}",
            log.snapshot()
        );
    }

    #[test]
    fn a_revision_compared_case_insensitively_still_matches() {
        // Agent 把 revision 原样抄成大写不是「文件变了」：形状校验收大小写，比较也不应区分。
        const BEFORE: &[u8] = b"PORT=8080\n";
        let transport = FakeTransport::with_content(BEFORE);
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let request = AgentEditRequest::check(
                "/etc/app.env",
                &content_revision(BEFORE).to_uppercase(),
                "PORT=8080".to_string(),
                "PORT=9090".to_string(),
            )
            .expect("request");
            service.agent_edit("srv_1", &request).await.expect("edit");
        });
    }

    #[test]
    fn an_edit_keeps_non_utf8_bytes_intact() {
        // 有损解码（U+FFFD）是 `read` 的「看一眼」语义，不能带进写路径：一份不是 UTF-8 的文件
        // 被编辑之后，除了被替换的那几个字节之外必须**逐字节**保持原样。
        const BEFORE: &[u8] = b"\xff\xfePORT=8080\x00\n";
        let transport = FakeTransport::with_content(BEFORE);
        let file = transport.file.clone();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let request = AgentEditRequest::check(
                "/var/lib/app/blob.bin",
                &content_revision(BEFORE),
                "PORT=8080".to_string(),
                "PORT=9090".to_string(),
            )
            .expect("request");
            let edit = service.agent_edit("srv_1", &request).await.expect("edit");
            assert_eq!(edit.bytes_before, BEFORE.len());
        });

        assert_eq!(
            file.lock().expect("file lock").clone(),
            b"\xff\xfePORT=9090\x00\n".to_vec()
        );
    }

    #[test]
    fn an_edit_of_a_new_file_is_a_different_tool() {
        // `edit` 只改已经存在的内容：空文件里 `oldString` 找不到，所以「创建」仍然只能由
        // `write` 完成。这条钉住的是「编辑不是被悄悄扩成写入」。
        let transport = FakeTransport::default();
        let log = transport.log();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let request = AgentEditRequest::check(
                "/var/app/new.txt",
                &content_revision(b""),
                "anything".to_string(),
                "something".to_string(),
            )
            .expect("request");
            match service.agent_edit("srv_1", &request).await {
                Err(Error::InvalidInput(message)) => assert!(message.contains("was not found")),
                other => panic!("expected a missing-match refusal, got {other:?}"),
            }
        });

        assert_eq!(log.writes(), 0, "log: {:?}", log.snapshot());
    }

    #[test]
    fn a_blocked_path_never_reaches_the_transport() {
        let blocked = [
            "/home/deploy/.ssh/id_ed25519",
            "/home/deploy/.aws/credentials",
            "/srv/app/.env.production",
            "/run/secrets/provider-token",
            "/proc/1/environ",
            "/etc/ssl/private/service.key",
            "/etc/shadow",
        ];
        let transport = FakeTransport::with_content(b"secret");
        let log = transport.log();
        let service = RemoteFileService::new(transport);

        // 到达传输的唯一途径是一个校验过的请求。这些路径都构造不出请求，所以下面三个
        // `Ok` 分支在一次通过的运行里是死代码 —— 一旦有谁把策略放松了，它会立刻在这里
        // 碰到传输并把路径写进日志，收尾的断言随即失败。
        runtime().block_on(async {
            for path in blocked {
                match AgentReadRequest::check(path, None) {
                    Ok(request) => {
                        let _ = service.agent_read("srv_1", &request).await;
                        panic!("{path} must not yield a read request");
                    }
                    Err(error) => match error {
                        Error::DeniedByPolicy(message) => {
                            assert_eq!(message, AGENT_PATH_POLICY_MESSAGE, "{path}")
                        }
                        other => panic!("{path}: expected a policy denial, got {other:?}"),
                    },
                }
                match AgentWriteRequest::check(path, "x".to_string()) {
                    Ok(request) => {
                        let _ = service.agent_write("srv_1", &request).await;
                        panic!("{path} must not yield a write request");
                    }
                    Err(error) => match error {
                        Error::DeniedByPolicy(message) => {
                            assert_eq!(message, AGENT_PATH_POLICY_MESSAGE, "{path}")
                        }
                        other => panic!("{path}: expected a policy denial, got {other:?}"),
                    },
                }
                // 编辑尤其要拦在这里：它是唯一一个「读一份内容再写回去」的入口，如果黑名单在
                // 别的工具上生效而在这里漏掉，它就成了凭据文件的改写原语。
                match AgentEditRequest::check(
                    path,
                    &content_revision(b"secret"),
                    "old".to_string(),
                    "new".to_string(),
                ) {
                    Ok(request) => {
                        let _ = service.agent_edit("srv_1", &request).await;
                        panic!("{path} must not yield an edit request");
                    }
                    Err(error) => match error {
                        Error::DeniedByPolicy(message) => {
                            assert_eq!(message, AGENT_PATH_POLICY_MESSAGE, "{path}")
                        }
                        other => panic!("{path}: expected a policy denial, got {other:?}"),
                    },
                }
            }
        });

        assert!(
            log.snapshot().is_empty(),
            "a blocked path reached the transport: {:?}",
            log.snapshot()
        );
    }

    #[test]
    fn the_credential_policy_is_decided_before_the_shape_and_limit_checks() {
        // 顺序是对外行为的一部分，宿主侧的失败码依赖它：今天「形状 → 黑名单 → 上限」，
        // 所以带黑名单的路径即使 maxBytes 也非法，报的仍是 denied_by_policy。
        match AgentReadRequest::check("/home/deploy/.ssh/id_rsa", Some(0))
            .expect_err("blocked path with a bad cap")
        {
            Error::DeniedByPolicy(message) => assert_eq!(message, AGENT_PATH_POLICY_MESSAGE),
            other => panic!("expected a policy denial, got {other:?}"),
        }

        match AgentWriteRequest::check("/srv/app/.env", "x".repeat(MAX_AGENT_WRITE_BYTES + 1))
            .expect_err("blocked path with oversized content")
        {
            Error::DeniedByPolicy(message) => assert_eq!(message, AGENT_PATH_POLICY_MESSAGE),
            other => panic!("expected a policy denial, got {other:?}"),
        }

        // 编辑的入参校验顺序同样是「形状 → 黑名单 → 形状细节」：黑名单路径即使 revision
        // 也不合法，报的仍是 denied_by_policy，与另外两个工具一致。
        match AgentEditRequest::check(
            "/srv/app/.env.local",
            "not-a-revision",
            "a".to_string(),
            "b".to_string(),
        )
        .expect_err("blocked path with a malformed revision")
        {
            Error::DeniedByPolicy(message) => assert_eq!(message, AGENT_PATH_POLICY_MESSAGE),
            other => panic!("expected a policy denial, got {other:?}"),
        }

        // 形状校验排在黑名单之前：相对路径先报 invalid_input。
        match AgentReadRequest::check("relative/.env", None).expect_err("relative path") {
            Error::InvalidInput(message) => assert_eq!(message, "remote path must be absolute"),
            other => panic!("expected an invalid-input error, got {other:?}"),
        }
        match AgentEditRequest::check(
            "relative/.env",
            &content_revision(b"x"),
            "a".to_string(),
            "b".to_string(),
        )
        .expect_err("relative path")
        {
            Error::InvalidInput(message) => assert_eq!(message, "remote path must be absolute"),
            other => panic!("expected an invalid-input error, got {other:?}"),
        }
    }

    #[test]
    fn the_ui_browser_read_uses_its_own_cap_and_is_not_policy_blocked() {
        // 浏览器不是 Agent：`~/.ssh/id_rsa` 这类路径由人来打开是正当的。哪一天这里开始拒绝，
        // 说明有人把 Agent 的策略套到了 UI 上 —— 那是行为变更，不是加固。
        let transport = FakeTransport::with_content(b"Host web\n");
        let log = transport.log();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let read = service
                .browse_read("srv_1", "/home/deploy/.ssh/id_rsa")
                .await
                .expect("browser read");
            assert_eq!(read.path, "/home/deploy/.ssh/id_rsa");
            assert_eq!(read.content, "Host web\n");
            assert!(!read.truncated);

            let listing = service
                .list("srv_1", "/home/deploy/.ssh")
                .await
                .expect("browser list");
            assert_eq!(listing.path, "/home/deploy/.ssh");
        });

        assert_eq!(
            log.snapshot(),
            vec![
                format!("read srv_1 /home/deploy/.ssh/id_rsa @{BROWSER_READ_BYTES}"),
                "list srv_1 /home/deploy/.ssh".to_string(),
            ]
        );
    }

    #[test]
    fn transport_failures_keep_their_own_text() {
        const MESSAGE: &str = "no session cached for server `srv_1`; connect first";
        let transport = FakeTransport {
            failure: Some(MESSAGE),
            ..FakeTransport::default()
        };
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let request = AgentReadRequest::check("/etc/app.env", None).expect("request");
            match service.agent_read("srv_1", &request).await {
                Err(Error::Transport(error)) => assert_eq!(error.to_string(), MESSAGE),
                other => panic!("expected a transport error, got {other:?}"),
            }

            let request =
                AgentWriteRequest::check("/etc/app.env", "x".to_string()).expect("request");
            match service.agent_write("srv_1", &request).await {
                Err(Error::Transport(error)) => assert_eq!(error.to_string(), MESSAGE),
                other => panic!("expected a transport error, got {other:?}"),
            }

            // 编辑的第一次传输调用（读取）失败时，原文直接穿到上层，不包装、不改写：宿主侧的
            // `transport_or_cancel` 依赖这段文字判断「连接没了」还是「用户按了停止」。
            let request = AgentEditRequest::check(
                "/etc/app.env",
                &content_revision(b"x"),
                "a".to_string(),
                "b".to_string(),
            )
            .expect("request");
            match service.agent_edit("srv_1", &request).await {
                Err(Error::Transport(error)) => assert_eq!(error.to_string(), MESSAGE),
                other => panic!("expected a transport error, got {other:?}"),
            }

            match service.browse_read("srv_1", "/etc/app.env").await {
                Err(Error::Transport(error)) => assert_eq!(error.to_string(), MESSAGE),
                other => panic!("expected a transport error, got {other:?}"),
            }

            match service.list("srv_1", "/etc").await {
                Err(Error::Transport(error)) => assert_eq!(error.to_string(), MESSAGE),
                other => panic!("expected a transport error, got {other:?}"),
            }
        });
    }
}
