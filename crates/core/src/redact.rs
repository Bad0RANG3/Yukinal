//! 日志脱敏：把一行**不可信**的文本变成可以安全写进日志/错误的东西。
//!
//! 从 `sidecar.rs` 拆出来，因为这是本 crate 里唯一一处**安全能力**：子进程的
//! stdout/stderr 内容与远端错误消息都会经它过滤，而误判的代价是不对称的 —— 误报只是
//! 日志少一段，漏报则是一把无法收回的密钥。让它单独成文件，是为了这类代码有几个测试、
//! 改了哪些行为一眼可见，而不是埋在八百行里的两百行。
//!
//! `truncate` 也算在这里：它是「让一行日志可安全展示」的另一半（先截断再脱敏）。
//!
//! 它从 `sidecar/` 搬到 crate 根，是因为 MCP 的 stdio 客户端有**同样**的需求：它也要把
//! 一个陌生子进程的 stderr 尾部与诊断尾巴交给界面。当时它够不着这里的 `pub(super)`，
//! 于是留下了一句「暂不脱敏」——而那正是这次搬运要消掉的形态：一份新的不可信文本来源，
//! 配上一份新的、迟早会与这里漂移的截断上限。脱敏逻辑只能有一份。
//!
//! 可见性：`pub(crate)` —— 同一个 crate 内的任何模块都能用，crate 之外拿不到。爆炸半径
//! 因此是「本 crate 内所有会把子进程文本写给用户的地方」，这正是应该覆盖的范围。
//! （`REDACTED` 同样放宽，供各模块的测试断言。）

/// 日志行的截断上限。
///
/// 导出为常量而不是写在 [`truncate`] 里，是因为 MCP 客户端也要知道这个上限：它保留的是
/// 「有界尾部」，行数由它自己定，但一行的长度必须与这里一致 —— 两份常数就是两套行为。
pub(crate) const TRUNCATE_MAX_CHARS: usize = 200;

pub(crate) fn truncate(line: &str) -> String {
    let mut out: String = line.chars().take(TRUNCATE_MAX_CHARS).collect();
    if line.chars().count() > TRUNCATE_MAX_CHARS {
        out.push('…');
    }
    out
}

pub(crate) const REDACTED: &str = "[redacted]";

/// Redact credentials before they can leave the process boundary via a diagnostic
/// error. This intentionally favors false positives: sidecar logs are diagnostic
/// only, while a leaked key cannot be recovered.
///
/// **顺序是安全属性，不是风格。** 前缀扫描必须排在具名扫描之前，因为具名规则只吃掉
/// 标记后面到第一个空白为止的那一段：`Authorization: Bearer <token>` 里的值是方案名
/// `Bearer`，于是整行会先变成 `Authorization: [redacted] <token>` —— 标记已被抹掉，
/// 后面那条 `bearer ` 规则就再也匹配不上，**token 原样留下**。反过来先跑前缀规则，
/// 这一段会被整体替换掉。这个漏洞是被 `crates/core/tests/mcp_stdio.rs` 的
/// `a_leaky_child_process_cannot_get_its_secrets_into_the_tails` 抓到的。
pub(crate) fn redact_log_line(line: &str) -> String {
    let mut redacted = line.to_string();
    // 先按凭据自身的形状抹（显式方案名与已知前缀），再按「标记 = 值」的形状抹。
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
    for marker in [
        "authorization",
        "api_key",
        "api-key",
        "apikey",
        "access_token",
        "access-token",
        "password",
        // `secret=` 与 `token=` 是同一种东西。它本来只出现在桌面侧的审计键名表里，
        // 日志这边漏了，于是 `banner with secret=...` 会原样进尾部（集成测试抓到的）。
        "secret",
        "token",
    ] {
        redacted = redact_named_value(&redacted, marker);
    }
    redacted
}

/// Keep private-key blocks out of process logs even if a future sidecar writes
/// one line at a time. The delimiters themselves are not useful diagnostics here.
pub(crate) fn redact_process_log_line(line: &str, inside_private_key: &mut bool) -> String {
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

pub(crate) fn redact_named_value(line: &str, marker: &str) -> String {
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

pub(crate) fn redact_token_after_prefix(line: &str, prefix: &str) -> String {
    // 多短的前缀+值才值得抹掉。
    //
    // 12 这个下限是为了避开「短匹配出现在普通句子里」：`akia` 可能是某个标识符的开头，
    // 而 `basic ` 后面跟着 `authentication`（「使用 basic 认证」）是排障日志里很常见
    // 的一句话，把它抹成 `[redacted]` 会丢掉一条真正有用的信息。
    //
    // 但 `bearer ` 是例外：`Bearer` 后面跟的东西几乎必然是凭据本身，而英文里「bearer」
    // 单独作词出现在日志里的机会极小。一个短 token（`Bearer abc`）同样是凭据，而这个
    // 仓库的取舍是明确的 —— 误报只是日志少一段，漏报是一把收不回的密钥。
    let minimum = if prefix == "bearer " { 1 } else { 12 };
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
        // `value_end == value_start` 表示前缀后面什么都没有（行尾）：没有值可抹，
        // 替换只会删掉一个方案名。
        if value_end.saturating_sub(start) >= minimum && value_end > value_start {
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

    /// 显式方案名后面的凭据必须被抹掉，**无论它有多短**。
    ///
    /// 这是被集成测试抓到的真实漏洞：具名规则先跑时，`Authorization: Bearer <token>`
    /// 会被它吃成 `Authorization: [redacted] <token>` —— 标记没了，`bearer ` 规则就再也
    /// 匹配不上，token 原样留下。顺序改了之后这条才成立，所以它值得单独钉住。
    #[test]
    fn a_credential_is_removed_even_when_it_follows_an_auth_scheme() {
        for line in [
            "Authorization: Bearer short",
            "authorization: bearer short",
            "proxy said: Bearer abc",
        ] {
            let redacted = redact_log_line(line);
            assert!(
                !redacted.contains("short") && !redacted.contains("abc"),
                "{line} -> {redacted}",
            );
            assert!(redacted.contains(REDACTED), "{line} -> {redacted}");
        }
    }

    /// `secret=` 与 `token=` / `password=` 是同一种东西，而它一度只在审计键名表里被当成
    /// 敏感词，日志这边漏了 —— 于是一条 `banner with secret=...` 会原样进诊断尾部。
    #[test]
    fn a_named_secret_value_is_redacted_like_token_or_password() {
        assert!(!redact_log_line("banner with secret=also-leak-me").contains("also-leak-me"));
        assert!(redact_log_line("secret: \"quoted-value\"").contains(REDACTED));
    }

    /// 已知的**误报**被钉在这里，而不是被藏起来。
    ///
    /// 这个函数的取舍是明说了的：误报只是日志少一段，漏报是一把收不回的密钥。所以
    /// 「使用 basic 认证」这种句子会被吃掉一个词，`bearer of bad news` 里的 `bearer of`
    /// 也一样。把它写成断言，让代价可见 —— 也让下一个人知道这是选择，不是 bug。
    #[test]
    fn known_false_positives_are_pinned_rather_than_hidden() {
        assert_eq!(
            redact_log_line("using basic authentication for this host"),
            "using [redacted] for this host",
        );
        assert_eq!(
            redact_log_line("bearer of bad news: the child exited"),
            "[redacted] bad news: the child exited",
        );
        // 反面：没有标记、没有方案名的普通行一字不改。脱敏不该变成「日志全被抹掉」。
        for line in [
            "the secret to a good log is truncation",
            "child exited with code 9",
            "docker ps listed 3 containers",
        ] {
            assert_eq!(redact_log_line(line), line, "prose must survive: {line}");
        }
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
