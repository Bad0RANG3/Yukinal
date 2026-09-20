//! `Runner` 的两个实现：本机进程（开发/本地）与远端 ssh（真机）。

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

use crate::{CollectorError, CommandOutput, Runner};
use yukinal_ssh::{Session, SshBackend};

const MAX_CAPTURE_BYTES: usize = 1024 * 1024;
const IO_DRAIN_GRACE: Duration = Duration::from_secs(1);
const REAP_GRACE: Duration = Duration::from_secs(1);

/// 本机执行：`tokio::process::Command`，带超时（超时即杀进程）。
#[must_use]
pub fn local() -> Runner {
    Arc::new(|command: &str, timeout: Duration| {
        let command = command.to_string();
        Box::pin(async move {
            let mut command_builder = Command::new("sh");
            command_builder
                .arg("-c")
                .arg(&command)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            configure_process_group(&mut command_builder);

            let mut child = command_builder
                .spawn()
                .map_err(|error| CollectorError::Runner(error.to_string()))?;
            let pid = child.id();
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| CollectorError::Runner("stdout was not piped".to_string()))?;
            let stderr = child
                .stderr
                .take()
                .ok_or_else(|| CollectorError::Runner("stderr was not piped".to_string()))?;
            let stdout_task = tokio::spawn(read_capped(stdout));
            let stderr_task = tokio::spawn(read_capped(stderr));

            match tokio::time::timeout(timeout, child.wait()).await {
                Ok(Ok(status)) => {
                    let (stdout, stderr) =
                        collect_pipe_output(stdout_task, stderr_task, pid).await?;
                    Ok(CommandOutput {
                        exit_code: status.code().unwrap_or(-1),
                        stdout: String::from_utf8_lossy(&stdout).into_owned(),
                        stderr: String::from_utf8_lossy(&stderr).into_owned(),
                    })
                }
                Ok(Err(error)) => {
                    kill_process_group(pid);
                    let _ = tokio::time::timeout(REAP_GRACE, child.wait()).await;
                    let _ = collect_pipe_output(stdout_task, stderr_task, pid).await;
                    Err(CollectorError::Runner(error.to_string()))
                }
                Err(_) => {
                    kill_process_tree(&mut child, pid);
                    let _ = tokio::time::timeout(REAP_GRACE, child.wait()).await;
                    let _ = collect_pipe_output(stdout_task, stderr_task, pid).await;
                    Err(CollectorError::Timeout)
                }
            }
        })
    })
}

async fn read_capped<R>(mut reader: R) -> std::io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut captured = Vec::with_capacity(8192);
    let mut chunk = [0_u8; 8192];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            return Ok(captured);
        }
        let remaining = MAX_CAPTURE_BYTES.saturating_sub(captured.len());
        if remaining > 0 {
            captured.extend_from_slice(&chunk[..read.min(remaining)]);
        }
        // Keep draining after the cap so a chatty child cannot block on a full pipe.
    }
}

async fn collect_pipe_output(
    stdout_task: tokio::task::JoinHandle<std::io::Result<Vec<u8>>>,
    stderr_task: tokio::task::JoinHandle<std::io::Result<Vec<u8>>>,
    pid: Option<u32>,
) -> Result<(Vec<u8>, Vec<u8>), CollectorError> {
    match tokio::time::timeout(IO_DRAIN_GRACE, async {
        tokio::join!(stdout_task, stderr_task)
    })
    .await
    {
        Ok((Ok(Ok(stdout)), Ok(Ok(stderr)))) => Ok((stdout, stderr)),
        Ok((stdout, stderr)) => {
            let _ = stdout;
            let _ = stderr;
            Err(CollectorError::Runner(
                "failed while reading command output".to_string(),
            ))
        }
        Err(_) => {
            kill_process_group(pid);
            Err(CollectorError::Runner(
                "command pipes did not close after the process exited".to_string(),
            ))
        }
    }
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

fn kill_process_tree(child: &mut Child, pid: Option<u32>) {
    kill_process_group(pid);
    let _ = child.start_kill();
}

#[cfg(unix)]
fn kill_process_group(pid: Option<u32>) {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;

    if let Some(group) = pid
        .and_then(|pid| i32::try_from(pid).ok())
        .and_then(i32::checked_neg)
    {
        let _ = kill(Pid::from_raw(group), Signal::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group(_pid: Option<u32>) {}

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
                .map_err(|error| match error {
                    yukinal_ssh::Error::Timeout => CollectorError::Timeout,
                    other => CollectorError::Runner(other.to_string()),
                })?;
            Ok(CommandOutput {
                exit_code: result.exit_code,
                stdout: result.stdout_lossy(),
                stderr: result.stderr_lossy(),
            })
        })
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_TEMP: AtomicUsize = AtomicUsize::new(0);

    fn temp_path(tag: &str) -> PathBuf {
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "yukinal-collector-{tag}-{}-{sequence}",
            std::process::id()
        ))
    }

    fn pid_is_alive(pid: i32) -> bool {
        use nix::sys::signal::kill;
        use nix::unistd::Pid;

        kill(Pid::from_raw(pid), None).is_ok()
    }

    async fn wait_until_gone(pid: i32) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            if !pid_is_alive(pid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("descendant process {pid} survived the runner timeout");
    }

    #[tokio::test]
    async fn local_timeout_kills_the_whole_process_group() {
        let marker = temp_path("descendant-pid");
        let command = format!("sleep 30 & echo $! > '{}'; wait", marker.display());
        let runner = local();
        let result = runner(&command, Duration::from_millis(100)).await;
        assert!(matches!(result, Err(CollectorError::Timeout)));

        let descendant = std::fs::read_to_string(&marker)
            .expect("timeout command must write its descendant pid")
            .trim()
            .parse::<i32>()
            .expect("descendant pid must be numeric");
        wait_until_gone(descendant).await;
        std::fs::remove_file(marker).ok();
    }

    #[tokio::test]
    async fn local_output_is_bounded_without_blocking_the_child() {
        let runner = local();
        let output = runner("yes x | head -c 2097152", Duration::from_secs(5))
            .await
            .expect("large output must be drained");
        assert_eq!(output.exit_code, 0);
        assert_eq!(output.stdout.len(), MAX_CAPTURE_BYTES);
    }

    #[tokio::test]
    async fn local_preserves_exit_code_and_both_streams() {
        let runner = local();
        let output = runner(
            "printf stdout; printf stderr >&2; exit 7",
            Duration::from_secs(5),
        )
        .await
        .expect("ordinary command must complete");
        assert_eq!(output.exit_code, 7);
        assert_eq!(output.stdout, "stdout");
        assert_eq!(output.stderr, "stderr");
    }
}
