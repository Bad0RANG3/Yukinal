//! 日志脱敏：把一行**不可信**的文本变成可以安全写进日志/错误的东西。
//!
//! 从 `sidecar.rs` 拆出来，因为这是本模块里唯一一处**安全能力**：子进程的 stdout/stderr
//! 内容与远端错误消息都会经它过滤，而误判的代价是不对称的 —— 误报只是日志少一段，
//! 漏报则是一把无法收回的密钥。让它单独成文件，是为了这类代码有几个测试、改了哪些行为
//! 一眼可见，而不是埋在八百行里的两百行。
//!
//! `truncate` 也算在这里：它是「让一行日志可安全展示」的另一半（先截断再脱敏）。
//!
//! 可见性：这里的项一律 `pub(super)` —— 它们只被 `sidecar` 自己的错误消息与日志转发
//! 调用，全仓库没有别的调用者。不外泄意味着改这里时爆炸半径就是 `sidecar` 一处。
//! （`REDACTED` 还要给 `mod.rs` 的测试用，所以同样放宽到 `pub(super)`。）

/// 日志行的截断上限。
pub(super) fn truncate(line: &str) -> String {
    const MAX: usize = 200;
    let mut out: String = line.chars().take(MAX).collect();
    if line.chars().count() > MAX {
        out.push('…');
    }
    out
}

pub(super) const REDACTED: &str = "[redacted]";

/// Redact credentials before they can leave the process boundary via a diagnostic
/// error. This intentionally favors false positives: sidecar logs are diagnostic
/// only, while a leaked key cannot be recovered.
pub(super) fn redact_log_line(line: &str) -> String {
    let mut redacted = line.to_string();
    for marker in [
        "authorization",
        "api_key",
        "api-key",
        "apikey",
        "access_token",
        "access-token",
        "password",
        "token",
    ] {
        redacted = redact_named_value(&redacted, marker);
    }
    for prefix in [
        "bearer ",
        "basic ",
        "sk-",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "github_pat_",
        "akia",
    ] {
        redacted = redact_token_after_prefix(&redacted, prefix);
    }
    redacted
}

/// Keep private-key blocks out of process logs even if a future sidecar writes
/// one line at a time. The delimiters themselves are not useful diagnostics here.
pub(super) fn redact_process_log_line(line: &str, inside_private_key: &mut bool) -> String {
    let uppercase = line.to_ascii_uppercase();
    let begins_private_key =
        uppercase.contains("-----BEGIN") && uppercase.contains("PRIVATE KEY-----");
    let ends_private_key = uppercase.contains("-----END") && uppercase.contains("PRIVATE KEY-----");
    if *inside_private_key || begins_private_key {
        *inside_private_key = !ends_private_key;
        return String::from("[redacted private-key material]");
    }
    redact_log_line(line)
}

pub(super) fn redact_named_value(line: &str, marker: &str) -> String {
    let mut output = line.to_string();
    let mut search_from = 0;
    loop {
        let lowercase = output.to_ascii_lowercase();
        let Some(relative) = lowercase[search_from..].find(marker) else {
            return output;
        };
        let marker_start = search_from + relative;
        let mut cursor = marker_start + marker.len();
        let mut found_separator = false;
        while let Some(character) = output[cursor..].chars().next() {
            match character {
                ':' | '=' => {
                    cursor += character.len_utf8();
                    found_separator = true;
                    break;
                }
                ' ' | '\t' | '"' | '\'' => cursor += character.len_utf8(),
                _ => break,
            }
        }
        if !found_separator {
            search_from = cursor.max(marker_start + marker.len());
            continue;
        }
        while let Some(character) = output[cursor..].chars().next() {
            if character.is_whitespace() {
                cursor += character.len_utf8();
            } else {
                break;
            }
        }
        let quote = output[cursor..]
            .chars()
            .next()
            .filter(|character| matches!(character, '"' | '\''));
        if let Some(quote) = quote {
            cursor += quote.len_utf8();
        }
        let value_start = cursor;
        let value_end = output[value_start..]
            .char_indices()
            .find_map(|(offset, character)| {
                (if let Some(quote) = quote {
                    character == quote
                } else {
                    character.is_whitespace() || matches!(character, '&' | ',' | ';' | '}' | ']')
                })
                .then_some(value_start + offset)
            })
            .unwrap_or(output.len());
        if value_start == value_end {
            return output;
        }
        output.replace_range(value_start..value_end, REDACTED);
        search_from = value_start + REDACTED.len();
    }
}

pub(super) fn redact_token_after_prefix(line: &str, prefix: &str) -> String {
    let mut output = line.to_string();
    let mut search_from = 0;
    loop {
        let lowercase = output.to_ascii_lowercase();
        let Some(relative) = lowercase[search_from..].find(prefix) else {
            return output;
        };
        let start = search_from + relative;
        let value_start = start + prefix.len();
        let value_end = output[value_start..]
            .char_indices()
            .find_map(|(offset, character)| {
                (!character.is_ascii_alphanumeric() && !matches!(character, '-' | '_' | '.'))
                    .then_some(value_start + offset)
            })
            .unwrap_or(output.len());
        if value_end.saturating_sub(start) >= 12 {
            output.replace_range(start..value_end, REDACTED);
            search_from = start + REDACTED.len();
        } else if value_end == output.len() {
            return output;
        } else {
            search_from = value_end.max(value_start + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_keeps_long_frames_short() {
        let long = "x".repeat(400);
        assert_eq!(truncate(&long).chars().count(), 201);
        assert_eq!(truncate("short"), "short");
    }

    #[test]
    fn sidecar_diagnostics_redact_known_credential_forms() {
        let key = format!("{}{}", "sk-proj-", "abcdefghijklmnopqrstuvwxyz");
        let line = format!("authorization: Bearer {key} api_key=another-secret");
        let redacted = redact_log_line(&line);
        assert!(!redacted.contains(&key));
        assert!(!redacted.contains("another-secret"));
        assert!(redacted.contains(REDACTED));
    }

    #[test]
    fn sidecar_diagnostics_redact_multiline_private_keys() {
        let mut inside_private_key = false;
        assert_eq!(
            redact_process_log_line(
                "-----BEGIN OPENSSH PRIVATE KEY-----",
                &mut inside_private_key
            ),
            "[redacted private-key material]"
        );
        assert!(inside_private_key);
        assert_eq!(
            redact_process_log_line("base64-private-key-payload", &mut inside_private_key),
            "[redacted private-key material]"
        );
        assert_eq!(
            redact_process_log_line("-----END OPENSSH PRIVATE KEY-----", &mut inside_private_key),
            "[redacted private-key material]"
        );
        assert!(!inside_private_key);
    }
}
