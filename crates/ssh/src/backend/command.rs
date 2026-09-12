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
        run_command(
            session.inner.conn.lock().await.clone(),
            command,
            timeout,
            cancel,
            MAX_COMMAND_OUTPUT_BYTES,
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
        let mut exit_code = None;
        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::Data { data } => append_bounded(&mut stdout, &data, max_output_bytes),
                ChannelMsg::ExtendedData { data, ext: 1 } => {
                    append_bounded(&mut stderr, &data, max_output_bytes)
                }
                ChannelMsg::ExitStatus { exit_status } => exit_code = Some(exit_status as i32),
                ChannelMsg::Close | ChannelMsg::Eof => break,
                _ => {}
            }
        }
        Ok(CommandResult {
            exit_code: exit_code.unwrap_or(-1),
            stdout,
            stderr,
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

fn append_bounded(output: &mut Vec<u8>, chunk: &[u8], max_bytes: usize) {
    let remaining = max_bytes.saturating_sub(output.len());
    output.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
}
