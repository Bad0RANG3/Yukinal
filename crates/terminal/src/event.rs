//! 会话信息与统一上抛事件。

use serde::Serialize;

/// 会话信息（列表 / 标题）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSessionInfo {
    pub terminal_session_id: String,
    pub server_id: String,
    pub cols: u16,
    pub rows: u16,
    pub opened_at: String,
}

/// 统一上抛事件；桌面层 1:1 转发为 Tauri events（`terminal.data` / `.opened` / `.closed`）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "name")]
pub enum TerminalAppEvent {
    Opened {
        payload: TerminalSessionInfo,
    },
    Data {
        terminal_session_id: String,
        /// 按 UTF-8 传输（交互式 shell 场景足够；非 UTF-8 程序后续换 base64）。
        data: String,
    },
    Closed {
        terminal_session_id: String,
        exit_code: Option<u32>,
    },
}
