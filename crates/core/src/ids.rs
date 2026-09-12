//! 跨语言 id 的格式规则（Rust 侧唯一一份）。
//!
//! 这些规则的真身在 `packages/shared` 的 schema 里，本模块是它们在 Rust 侧的镜像。
//! 之所以要有个模块而不是让每个调用点自己写一遍：这里的规则已经在命令层漂移过一次，
//! 而漂移的方向是**变得更宽松** —— 见 `is_stable_server_id` 的说明。

/// 契约里的 `srv_` 服务器 id，对应 `packages/shared/src/schemas/server.ts` 的
/// `SERVER_ID_SCHEMA`（`^srv_[a-z0-9]+$`）。
///
/// **这条规则曾经有三份拷贝，其中两份多允许一个下划线。** 于是 `srv_a_b` 这种字符串
/// 会被审计管道（`commands/mod.rs`）与聊天记录（`commands/chat.rs`）当作合法的服务器
/// 目标，而 `tool_execution_list`（`commands/execution.rs`）与共享 schema 会拒绝同一个
/// 字符串。分歧的那一侧恰好是**写审计记录**的那一侧 —— 它会把一条指向不存在服务器的
/// 执行记录落库，而记录视图按 id 去查的时候什么都找不到。
///
/// 严格规则没有拒绝任何真实 id：生成的 id 是 `srv` + `_` + 十六进制毫秒 + 十六进制
/// 计数器（`commands/server` 的 `next_id`），逐字符落在 `[a-z0-9]` 里。
///
/// 与 schema 的两处差别是有意的：schema 另外做 `trim()` 与 256 上限，那是 IPC 入口
/// 解析**不可信载荷**时的事；这个谓词回答的是一个更窄的问题 —— 「这个字符串是不是一个
/// 格式正确的 `srv_` id」，调用点拿到的是自己已经解析过的字段。
pub fn is_stable_server_id(value: &str) -> bool {
    value.len() > 4
        && value.starts_with("srv_")
        && value
            .bytes()
            .skip(4)
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::is_stable_server_id;

    #[test]
    fn a_generated_server_id_is_accepted() {
        assert!(is_stable_server_id("srv_01abc"));
        assert!(is_stable_server_id("srv_19a3f2b1c4d5"));
    }

    #[test]
    fn an_underscore_after_the_prefix_is_not_a_server_id() {
        // 这三条断言正是三份拷贝漂移时**缺失**的那条：两份宽松的拷贝各自只测了
        // 无下划线的正例与大小写反例，谁都没有拿一个带下划线的 id 试过。
        assert!(!is_stable_server_id("srv_01_abc"));
        assert!(!is_stable_server_id("srv_a_b"));
    }

    #[test]
    fn host_names_uppercase_and_an_empty_suffix_are_rejected() {
        assert!(!is_stable_server_id("api.example.com:22"));
        assert!(!is_stable_server_id("srv_ABC"));
        assert!(!is_stable_server_id("server_01abc"));
        assert!(!is_stable_server_id("srv_"));
        assert!(!is_stable_server_id(""));
    }
}
