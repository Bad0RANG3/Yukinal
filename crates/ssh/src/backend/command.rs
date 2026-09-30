//! 一次性命令执行：有界输出、超时与取消。

use std::sync::Arc;

use russh::client::Handle;
use russh::ChannelMsg;

use super::error::map_send_err;
use super::hostkey::ConnHandler;
use super::{retry_transport_async, RusshBackend};
use crate::{CommandResult, Error, Result, Session};

/// Remote commands must not be able to grow an unbounded Rust `Vec` from a
/// hostile or unexpectedly noisy process. The host layer applies tighter
/// semantic limits when it parses a result; this is the transport-level cap.
const MAX_COMMAND_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

impl RusshBackend {
    /// Execute a command exactly once. Read-only commands use the trait method,
    /// which may reconnect and retry a transport failure; callers with side
    /// effects must use this method so a lost response cannot repeat the action.
    pub async fn execute_once(
        &self,
        session: &Session,
        command: &str,
        timeout: Option<std::time::Duration>,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<CommandResult> {
        self.execute_bounded_once(session, command, timeout, MAX_COMMAND_OUTPUT_BYTES, cancel)
            .await
    }

    /// Execute once with a caller-selected combined stdout/stderr output budget.
    ///
    /// The budget is enforced while SSH channel frames arrive, before bytes are retained in
    /// memory. An effectful command must use this no-retry entry point: a lost reply cannot
    /// safely replay a command whose remote effect may already have happened.
    pub async fn execute_bounded_once(
        &self,
        session: &Session,
        command: &str,
        timeout: Option<std::time::Duration>,
        max_output_bytes: usize,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<CommandResult> {
        if session.inner.is_closed() {
            return Err(Error::Channel("session is closed".into()));
        }
        run_command(
            session.inner.connection().await?,
            command,
            timeout,
            cancel,
            max_output_bytes.min(MAX_COMMAND_OUTPUT_BYTES),
        )
        .await
    }
}

/// trait 入口 [`SshBackend::execute`] 的实现体：包一层「transport 断开 → 重连 → 重试」。
pub(super) async fn execute(
    session: &Session,
    command: &str,
    timeout: Option<std::time::Duration>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<CommandResult> {
    retry_transport_async(
        session,
        |conn| run_command(conn, command, timeout, cancel, MAX_COMMAND_OUTPUT_BYTES),
        Some(cancel),
    )
    .await
}

async fn run_command(
    conn: Arc<Handle<ConnHandler>>,
    command: &str,
    timeout: Option<std::time::Duration>,
    cancel: &tokio_util::sync::CancellationToken,
    max_output_bytes: usize,
) -> Result<CommandResult> {
    let mut channel = conn.channel_open_session().await.map_err(map_send_err)?;
    channel.exec(true, command).await.map_err(map_send_err)?;

    let body = async {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut output_remaining = max_output_bytes;
        let mut stdout_truncated = false;
        let mut stderr_truncated = false;
        let mut exit_code = None;
        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::Data { data } => append_bounded(
                    &mut stdout,
                    &data,
                    &mut output_remaining,
                    &mut stdout_truncated,
                ),
                ChannelMsg::ExtendedData { data, ext: 1 } => append_bounded(
                    &mut stderr,
                    &data,
                    &mut output_remaining,
                    &mut stderr_truncated,
                ),
                ChannelMsg::ExitStatus { exit_status } => exit_code = Some(exit_status as i32),
                ChannelMsg::Close | ChannelMsg::Eof => break,
                _ => {}
            }
        }
        Ok(CommandResult {
            exit_code: exit_code.unwrap_or(-1),
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
        })
    };

    match timeout {
        Some(limit) => tokio::select! {
            _ = cancel.cancelled() => Err(Error::Cancelled),
            result = tokio::time::timeout(limit, body) => result.map_err(|_| Error::Timeout)?,
        },
        None => tokio::select! {
            _ = cancel.cancelled() => Err(Error::Cancelled),
            result = body => result,
        },
    }
}

fn append_bounded(output: &mut Vec<u8>, chunk: &[u8], remaining: &mut usize, truncated: &mut bool) {
    let take = chunk.len().min(*remaining);
    output.extend_from_slice(&chunk[..take]);
    *remaining -= take;
    if take < chunk.len() {
        *truncated = true;
    }
}

#[cfg(test)]
mod tests {
    use super::append_bounded;

    #[test]
    fn combined_stdout_and_stderr_share_the_requested_budget_and_report_truncation() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut remaining = 5;
        let mut stdout_truncated = false;
        let mut stderr_truncated = false;

        append_bounded(&mut stdout, b"abc", &mut remaining, &mut stdout_truncated);
        append_bounded(&mut stderr, b"def", &mut remaining, &mut stderr_truncated);

        assert_eq!(stdout, b"abc");
        assert_eq!(stderr, b"de");
        assert_eq!(remaining, 0);
        assert!(!stdout_truncated);
        assert!(stderr_truncated);
    }
}
