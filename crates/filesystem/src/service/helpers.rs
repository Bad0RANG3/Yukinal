//! 服务内部的小纯函数：字节匹配、行数、有界解码成结果、父路径与名字拼接。

use crate::limits::decode_bounded;
use crate::revision::content_revision;

use super::types::RemoteRead;

/// `needle` 在 `haystack` 里出现的每个起始偏移。
///
/// 字节级而不是字符级：编辑写回的是原始字节，`oldString` / `newString` 只是它们的 UTF-8
/// 编码。对合法 UTF-8 输入，字节匹配与字符串匹配等价；对不合法输入，字节匹配至多给出一个
/// 不落在字符边界上的匹配 —— 那仍然是一次精确的、可由 revision 复核的替换。
///
/// 空 `needle` 返回空列表而不是「每个位置都匹配」：`AgentEditRequest::check` 已经拒绝空
/// `oldString`，这里的存在只是不让一个防御漏掉的空串变成无穷匹配。
pub(super) fn byte_match_offsets(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
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
pub(super) fn count_lines(bytes: &[u8]) -> i64 {
    bytes.iter().filter(|byte| **byte == b'\n').count() as i64
}

pub(super) fn read_result(path: &str, bytes: &[u8], max_bytes: usize) -> RemoteRead {
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
pub(super) fn join_remote_path(parent: &str, name: &str) -> String {
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
    use super::{byte_match_offsets, count_lines, join_remote_path};

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
}
