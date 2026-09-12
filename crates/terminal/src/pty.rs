//! 终端对 pty 的最小需求。`SshPty`（基于 `yukinal-ssh`）与测试用 `MemoryPty` 都实现它。

use yukinal_ssh::PtyEvent;

use crate::Result;

pub trait TerminalPty: Send + Sync {
    fn write(&self, data: &[u8]) -> impl std::future::Future<Output = Result<()>> + Send;
    fn resize(&self, cols: u16, rows: u16) -> impl std::future::Future<Output = Result<()>> + Send;
    /// 输出事件流（远端字节 / 关闭）。reopen 后新 pty 的新任务重新订阅。
    ///
    /// 接收端是**有界**的：远端产出快于消费时，生产端在 `send().await` 上挂起并把背压
    /// 传回远端，而不是在宿主进程里无限堆积。容量由实现方决定（`yukinal-ssh` 用
    /// `PTY_OUTPUT_CAPACITY`）。
    fn events(&self) -> tokio::sync::mpsc::Receiver<PtyEvent>;
    fn close(&self) -> impl std::future::Future<Output = Result<()>> + Send;
}
