//! MCP（stdio）客户端：Rust 宿主派生并监督 MCP 服务进程
//! （README.md 的「进程生命周期仍然归 Rust」，ADR 0001/0008）。
//!
//! 边界不是从某份「设计意图」文档抄来的，而是这份部署形状自己推出来的：一个 MCP 服务就是
//! 一个**子进程**，而本架构里派生进程的地方只有 Rust 宿主（sidecar 也走同一条路，见
//! [`crate::supervisor`]）。它的 stdin/stdout 是本机的一对管道、生命周期挂在宿主身上、崩溃
//! 必须能被解释 —— 这三件事合起来只留下一个答案：谁派生，谁监督，谁就能在它死的时候说清
//! 它是怎么死的。仓库根的 README 的「边界：外部工具（MCP）」一节记的就是这段推理的落地结论。
//!
//! # 它是什么
//!
//! - [`McpStdioConfig`]：**启动什么**（[`config`]），以及 `http` 被拒绝的那条路。
//! - [`descriptor`]：名字规则与本地类型 —— 远端拼写必须先变成合法段，才准进注册表。
//! - [`wire`]：线协议（一行一帧的 JSON-RPC 2.0）、版本协商、三个方法的形状。
//! - `handle`：[`McpServerHandle`] —— **一个**服务进程：握手、`tools/list`、`tools/call`、
//!   每次请求的超时、退出记录、有界的 stderr 尾部。
//! - `supervisor`：[`McpSupervisor`] —— 按 serverId 记账：一个 serverId 一个进程、启动串行化、
//!   状态与关闭。
//! - `error`：[`McpError`] —— 每一个变体对应一件调用方能**分别处理**的事。
//!
//! # 生命周期契约（README.md 的「进程生命周期仍然归 Rust」要求它被写下来，而不是被假定）
//!
//! - **单实例**：同一个 serverId 同时只有一个进程。已经跑着时 `start` 复用它，不派生第二个。
//! - **退出语义**：正常结束只有两条路 —— 调用方 [`McpSupervisor::shutdown`]，或者句柄被丢弃
//!   （`kill_on_drop`）。关闭时先关 stdin（MCP stdio 的体面退出就是 EOF），等一个有界的
//!   宽限期；服务端不理会，就升级到强杀，再等一个有界的宽限期，然后如实上报「没能确认它
//!   消失」，而不是无限等下去。孤儿进程不可接受，卡死也不可接受，这两件事都要有预算。
//! - **崩了不自愈**：进程死了就是死了。工具随之不可用，失败如实上报（退出码/信号留在
//!   [`McpServerStatus::last_exit`]），并且**不会自动重启**。sidecar 有重启预算
//!   （[`crate::supervisor`]），MCP 服务没有：一个第三方进程崩溃后，宿主替它重新拉起来
//!   等于把一个未知状态的进程放回工具表里；要不要再来一次是用户/上层显式按下的动作。
//! - **超时**：`initialize` / `tools/list` / `tools/call` 每一次调用都带超时。超时是一个
//!   **独立于「进程死了」的错误**（[`McpError::Timeout`] vs [`McpError::Exited`]）：
//!   前者可以换服务器或重试，后者不能。
//!
//! # 它刻意不做什么
//!
//! - **不做 `http` 传输**（README.md 的「进程生命周期仍然归 Rust」）：出站网络策略还不存在，
//!   所以 [`McpStdioConfig`] 根本无法表示 http —— 拒绝发生在 `from_server_config`，
//!   类型本身就是那道闸门。
//! - **不注册工具**：把 MCP 工具变成 `ToolDeclaration`、定风险等级、跑 Provider 名称冲突
//!   检查，都是适配器与 ToolRegistry 的事；这里只交出经校验的描述符
//!   （README.md 的「外部工具必须先变成 Yukinal 的工具声明」「命名空间与名称冲突」
//!   「风险等级由本地决定」）。
//! - **不校验入参**（README.md 的「输入、输出与超时由本地强制」）：远端 JSON Schema 只作为
//!   翻译来源，本地 Zod schema 才决定一次调用接受什么。本模块里没有任何一处拿远端 schema
//!   当校验依据。
//! - **不解析目标**（README.md 的「目标必须在本地解析」）：`ToolTarget` 由调用侧给出，
//!   本模块不去猜服务器。
//! - **不产生事件、不写审计、不加 IPC 命令**：宿主协议的类型与 Tauri 命令属于接线那一步，
//!   不在这里。本模块因此**没有**让 `capabilities.mcp` 变成
//!   `true`：能力报告必须是事实，而适配器还不存在（README.md「边界：外部工具（MCP）」的
//!   「不要暗示已经可用」）。
//! - **不回答服务端的请求**：`initialize` 里我们如实声明零客户端能力，合规的服务端就不该
//!   发请求过来；真发了只记一条诊断。替它编一个答案正是 README.md 的「不要暗示已经可用」
//!   禁止的「假装能用」。
//! - **不处理 `notifications/tools/list_changed`**：工具表变化意味着重新注册，属于适配器
//!   （README.md 的「外部工具必须先变成 Yukinal 的工具声明」）；本模块只把它记进诊断尾部，
//!   缓存不刷新。
//! - **不假装能自动重启**：崩掉的服务端就是崩了，退出记录与诊断尾部说明怎么死的；重启
//!   一次可能把上一次的副作用再执行一遍。恢复由用户决定。
//! - **脱敏之后才能出门**：stderr 尾部、诊断尾部与远端错误文本都是给排障用的子进程输出，
//!   但它们来自一个我们并不信任的进程，所以一律先截断、再经 [`crate::redact`] 过滤
//!   （与 sidecar 转发日志用的是同一份实现）。工具**描述**是例外：那是要交给模型的输入，
//!   不是日志，脱敏会篡改服务端写下的文档。

mod catalog;
mod config;
mod descriptor;
mod error;
mod handle;
mod supervisor;
mod wire;

pub use catalog::{
    catalog, describe_dead, is_mcp_tool_name, split_mcp_tool_name, McpCatalogFailure,
    McpCatalogResponse, McpCatalogServer, McpCatalogTool, McpFailureCode, CATALOG_START_BUDGET,
};
pub use config::{
    McpStdioConfig, DEFAULT_REQUEST_TIMEOUT, MAX_REQUEST_TIMEOUT, MIN_REQUEST_TIMEOUT,
};
pub use descriptor::{
    internal_tool_name, is_segment, McpContentBlock, McpExitRecord, McpToolDescriptor,
    McpToolResult, NameRejection, MCP_NAMESPACE, PROVIDER_TOOL_NAME_MAX_LENGTH, SEGMENT_MAX_LENGTH,
    SEGMENT_PATTERN, TOOL_DESCRIPTION_MAX_CHARS,
};
pub use error::McpError;
pub use handle::{McpServerHandle, McpServerInfo, McpServerStart, McpServerStatus, ShutdownReport};
pub use supervisor::McpSupervisor;
pub use wire::{McpInitialize, PREFERRED_PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS};

/// 一行不可信文本可以有多长才被记为诊断。
///
/// 远端文本的长度不受我们控制：错误消息里的工具名、stdout 上的噪声、stderr 的一行都会经过
/// 这里。数值**就是** [`crate::redact::TRUNCATE_MAX_CHARS`]，不再各留一份常数 —— 两份常数
/// 迟早漂移，而这一对的意义正是「所有不可信文本按同一把尺子截断」。
pub(super) const TEXT_MAX_CHARS: usize = crate::redact::TRUNCATE_MAX_CHARS;

/// stderr 保留多少行。
///
/// sidecar 的日志尾部留 200 行，因为崩掉的 agent 的堆栈就是你要找的东西。MCP 服务的 stderr
/// 通常只有启动那几行，而「怎么死的」由退出记录回答，不靠堆栈：100 行足够装下一次启动横幅
/// 加一段 traceback，又小到可以随状态一起发给界面。
pub const STDERR_TAIL_LINES: usize = 100;

/// 一行不可信文本 → 可以安全放进状态与错误消息里的东西。
pub(super) fn truncated_to(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

pub(super) fn truncated(text: &str) -> String {
    truncated_to(text, TEXT_MAX_CHARS)
}

/// 截断之后再脱敏：任何**要交给用户看**的不可信文本都必须经过它。
///
/// 顺序是「先截断，再脱敏」：反过来的话，一段在 200 字符处被切断的文本会让边界上的密钥
/// 只剩前半截，脱敏规则就匹配不到了。这里与 `sidecar` 转发日志时用的是同一个函数、
/// 同一套规则。
pub(super) fn redacted(text: &str) -> String {
    crate::redact::redact_log_line(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_frame_is_shortened_for_display_and_marked_as_shortened() {
        let long = "x".repeat(TEXT_MAX_CHARS + 50);
        let short = truncated(&long);
        assert_eq!(short.chars().count(), TEXT_MAX_CHARS + 1);
        assert!(short.ends_with('…'), "a truncated line must say so");
        assert_eq!(truncated("kept"), "kept");
        assert_eq!(
            truncated_to("abcdef", 3),
            "abc…",
            "the cap counts characters, so a multi-byte line cannot split a character"
        );
        assert_eq!(truncated_to("汉字汉字", 2), "汉字…");
    }
}
