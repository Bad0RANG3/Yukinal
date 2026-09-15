//! 不可信形状 → 本地类型：远端声明的名字、工具与调用结果。
//!
//! 这个文件只有两件事：
//!
//! - **名字规则**（[`normalize_segment`] / [`is_segment`] / [`internal_tool_name`]）：
//!   远端拼写必须先变成合法的内部名段，才有资格拼进 `mcp.<server>.<tool>`
//!   （ADR 0004、docs/boundaries/mcp.md 的「命名空间与名称冲突」）。这里挡的是「外来拼写进入注册表」
//!   这条路，因为一旦进了注册表，审计里的名字就跟远端真正答应的名字对不上了。
//! - **本地类型**（[`McpToolDescriptor`] / [`McpToolResult`]）：把远端 JSON 变成有字段、
//!   有文档的形状，并在类型上写清哪一部分是**不可信内容**。
//!
//! 刻意不在这里做的事：不校验工具入参（本地 Zod schema 才是校验依据，
//! docs/boundaries/mcp.md 的「输入、输出与超时由本地强制」）、不翻译 JSON Schema（那是适配器的事，
//! docs/boundaries/mcp.md 的「外部工具必须先变成 Yukinal 的工具声明」）、不判断风险等级
//! （docs/boundaries/mcp.md 的「风险等级由本地决定」）。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::truncated;

/// Provider 侧工具名上限，与 `packages/shared/src/naming/tool-name.ts` 的
/// `PROVIDER_TOOL_NAME_MAX_LENGTH` 和 ADR 0004 是同一个数。
pub const PROVIDER_TOOL_NAME_MAX_LENGTH: usize = 64;

/// 内部名里属于 MCP 来源的命名空间前缀（docs/boundaries/mcp.md 的「命名空间与名称冲突」：
/// `mcp.<serverId>.<tool>`）。
pub const MCP_NAMESPACE: &str = "mcp";

/// 内部名分隔符（ADR 0004）。
const INTERNAL_SEPARATOR: char = '.';

/// Provider 侧分隔符（ADR 0004）：`docker.ps` 在 Provider 边界变成 `docker__ps`。
const PROVIDER_SEPARATOR: &str = "__";

/// 段规则。它以字符串形式存在，只是因为它要出现在错误消息里；**判断不是用它做的**
/// （[`is_segment`] 是手写的，本 crate 不引入正则依赖）。下面的单元测试用一张正反例表
/// 把这两者钉在一起，所以模式串抄错会红，而不是悄悄放宽。
pub const SEGMENT_PATTERN: &str = "^[a-z][a-z0-9]*(-[a-z0-9]+)*$";

/// `mcp__{server}__{tool}` 里两段合起来还剩多少字符：64 − 3 − 2 − 2 = 57。
const SEGMENT_BUDGET: usize = PROVIDER_TOOL_NAME_MAX_LENGTH - MCP_NAMESPACE.len() - 2 * 2;

/// 单个段（serverId 或外部工具名）的长度上限：57 / 2 = 28。
///
/// 取一半，而不是「各给 57」：两段都可能来自远端，谁也不该假定另一方短。28 保证最坏的
/// 组合（28 + 28 = 56 ≤ 57）仍然能映射成合法的 Provider 名称，失败发生在导入时，而不是
/// 某次真实调用时 `toProviderToolName()` 抛错。
pub const SEGMENT_MAX_LENGTH: usize = SEGMENT_BUDGET / 2;

/// Long remote names keep a readable prefix plus a fixed SHA-256 suffix.
///
/// 12 hex characters provide 48 bits of collision resistance while leaving 15 bytes of
/// the original spelling visible in logs and tool names.
const LONG_SEGMENT_PREFIX_LENGTH: usize = SEGMENT_MAX_LENGTH - 1 - 12;

/// 两个上限之和仍在预算里 —— 这条不变量就是「为什么取一半」的全部内容，所以让它由编译器
/// 守着，而不是只写在上面那段注释里。
const _: () = assert!(2 * SEGMENT_MAX_LENGTH <= SEGMENT_BUDGET);

/// 一个工具描述的长度上限。
///
/// 描述是给用户看的展示文本（docs/boundaries/mcp.md 的「描述文本一律视为不可信数据」），没有理由被远端
/// 塞到几 MB：它将来还会进入工具声明与界面，而「一个远端能决定宿主记住多少字节」本身就是
/// 一种资源控制权。4096 字符远超任何真实工具描述的长度。
pub const TOOL_DESCRIPTION_MAX_CHARS: usize = 4096;

/// 一个名字为什么不能成为内部工具名段。原值一起带着，因为消息必须指出是**哪个**名字被拒了
/// —— 一个只说「有名字不合法」的导入失败没法排查。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{value} cannot be an internal tool name segment: {reason}")]
pub struct NameRejection {
    /// 被拒的原拼写（已截断：它同样来自远端，可能任意长）。
    pub value: String,
    pub reason: String,
}

impl NameRejection {
    fn new(value: &str, reason: impl Into<String>) -> Self {
        Self {
            value: truncated(value),
            reason: reason.into(),
        }
    }
}

/// `value` 是不是一个合法的内部名段（ADR 0004 的模式）。
///
/// 手写而不是正则：`[a-z][a-z0-9]*(-[a-z0-9]+)*` 是一条两状态的规则，写出来比解释
/// 「为什么这个正则等价」短。合法的段**只含 ASCII**，所以下面按字节判定是安全的
/// （非 ASCII 一定落到 `_ => false`），长度也可以用字节数。
#[must_use]
pub fn is_segment(value: &str) -> bool {
    value.len() <= SEGMENT_MAX_LENGTH && matches_segment_pattern(value)
}

fn matches_segment_pattern(value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    let mut bytes = value.bytes();
    match bytes.next() {
        // 段必须以字母开头：`1password`、`-docker` 都不是合法段。
        Some(first) if first.is_ascii_lowercase() => {}
        _ => return false,
    }
    let mut previous_was_separator = false;
    for byte in bytes {
        match byte {
            b'a'..=b'z' | b'0'..=b'9' => previous_was_separator = false,
            b'-' => {
                if previous_was_separator {
                    return false;
                }
                previous_was_separator = true;
            }
            // 大写、下划线、点、空格、双下划线与全部非 ASCII 都在这里被拒。
            _ => return false,
        }
    }
    // 结尾是 `-` 的话，`previous_was_separator` 会留在这里。
    !previous_was_separator
}

/// 远端拼写 → 合法的内部名段，或者一个说得清的理由。
///
/// 已经合法的名字原样通过（**不改写**：没有理由「顺手美化」一个本来就合法的拼写）。
/// 唯一被翻译的写法是**单个**下划线，因为它是 MCP 世界里最常见的分词法（`read_file`），
/// 而且它到 `-` 的映射唯一确定，并且会被记进 [`McpToolDescriptor::remote_name`]。
///
/// 双下划线**不翻译**：它正是 Provider 侧的分隔符，`docker__get` 这类拼写就是 ADR 0004
/// 要求拒绝的遮蔽来源。除单下划线以外，任何「听起来像」的映射都是猜测，而 ADR 0004 的
/// 原话是「无法映射的名称不会被猜测」。
pub fn normalize_segment(raw: &str) -> Result<String, NameRejection> {
    if raw.is_empty() {
        return Err(NameRejection::new(raw, "the name is empty"));
    }
    if raw.contains(PROVIDER_SEPARATOR) {
        return Err(NameRejection::new(
            raw,
            format!(
                "it contains `{PROVIDER_SEPARATOR}`, which is this project's Provider-side separator \
                 (ADR 0004); translating it would let a foreign spelling shadow a built-in tool name"
            ),
        ));
    }
    let candidate = raw.replace('_', "-");
    if is_segment(&candidate) {
        return Ok(candidate);
    }
    if candidate.len() > SEGMENT_MAX_LENGTH && matches_segment_pattern(&candidate) {
        return Ok(shorten_segment(&candidate));
    }
    Err(NameRejection::new(raw, rejection_reason(raw)))
}

fn shorten_segment(candidate: &str) -> String {
    let mut prefix: String = candidate.chars().take(LONG_SEGMENT_PREFIX_LENGTH).collect();
    while prefix.ends_with('-') {
        prefix.pop();
    }
    let digest = format!("{:x}", Sha256::digest(candidate.as_bytes()));
    format!("{prefix}-{}", &digest[..12])
}

/// 为什么这个名字连单下划线翻译之后也不合法。
fn rejection_reason(raw: &str) -> String {
    if !raw.is_ascii() {
        return "it contains non-ASCII characters".to_string();
    }
    if raw.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return "it contains uppercase letters; folding case would make different remote spellings \
                the same internal tool, and the remote would never see the name it declared"
            .to_string();
    }
    format!("it does not match `{SEGMENT_PATTERN}`")
}

/// 内部工具名 `mcp.<server>.<tool>`（ADR 0004 / docs/boundaries/mcp.md 的「命名空间与名称冲突」）。
///
/// 合起来超限的组合在这里被拦住。在 `SEGMENT_MAX_LENGTH` 取 `SEGMENT_BUDGET / 2` 的今天，
/// 两段各自合法就不可能超预算（编译期已断言），所以这一步**不会**触发；它留着，是因为
/// 两个上限是独立的常量，而「有人把段上限调大到 29」正是必须有东西拦住的那一步。
pub fn internal_tool_name(
    server_segment: &str,
    tool_segment: &str,
) -> Result<String, NameRejection> {
    if !is_segment(server_segment) {
        return Err(NameRejection::new(
            server_segment,
            format!("the server segment must match `{SEGMENT_PATTERN}`"),
        ));
    }
    if !is_segment(tool_segment) {
        return Err(NameRejection::new(
            tool_segment,
            format!("the tool segment must match `{SEGMENT_PATTERN}`"),
        ));
    }
    let projected = MCP_NAMESPACE.len()
        + 2 * PROVIDER_SEPARATOR.len()
        + server_segment.len()
        + tool_segment.len();
    if projected > PROVIDER_TOOL_NAME_MAX_LENGTH {
        return Err(NameRejection::new(
            tool_segment,
            format!(
                "`{MCP_NAMESPACE}{PROVIDER_SEPARATOR}{server_segment}{PROVIDER_SEPARATOR}{tool_segment}` \
                 would be {projected} characters at the Provider boundary; the limit is \
                 {PROVIDER_TOOL_NAME_MAX_LENGTH}"
            ),
        ));
    }
    Ok(format!(
        "{MCP_NAMESPACE}{INTERNAL_SEPARATOR}{server_segment}{INTERNAL_SEPARATOR}{tool_segment}"
    ))
}

/// 一个工具的本地声明：远端说了什么，以及它对应哪个内部名字。
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolDescriptor {
    /// 已校验的内部名段。只有它能进入 `mcp.<server>.<name>`。
    pub name: String,
    /// 远端自己的拼写，仅在它与 `name` 不同时才存在。
    ///
    /// 两个名字都留着是 ADR 0004 的直接要求：`tools/call` 必须发远端声明过的拼写
    /// （服务端只认自己的名字），而注册进 ToolRegistry 的必须是合法段。一次改写过却没有
    /// 记录的改名，就是 ADR 0004 拒绝的「无法审计的工具」。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_name: Option<String>,
    /// 不可信展示文本（docs/boundaries/mcp.md 的「描述文本一律视为不可信数据」）：可以给用户看，
    /// 绝不当作指令、绝不拼进系统提示词。
    pub description: String,
    /// 远端声明的 JSON Schema，原样保留。**它不是校验依据**：docs/boundaries/mcp.md 的
    /// 「输入、输出与超时由本地强制」要求一次调用接受什么由本地 schema 决定，
    /// 远端声明只提供翻译来源。这里保存它，只是因为翻译需要它。
    pub input_schema: Value,
    /// 同上，`outputSchema` 也只是翻译来源；远端没声明时为空。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
}

impl McpToolDescriptor {
    /// `tools/call` 要发的名字：远端声明过的那个，而不是我们内部的段。
    #[must_use]
    pub fn call_name(&self) -> &str {
        self.remote_name.as_deref().unwrap_or(&self.name)
    }

    /// 这个工具的名字是否被本地规范化改写过（只可能发生在单下划线身上）。
    #[must_use]
    pub fn renamed(&self) -> bool {
        self.remote_name.is_some()
    }

    /// 内部工具名 `mcp.<server>.<tool>`（ADR 0004）。
    pub fn internal_name(&self, server_segment: &str) -> Result<String, NameRejection> {
        internal_tool_name(server_segment, &self.name)
    }
}

/// `tools/call` 的结果。
///
/// [`McpToolResult::is_error`] 是**服务端**对这次工具执行的判断，不是传输层失败：`true`
/// 表示「工具跑了，但它自己报了错」（文件不存在、命令返回非零……）。传输层出问题一律是
/// `Err(McpError::…)`。两者必须可区分，否则「工具说没有这个文件」和「服务进程崩了」在
/// 界面与审计里会长得一模一样。
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolResult {
    pub is_error: bool,
    /// 结果内容块，逐字保留。本模块不解释它（docs/boundaries/mcp.md 的「描述文本一律视为不可信数据」）。
    pub content: Vec<McpContentBlock>,
    /// 服务端给的 `structuredContent`，原样保留；没给时为空。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
}

impl McpToolResult {
    /// 把文本块按顺序连起来，供展示与诊断用。
    ///
    /// 仍然是不可信内容：这个方法只是「把块拼成一段」，它不做任何解释，也不把结果交给模型
    /// 当作可信上下文。
    #[must_use]
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(McpContentBlock::text)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// 一个结果内容块。
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpContentBlock {
    Text {
        text: String,
    },
    /// 其它块类型（image / audio / resource / resource_link……）原样保留：本模块不翻译
    /// 内容，只搬运内容，翻译是适配器的事
    /// （docs/boundaries/mcp.md 的「外部工具必须先变成 Yukinal 的工具声明」）。
    Other {
        value: Value,
    },
}

impl McpContentBlock {
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text),
            Self::Other { .. } => None,
        }
    }
}

/// 一次进程退出：怎么死的，什么时候。
///
/// `code` 与 `signal` 分开保留，因为「它死了」不是可行动的信息，而「退出码 7」是。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpExitRecord {
    pub code: Option<i32>,
    pub signal: Option<String>,
    pub at: String,
}

impl McpExitRecord {
    /// 给人看的一行。`reason` 与 `code`/`signal` 是同一事实的两个用途：前者进消息，后者给
    /// 调用方（以及测试）判定 —— 断言「退出码是 7」比断言消息里有某个子串硬。
    #[must_use]
    pub fn reason(&self) -> String {
        match (self.code, self.signal.as_deref()) {
            (Some(code), _) => format!("exit code {code}"),
            (None, Some(signal)) => format!("signal {signal}"),
            // 既没有退出码也没有信号，只能是坏掉的退出状态：说「未知」，而不是说「正常」。
            (None, None) => "no exit status".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rejected(raw: &str) -> NameRejection {
        normalize_segment(raw).expect_err("this spelling must be refused, not translated")
    }

    #[test]
    fn legal_segments_pass_through_unchanged() {
        for value in ["docker", "read-file", "mcp-1", "a", "a1-b2-c3"] {
            assert_eq!(
                normalize_segment(value).expect("legal segment"),
                value,
                "a spelling that is already legal must not be rewritten"
            );
            assert!(is_segment(value), "{value}");
        }
    }

    #[test]
    fn a_single_underscore_is_the_one_translation() {
        // 唯一被翻译的写法，理由写在 `normalize_segment` 上：它在 MCP 世界里是分词符，
        // 映射唯一。数据库里已有的 `mcp_1` 这类 id 也靠它才能变成合法段。
        assert_eq!(
            normalize_segment("read_file").expect("translated"),
            "read-file"
        );
        assert_eq!(normalize_segment("mcp_1").expect("translated"), "mcp-1");
        assert_eq!(normalize_segment("a_b_c").expect("translated"), "a-b-c");
    }

    #[test]
    fn a_foreign_spelling_that_would_shadow_a_builtin_is_rejected() {
        // ADR 0004 点名的例子：`docker__get` 与内部的 `docker.get` 会投影到同一个 Provider
        // 名称，所以它必须在导入期被拒绝，而不是被翻译成 `docker--get` 之类。
        let error = rejected("docker__get");
        assert!(
            error.reason.contains("Provider-side separator"),
            "{}",
            error.reason
        );
        assert_eq!(error.value, "docker__get");
    }

    #[test]
    fn names_that_cannot_be_mapped_without_guessing_are_rejected() {
        for (raw, expected_reason) in [
            ("", "empty"),
            ("ReadFile", "uppercase"),
            ("read file", "does not match"),
            ("read.file", "does not match"),
            ("_read", "does not match"),
            ("read_", "does not match"),
            ("read--file", "does not match"),
            ("1read", "does not match"),
            ("-read", "does not match"),
            ("read-", "does not match"),
            ("read\u{4e2d}", "non-ASCII"),
        ] {
            let error = rejected(raw);
            assert!(
                error.reason.contains(expected_reason),
                "{raw:?}: {} did not mention {expected_reason:?}",
                error.reason
            );
        }
    }

    #[test]
    fn a_long_remote_name_becomes_a_stable_readable_segment() {
        let too_long = format!(
            "trigger-long-running-operation-{}",
            "x".repeat(SEGMENT_MAX_LENGTH)
        );
        assert!(!is_segment(&too_long));
        let shortened = normalize_segment(&too_long).expect("long valid names are shortened");
        assert!(is_segment(&shortened));
        assert_eq!(shortened.len(), SEGMENT_MAX_LENGTH);
        assert!(shortened.starts_with("trigger-long-ru"));
        assert_eq!(
            shorten_segment(&too_long),
            shortened,
            "the mapping must be deterministic across processes"
        );
        assert_ne!(
            shorten_segment(&too_long),
            shorten_segment(&format!("{too_long}-different")),
            "the digest must distinguish names sharing the same readable prefix"
        );
        // 边界本身是合法的：28 个字符的段仍然能进入 Provider 名称。
        assert!(is_segment(&"a".repeat(SEGMENT_MAX_LENGTH)));
    }

    #[test]
    fn the_pattern_we_print_is_the_pattern_we_enforce() {
        // 模式串只用于消息；这条测试是两者之间的桥：它按模式串手写了一张表，
        // 断言 `is_segment` 给出同样的答案。
        let accepted = ["a", "abc", "a1", "read-file", "a-b-c", "docker"];
        let refused = ["", "A", "1a", "-a", "a-", "a--b", "a_b", "a.b", "a b", "ä"];
        for value in accepted {
            assert!(is_segment(value), "{value} should match {SEGMENT_PATTERN}");
        }
        for value in refused {
            assert!(
                !is_segment(value),
                "{value} should not match {SEGMENT_PATTERN}"
            );
        }
    }

    #[test]
    fn the_internal_name_is_the_dotted_one_adr_0004_registers() {
        assert_eq!(
            internal_tool_name("mcp-1", "read-file").expect("legal pair"),
            "mcp.mcp-1.read-file"
        );
    }

    #[test]
    fn two_max_length_segments_still_produce_a_legal_internal_name() {
        // 段上限取一半的理由：Provider 名称是 `mcp__{server}__{tool}`，长度 = 7 + server + tool，
        // 上限 64。这条不等式在模块里以 `const _: () = assert!(...)` 写在编译期，这里只演示后果。
        let server = "a".repeat(SEGMENT_MAX_LENGTH);
        let tool = "b".repeat(SEGMENT_MAX_LENGTH);
        let name = internal_tool_name(&server, &tool).expect("28 + 28 is inside the budget");
        // `mcp` + `.` + 段 + `.` + 段
        assert_eq!(
            name.len(),
            MCP_NAMESPACE.len() + (1 + SEGMENT_MAX_LENGTH) * 2
        );
    }

    #[test]
    fn a_descriptor_keeps_the_remote_spelling_and_the_legal_segment() {
        let descriptor = McpToolDescriptor {
            name: normalize_segment("read_file").expect("translated"),
            remote_name: Some("read_file".to_string()),
            description: "reads a file".to_string(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        };
        assert!(descriptor.renamed());
        assert_eq!(
            descriptor.call_name(),
            "read_file",
            "the server only answers to the name it declared"
        );
        assert_eq!(
            descriptor.internal_name("srv-a").expect("legal"),
            "mcp.srv-a.read-file"
        );
    }

    #[test]
    fn exit_records_say_how_the_process_died() {
        let record = McpExitRecord {
            code: Some(7),
            signal: None,
            at: "2026-01-01T00:00:00Z".to_string(),
        };
        assert_eq!(record.reason(), "exit code 7");
        assert_eq!(
            McpExitRecord {
                code: None,
                signal: Some("9".to_string()),
                at: String::new(),
            }
            .reason(),
            "signal 9"
        );
        assert_eq!(
            McpExitRecord {
                code: None,
                signal: None,
                at: String::new(),
            }
            .reason(),
            "no exit status",
            "an unknown exit must not be reported as a success"
        );
    }
}
