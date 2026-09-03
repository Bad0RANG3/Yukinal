//! `Runner` 的两个实现：本机进程（开发/本地）与远端 ssh（真机）。

use std::sync::Arc;
use std::time::Duration;

use crate::{CollectorError, CommandOutput, Runner};
use yukinal_ssh::{Session, SshBackend};

/// 本机执行：`tokio::process::Command`，带超时（超时即杀进程）。
#[must_use]
pub fn local() -> Runner {
    Arc::new(|command: &str, timeout: Duration| {
        let command = command.to_string();
        Box::pin(async move {
            let child = tokio::process::Command::new("sh")
                .arg("-c")
                .arg(&command)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .map_err(|error| CollectorError::Runner(error.to_string()))?;

            // 超时时 wait_with_output 的 future 被 drop，kill_on_drop 兜底杀掉进程。
            let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
                Ok(output) => output.map_err(|error| CollectorError::Runner(error.to_string()))?,
                Err(_) => return Err(CollectorError::Timeout),
            };
            Ok(CommandOutput {
                exit_code: output.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            })
        })
    })
}

/// 远端执行：经 `SshBackend.execute`，同样带超时与取消语义。
pub fn ssh<B>(backend: Arc<B>, session: Session) -> Runner
where
    B: SshBackend + Send + Sync + 'static,
{
    Arc::new(move |command: &str, timeout: Duration| {
        let backend = Arc::clone(&backend);
        let session = session.clone();
        let command = command.to_string();
        Box::pin(async move {
            use tokio_util::sync::CancellationToken;
            let result = backend
                .execute(&session, &command, Some(timeout), &CancellationToken::new())
                .await
                .map_err(|error| CollectorError::Runner(error.to_string()))?;
            Ok(CommandOutput {
                exit_code: result.exit_code,
                stdout: result.stdout_lossy(),
                stderr: result.stderr_lossy(),
            })
        })
    })
}

/// 只在 Windows 之外可用的本机 runner：采集器解析的是 Linux 输出
/// （`/proc`、`df` 等），Windows 上跑它只会得到解析失败 —— 诚实报错。
pub fn local_linux_only() -> Runner {
    local()
}
