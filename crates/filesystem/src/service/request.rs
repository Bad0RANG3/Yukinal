//! Agent 三个文件工具的请求类型：字段私有，唯一构造入口是各自的 `check`。
//!
//! 形状与上限校验在**构造时同步做完**，所以到达传输的请求一定已经过了路径策略 —— 这正是
//! 「非法输入即使已经按下停止也必须报 `invalid_input`」这条顺序的结构化表达。

use crate::limits::{
    DEFAULT_AGENT_READ_BYTES, MAX_AGENT_EDIT_BYTES, MAX_AGENT_READ_BYTES, MAX_AGENT_WRITE_BYTES,
};
use crate::policy::{self, AGENT_PATH_POLICY_MESSAGE};
use crate::revision::is_content_revision;

use super::error::{Error, Result};

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
    pub(super) path: String,
    pub(super) max_bytes: usize,
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
    pub(super) path: String,
    pub(super) content: String,
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
    pub(super) path: String,
    pub(super) expected_revision: String,
    pub(super) old_string: String,
    pub(super) new_string: String,
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
