//! 终端对 PTY transport 的最小需求。具体连接由上层适配器提供，测试用 `MemoryPty` 用于验证会话管理。

use crate::Result;

/// Remote PTY output and exit events, independent of the transport that owns the PTY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalPtyEvent {
    Output(Vec<u8>),
    Closed { code: Option<u32> },
}

pub trait TerminalPty: Send + Sync {
    fn write(&self, data: &[u8]) -> impl std::future::Future<Output = Result<()>> + Send;
    fn resize(&self, cols: u16, rows: u16) -> impl std::future::Future<Output = Result<()>> + Send;
    /// 输出事件流（远端字节 / 关闭）。reopen 后新 pty 的新任务重新订阅。
    ///
    /// 接收端是**有界**的：远端产出快于消费时，生产端在 `send().await` 上挂起并把背压
    /// 传回远端，而不是在宿主进程里无限堆积。容量由实现方决定。
    fn events(&self) -> tokio::sync::mpsc::Receiver<TerminalPtyEvent>;
    fn close(&self) -> impl std::future::Future<Output = Result<()>> + Send;
}
