//! SFTP 子系统：惰性建立的句柄、目录清单与有界读写。
//!
//! 这里只有自由函数与 [`RusshBackend`] 的固有方法；[`crate::SshBackend`] 的
//! 唯一一份实现留在 `backend::mod`，它把 `sftp` 转发到下面的 [`sftp`]。

use std::sync::Arc;

use super::error::map_send_err;
use super::{retry_transport_async, RusshBackend};
use crate::conn::SftpHandle;
use crate::{Error, Result, Session, SftpClient};

impl RusshBackend {
    /// SFTP 冒烟操作（文件工具落地前证明子系统真实可用）：远端目录清单。
    pub async fn sftp_list_dir(&self, client: &SftpClient, path: &str) -> Result<Vec<String>> {
        let sftp = lock_sftp(client).await?;
        let dir = sftp
            .read_dir(path)
            .await
            .map_err(|error| Error::Channel(error.to_string()))?;
        Ok(dir.map(|entry| entry.file_name()).collect())
    }

    pub async fn sftp_list_dir_detailed(
        &self,
        client: &SftpClient,
        path: &str,
    ) -> Result<Vec<(String, String, u64)>> {
        let sftp = lock_sftp(client).await?;
        let dir = sftp
            .read_dir(path)
            .await
            .map_err(|error| Error::Channel(error.to_string()))?;
        Ok(dir
            .map(|entry| {
                let file_type = match entry.file_type() {
                    russh_sftp::protocol::FileType::Dir => "directory",
                    russh_sftp::protocol::FileType::File => "file",
                    russh_sftp::protocol::FileType::Symlink => "symlink",
                    russh_sftp::protocol::FileType::Other => "other",
                };
                (
                    entry.file_name(),
                    file_type.to_string(),
                    entry.metadata().len(),
                )
            })
            .collect())
    }

    /// SFTP 读整文件（filesystem read 工具的基础操作）。
    pub async fn sftp_read_file(&self, client: &SftpClient, path: &str) -> Result<Vec<u8>> {
        self.sftp_read_file_bounded(client, path, usize::MAX).await
    }

    /// SFTP 读取至多 `max_bytes + 1` 字节，以便调用方可靠判断是否截断。
    pub async fn sftp_read_file_bounded(
        &self,
        client: &SftpClient,
        path: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>> {
        use russh_sftp::protocol::OpenFlags;
        use tokio::io::AsyncReadExt;
        let sftp = lock_sftp(client).await?;
        let file = sftp
            .open_with_flags(path, OpenFlags::READ)
            .await
            .map_err(|error| Error::Channel(error.to_string()))?;
        let mut out = Vec::new();
        file.take(max_bytes.saturating_add(1) as u64)
            .read_to_end(&mut out)
            .await
            .map_err(|error| Error::Channel(error.to_string()))?;
        Ok(out)
    }

    /// SFTP 覆盖写入一个文件；close 会等待远端确认所有 pending writes。
    pub async fn sftp_write_file(
        &self,
        client: &SftpClient,
        path: &str,
        data: &[u8],
    ) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        let sftp = lock_sftp(client).await?;
        let mut file = sftp
            .create(path)
            .await
            .map_err(|error| Error::Channel(error.to_string()))?;
        file.write_all(data)
            .await
            .map_err(|error| Error::Channel(error.to_string()))?;
        file.close()
            .await
            .map_err(|error| Error::Channel(error.to_string()))?;
        Ok(())
    }
}

/// [`crate::SshBackend::sftp`] 的实现体。
pub(super) async fn sftp(session: &Session) -> Result<SftpClient> {
    let sftp = retry_transport_async(
        session,
        |conn| async move {
            let channel = conn.channel_open_session().await.map_err(map_send_err)?;
            channel
                .request_subsystem(true, "sftp")
                .await
                .map_err(map_send_err)?;
            let stream = channel.into_stream();
            russh_sftp::client::SftpSession::new(stream)
                .await
                .map_err(|error| Error::Channel(format!("sftp handshake failed: {error}")))
        },
        None,
    )
    .await?;
    Ok(SftpClient {
        session_id: session.session_id.clone(),
        server_id: session.server_id.clone(),
        inner: Arc::new(SftpHandle::new_some(Arc::new(sftp))),
    })
}

async fn lock_sftp(client: &SftpClient) -> Result<Arc<russh_sftp::client::SftpSession>> {
    client
        .inner
        .sftp
        .lock()
        .await
        .clone()
        .ok_or_else(|| Error::Channel("sftp session is not established".into()))
}

impl SftpHandle {
    fn new_some(session: Arc<russh_sftp::client::SftpSession>) -> Self {
        Self {
            sftp: tokio::sync::Mutex::new(Some(session)),
        }
    }
}
