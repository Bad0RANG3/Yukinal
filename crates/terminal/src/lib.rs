//! yukinal-terminal — PTY Manager。
//!
//! 数据流：
//! ```text
//! xterm.js → Tauri IPC → yukinal-terminal(PTY Manager) → SSH Channel → Remote PTY
//! ```
//!
//! 必须支持：resize / stdin / stdout streaming / reconnect / terminal lifecycle /
//! 多会话（每个 session 一个稳定 `terminal_session_id`）。
//!
//! 当前为契约占位。
