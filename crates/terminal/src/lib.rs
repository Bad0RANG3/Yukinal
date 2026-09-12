//! yukinal-terminal — PTY Manager（数据流：xterm.js → Tauri IPC → 本 crate → SSH Channel → Remote PTY）。
//!
//! 职责：
//! - 每个会话一个稳定 `terminal_session_id`，多会话互不干扰；
//! - 所有会话的路由 / 事件统一成 [`TerminalAppEvent`]（桌面层只负责转发到 Tauri events）；
//! - 会话生命周期：open → write/resize 流式 → close；输出即来即推（broadcast，不积压）；
//! - reconnect：同一会话 id 下用新 pty 替换（`reopen`），xterm.js 缓冲区不丢。
//!
//! 本 crate 不接触凭据 / 服务器模型：pty 由上层（Rust core）经 [`TerminalPty`]
//! 注入。测试用内存 pty 覆盖全部路由逻辑，无需真机。
//!
//! 实现拆成四个私有子模块，公开路径由下面的 `pub use` 原样转出：
//! `error`（[`TerminalError`]）、`event`（[`TerminalSessionInfo`] /
//! [`TerminalAppEvent`]）、`pty`（[`TerminalPty`]）、`manager`
//! （[`TerminalManager`] 与每会话的转发任务）。

// 这里原来有一行 `#![allow(dead_code)]`（理由：「待 server 工具落地后全面使用」）。
// 去掉它之后整个 crate 只剩一处告警，而且那处在**测试**里（`MemoryPty` 的一个
// 冗余字段，已删）。也就是说这行 allow 多年来遮住的唯一东西是测试夹具里的一块
// 残骸，代价却是让这个 crate 的私有代码 —— 会话路由、一次性订阅语义这些最容易
// 出错的地方 —— 完全失去死代码检查。契约先行的 `pub` 接口本来就不需要 lint 豁免。

mod error;
mod event;
mod manager;
mod pty;

pub use error::TerminalError;
pub use event::{TerminalAppEvent, TerminalSessionInfo};
pub use manager::TerminalManager;
pub use pty::TerminalPty;

pub type Result<T> = std::result::Result<T, TerminalError>;

/// ISO-8601 UTC（显示用）。
///
/// 这里原本是 `iso8601_now` + `iso8601_utc` + `civil_from_days` 三个函数的整份副本，
/// 旁边还留着「与 core 侧同一套日历算法，保持所有时间戳格式一致」这句注释 —— 也就是
/// 把一致性寄托在纪律上。现已收进 `yukinal-time`：本 crate 依赖面刻意极窄，
/// core 又在上层，两者不可能互相依赖，所以共享实现只能放在共同的下层。直接再导出，
/// 调用点（`manager::TerminalManager::open` 里的 `opened_at`）不用改。
pub use yukinal_time::{iso8601_now, iso8601_utc};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso8601_matches_reference() {
        assert_eq!(iso8601_utc(1_700_000_000), "2023-11-14T22:13:20Z");
    }
}
