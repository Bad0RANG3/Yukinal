//! 终端错误：会话路由上的三类失败。
//!
//! 分开是因为调用方的处置完全不同：「这个会话不存在」（多半是 UI 与宿主不同步）、
//! 「已经存在」（重复 open）、以及「pty 自己做失败了」。

#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    #[error("terminal session `{0}` not found")]
    NotFound(String),
    #[error("terminal session `{0}` already exists")]
    Exists(String),
    #[error("terminal operation failed: {0}")]
    Channel(String),
}
