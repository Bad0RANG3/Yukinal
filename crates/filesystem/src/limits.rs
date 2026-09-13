//! 文件能力的全部上限，以及有界读取的解码。
//!
//! 上限在这里各只有一份。它们原先分散在两处：Agent 宿主工具的一组常量在
//! `apps/desktop/src-tauri/src/commands/host.rs`，UI 浏览器的 1 MiB 在
//! `apps/desktop/src-tauri/src/commands/files.rs`。两个数字当初是一致的（1 MiB），
//! 而「一致」只靠纪律维持 —— 改一处忘一处是不会报错的。

/// 远端路径长度上限（**字符**数，见 `policy::validate_remote_path` 的说明）。
pub const MAX_REMOTE_PATH_CHARS: usize = 4_096;

/// Agent `filesystem.read` 未给 `maxBytes` 时的读取字节数。
pub const DEFAULT_AGENT_READ_BYTES: usize = 128 * 1024;

/// Agent `filesystem.read` 允许请求的最大字节数。
///
/// 这个数字与 `packages/shared/src/schemas/file.ts` 的 `maxBytes` 上界是同一个契约的两侧
/// （TS 侧写的是 `1024 * 1024` 字面量），改动必须两边同时改。
pub const MAX_AGENT_READ_BYTES: usize = 1024 * 1024;

/// Agent `filesystem.write` 允许写入的最大字节数。
pub const MAX_AGENT_WRITE_BYTES: usize = 512 * 1024;

/// Agent `filesystem.edit` 能安全编辑的文件最大字节数。
///
/// 编辑的形状是「读**全文** → 校验 revision → 替换 → 写回全文」，所以这个数字同时受两边约束：
/// 文件必须能整个读进来（否则 revision 与写回的缓冲区都只是前缀），而且写回去的内容不能超过
/// `write` 的上限 —— 它取 [`MAX_AGENT_WRITE_BYTES`] 正是为了后者，同时 `MAX_AGENT_EDIT_BYTES
/// <= MAX_AGENT_READ_BYTES` 由 limits 的测试钉住。
///
/// 为什么这个常量是安全性的关键而不是调参：`read` 的 revision 只描述它**读到的那些字节**。
/// 截断读取（`truncated = true`）拿到的是文件**前缀**的 revision；如果允许以它为凭据写回，
/// 「校验通过 → 写回缓冲区」就会把用户 1 MiB 的文件截成 128 KiB。拒绝超限文件是让那件事
/// 不可能的**唯一**手段 —— 不是优化，也不是可以放宽的阈值。
pub const MAX_AGENT_EDIT_BYTES: usize = MAX_AGENT_WRITE_BYTES;

/// UI 远端文件浏览器的单次读取上限（1 MiB）。浏览器无法请求更大的量。
pub const BROWSER_READ_BYTES: usize = 1024 * 1024;

/// 一次有界读取的解码结果。
///
/// `content` 与 `truncated` 是两个调用点真正要的字段；两个字节数一起带出来，是为了让
/// 「读了多少、留下多少」在日志与测试里可读，而不是只能从字符串长度反推。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedRead {
    /// 已按上限截断、并按 UTF-8 有损解码的正文。
    pub content: String,
    /// 传输是否给出了超过上限的字节（即正文被截断）。
    pub truncated: bool,
    /// 传输返回的字节数（可能大于 `bytes_kept`）。
    pub bytes_read: usize,
    /// 进入 `content` 的字节数。
    pub bytes_kept: usize,
}

/// 把一次有界读取的原始字节变成正文 + 截断标记。
///
/// 这一段原先在两个调用点各写了一遍
/// （`String::from_utf8_lossy(&bytes[..bytes.len().min(max_bytes)])` 加一个 `truncated` 标志）。
///
/// 两个刻意的选择：
/// - **上限处切断，而不是拒绝**：多出来的那一个字节是传输用来表示「还有更多」的信号，
///   截断后照样返回正文（`truncated = true`），这样 UI 与 Agent 都能看到文件开头。
/// - **有损解码**：远端文件未必是 UTF-8。切点落在多字节字符中间时会得到一个替换字符
///   （U+FFFD），这是可接受的：工具的用途是「看一眼」而不是逐字节还原。
#[must_use]
pub fn decode_bounded(bytes: &[u8], max_bytes: usize) -> BoundedRead {
    let bytes_kept = bytes.len().min(max_bytes);
    BoundedRead {
        content: String::from_utf8_lossy(&bytes[..bytes_kept]).into_owned(),
        truncated: bytes.len() > max_bytes,
        bytes_read: bytes.len(),
        bytes_kept,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        decode_bounded, BROWSER_READ_BYTES, DEFAULT_AGENT_READ_BYTES, MAX_AGENT_EDIT_BYTES,
        MAX_AGENT_READ_BYTES, MAX_AGENT_WRITE_BYTES, MAX_REMOTE_PATH_CHARS,
    };

    #[test]
    fn the_limits_are_pinned_to_their_published_values() {
        // 这些数字是对外契约（`packages/shared` 的 schema、`docs/security.md` 的说明），不是实现细节：
        // 改动它们等于改动 Agent 工具与 UI 看到的行为，所以在这里钉住而不是靠人记得。
        assert_eq!(MAX_REMOTE_PATH_CHARS, 4_096);
        assert_eq!(DEFAULT_AGENT_READ_BYTES, 131_072);
        assert_eq!(MAX_AGENT_READ_BYTES, 1_048_576);
        assert_eq!(MAX_AGENT_WRITE_BYTES, 524_288);
        assert_eq!(MAX_AGENT_EDIT_BYTES, 524_288);
        assert_eq!(BROWSER_READ_BYTES, 1_048_576);
    }

    #[test]
    fn the_default_read_is_within_the_requestable_cap() {
        // 这个关系编译期就能证明，所以钉在 const block 里：默认值一旦被改到上限之上，
        // 构建直接失败，而不是等到某个不传 maxBytes 的调用在运行时报 invalid_input。
        const { assert!(DEFAULT_AGENT_READ_BYTES <= MAX_AGENT_READ_BYTES) };
    }

    #[test]
    fn the_edit_cap_stays_within_what_a_read_can_return_in_full() {
        // 编辑的上限**必须**不超过读上限，否则 `edit` 会拿到一份截断的内容并把它当作全文写回
        // —— 那正是「文件被截成前缀」这种数据丢失。这个不等式是那条规则的可执行形式。
        const { assert!(MAX_AGENT_EDIT_BYTES <= MAX_AGENT_READ_BYTES) };
        // 一次编辑写回去的内容同样不能超过 `write` 的上限：否则编辑就成了绕过写入上限的路。
        const { assert!(MAX_AGENT_EDIT_BYTES <= MAX_AGENT_WRITE_BYTES) };
    }

    #[test]
    fn bytes_under_the_limit_are_returned_whole() {
        let decoded = decode_bounded(b"PORT=8080\n", 64);
        assert_eq!(decoded.content, "PORT=8080\n");
        assert!(!decoded.truncated);
        assert_eq!(decoded.bytes_read, 10);
        assert_eq!(decoded.bytes_kept, 10);
    }

    #[test]
    fn exactly_at_the_limit_is_not_truncated() {
        // 边界：传输返回的字节数等于上限时不算截断（`>` 而不是 `>=`）。
        let bytes = vec![b'x'; 16];
        let decoded = decode_bounded(&bytes, 16);
        assert_eq!(decoded.content.len(), 16);
        assert!(!decoded.truncated);
        assert_eq!(decoded.bytes_kept, 16);
    }

    #[test]
    fn one_byte_over_the_limit_is_truncated_and_reports_both_counts() {
        // 那个多出来的字节是传输「还有更多」的信号，不进正文。
        let bytes = b"0123456789abcdefX".to_vec();
        let decoded = decode_bounded(&bytes, 16);
        assert_eq!(decoded.content, "0123456789abcdef");
        assert!(decoded.truncated);
        assert_eq!(decoded.bytes_read, 17);
        assert_eq!(decoded.bytes_kept, 16);
    }

    #[test]
    fn a_multi_byte_character_cut_in_half_becomes_one_replacement_character() {
        // "é" 是 C3 A9：切在它中间时，剩下的半个字符解码成一个 U+FFFD，而不是丢失或 panic。
        let bytes = b"ab\xc3\xa9cd".to_vec();
        let decoded = decode_bounded(&bytes, 3);
        assert_eq!(decoded.content, "ab\u{fffd}");
        assert!(decoded.truncated);
        assert_eq!(decoded.bytes_kept, 3);
        assert_eq!(decoded.bytes_read, 6);

        // 三字节字符（"€" = E2 82 AC）切在第一个字节之后同样是**一个**替换字符：
        // 被截断的序列只算一次解码失败。
        let decoded = decode_bounded("€".as_bytes(), 1);
        assert_eq!(decoded.content, "\u{fffd}");
        assert_eq!(decoded.content.chars().count(), 1);
        assert!(decoded.truncated);
    }

    #[test]
    fn an_empty_read_is_not_truncated() {
        let decoded = decode_bounded(&[], 0);
        assert_eq!(decoded.content, "");
        assert!(!decoded.truncated);
        assert_eq!((decoded.bytes_read, decoded.bytes_kept), (0, 0));
    }

    #[test]
    fn an_unbounded_cap_keeps_everything() {
        // `usize::MAX` 是「不设上限」的既有写法（`yukinal-core` 的 `sftp_read` 用它）。
        let bytes = b"whole file".to_vec();
        let decoded = decode_bounded(&bytes, usize::MAX);
        assert_eq!(decoded.content, "whole file");
        assert!(!decoded.truncated);
    }
}
