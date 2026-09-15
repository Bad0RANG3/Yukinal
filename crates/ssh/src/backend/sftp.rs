//! SFTP 子系统：惰性建立的句柄、目录清单与有界读写。
//!
//! 这里只有自由函数与 [`RusshBackend`] 的固有方法；[`crate::SshBackend`] 的
//! 唯一一份实现留在 `backend::mod`，它把 `sftp` 转发到下面的 [`sftp`]。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use super::error::map_send_err;
use super::{retry_transport_async, RusshBackend};
use crate::conn::SftpHandle;
use crate::{Error, Result, Session, SftpClient};

/// SFTP 属性里能读到的、编辑需要知道的那几件事。
///
/// **没有链接数**：SFTP v3 的 `SSH_FXP_ATTRS` 里没有这一项，v4/v5 也没有补上，所以硬链接
/// 只能靠远端 `stat` 探针去问（见 [`link_count_probe_command`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SftpFileStat {
    pub kind: SftpEntryKind,
    pub size: u64,
    /// 远端给出的修改时间（Unix 秒）。SFTP 的粒度是**秒**，这也决定了守卫的粒度。
    pub modified: Option<u32>,
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SftpEntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

/// 替换前记录下来的元数据守卫：`size` 与 `mtime`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SftpReplaceGuard {
    pub size: u64,
    pub modified: Option<u32>,
}

/// 发布之后的实测属性。调用方要拿它复核「发布出来的就是我们写进去的那一份」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SftpReplacement {
    pub size: u64,
    pub modified: Option<u32>,
}

/// 一次有守卫替换的失败：**调用方必须能区分「有人改了」与「远端做不到」**，
/// 因为前者该重读重试、后者重试多少次都一样。
#[derive(Debug)]
pub enum SftpReplaceError {
    /// 目标在读取之后被改过，或发布之后发现不是我们写进去的内容。
    ConcurrentChange(String),
    /// 远端不具备安全替换的能力（symlink、rename 被拒、staging 建不出来）。
    Unsupported(String),
    /// metadata 保不住；`missing` 逐项点名。
    MetadataNotPreserved {
        message: String,
        missing: Vec<String>,
    },
    /// 传输层失败。
    Transport(Error),
}

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

    /// 读取一个路径的属性（不跟随 symlink：`lstat` 语义）。
    pub async fn sftp_stat(&self, client: &SftpClient, path: &str) -> Result<SftpFileStat> {
        let sftp = lock_sftp(client).await?;
        let metadata = sftp
            .symlink_metadata(path)
            .await
            .map_err(|error| Error::Channel(error.to_string()))?;
        Ok(stat_from_attributes(&metadata))
    }

    /// 有守卫的原子替换（ADR 0017）：同目录 staging → 属性复核 → 守卫复核 → rename → 发布复核。
    ///
    /// **没有退回原位写的分支。** 做不到就返回 [`SftpReplaceError::Unsupported`]，因为「换成一种
    /// 更弱的写法继续」会让「原子」这句话在失败路径上变成假话，也把一次可能被覆盖的改写从可检测
    /// 变成不可检测。
    ///
    /// 守卫是 `size` + `mtime`（秒）：窗口没有被消除，只是被缩到 `stat → rename` 之间，并且窗口
    /// 里的改动会变成一条错误。同一秒内、同样大小的改写仍不可检测——这一点写在 ADR 与文档里。
    pub async fn sftp_replace_file_guarded(
        &self,
        client: &SftpClient,
        path: &str,
        guard: SftpReplaceGuard,
        data: &[u8],
    ) -> std::result::Result<SftpReplacement, SftpReplaceError> {
        use russh_sftp::protocol::{FileAttributes, OpenFlags};
        use tokio::io::AsyncWriteExt;

        static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

        let sftp = lock_sftp(client)
            .await
            .map_err(SftpReplaceError::Transport)?;
        let Some(temp_path) = atomic_staging_path(
            path,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                ^ u128::from(TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)),
        ) else {
            return Err(SftpReplaceError::Unsupported(format!(
                "{path} is not an absolute file path, so a staging file cannot be placed next to it"
            )));
        };

        let metadata = match sftp.symlink_metadata(path).await {
            Ok(metadata) => metadata,
            Err(error) => {
                return Err(SftpReplaceError::Transport(Error::Channel(
                    error.to_string(),
                )))
            }
        };
        let target = stat_from_attributes(&metadata);
        if target.kind != SftpEntryKind::File {
            return Err(SftpReplaceError::Unsupported(
                "only a regular file can be replaced atomically; this path is a symlink, a \
                 directory or something else"
                    .to_string(),
            ));
        }
        if target.size != guard.size || target.modified != guard.modified {
            return Err(SftpReplaceError::ConcurrentChange(format!(
                "the file changed between the read and the replacement (size {} → {}, mtime {:?} → {:?})",
                guard.size, target.size, guard.modified, target.modified
            )));
        }
        let attributes = FileAttributes {
            uid: metadata.uid,
            gid: metadata.gid,
            permissions: metadata.permissions,
            // mtime 一起带上：编辑不该顺手把文件的时间戳改成「刚刚」。
            mtime: metadata.mtime,
            ..FileAttributes::default()
        };

        let mut file = match sftp
            .open_with_flags_and_attributes(
                &temp_path,
                OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE,
                attributes.clone(),
            )
            .await
        {
            Ok(file) => file,
            Err(error) => {
                return Err(SftpReplaceError::Unsupported(format!(
                    "could not create a staging file next to {path}: {error}"
                )))
            }
        };

        let staged = async {
            // fsetstat 优先；服务器若不支持句柄上的 setstat，再试路径上的 setstat。两条都失败
            // 才算「保不住属性」，而不是直接放弃。
            if file.set_metadata(attributes.clone()).await.is_err() {
                sftp.set_metadata(&temp_path, attributes.clone())
                    .await
                    .map_err(|error| Error::Channel(error.to_string()))?;
            }
            file.write_all(data)
                .await
                .map_err(|error| Error::Channel(error.to_string()))?;
            file.sync_all()
                .await
                .map_err(|error| Error::Channel(error.to_string()))?;
            file.close()
                .await
                .map_err(|error| Error::Channel(error.to_string()))?;
            Ok::<(), Error>(())
        }
        .await;

        if let Err(error) = staged {
            let _ = sftp.remove_file(&temp_path).await;
            return Err(SftpReplaceError::Transport(error));
        }

        // 属性复核：staging 上的 mode/mtime/owner/group 必须与目标一致，否则这次编辑会把
        // metadata 悄悄改掉 —— 逐项点名，而不是含糊地说失败。
        let staged_stat = match sftp.symlink_metadata(&temp_path).await {
            Ok(metadata) => stat_from_attributes(&metadata),
            Err(error) => {
                let _ = sftp.remove_file(&temp_path).await;
                return Err(SftpReplaceError::Transport(Error::Channel(
                    error.to_string(),
                )));
            }
        };
        let missing = unpreserved_attributes(&target, &staged_stat);
        if !missing.is_empty() {
            let _ = sftp.remove_file(&temp_path).await;
            return Err(SftpReplaceError::MetadataNotPreserved {
                message: format!(
                    "the remote would not keep the file's {}; the edit was not published",
                    missing.join(", ")
                ),
                missing,
            });
        }

        // 守卫复核：staging 已经写好，rename 之前的最后一次确认。
        match sftp.symlink_metadata(path).await {
            Ok(metadata) => {
                let now = stat_from_attributes(&metadata);
                if now.size != guard.size || now.modified != guard.modified {
                    let _ = sftp.remove_file(&temp_path).await;
                    return Err(SftpReplaceError::ConcurrentChange(format!(
                        "the file changed while the edit was being staged (size {} → {}, mtime {:?} → {:?})",
                        guard.size, now.size, guard.modified, now.modified
                    )));
                }
            }
            Err(error) => {
                let _ = sftp.remove_file(&temp_path).await;
                return Err(SftpReplaceError::Transport(Error::Channel(
                    error.to_string(),
                )));
            }
        }

        if let Err(error) = sftp.rename(&temp_path, path).await {
            let _ = sftp.remove_file(&temp_path).await;
            return Err(SftpReplaceError::Unsupported(format!(
                "the server refused to replace {path} through a rename ({error}); this server \
                 cannot publish an edit atomically, so nothing was written"
            )));
        }

        // 发布复核：目标现在必须就是我们写进去的那一份。
        let published = match sftp.symlink_metadata(path).await {
            Ok(metadata) => stat_from_attributes(&metadata),
            Err(error) => {
                return Err(SftpReplaceError::Transport(Error::Channel(
                    error.to_string(),
                )))
            }
        };
        Ok(SftpReplacement {
            size: published.size,
            modified: published.modified,
        })
    }
}

fn stat_from_attributes(metadata: &russh_sftp::protocol::FileAttributes) -> SftpFileStat {
    let kind = match metadata.file_type() {
        russh_sftp::protocol::FileType::File => SftpEntryKind::File,
        russh_sftp::protocol::FileType::Dir => SftpEntryKind::Directory,
        russh_sftp::protocol::FileType::Symlink => SftpEntryKind::Symlink,
        russh_sftp::protocol::FileType::Other => SftpEntryKind::Other,
    };
    SftpFileStat {
        kind,
        size: metadata.size.unwrap_or(0),
        modified: metadata.mtime,
        mode: metadata.permissions,
        uid: metadata.uid,
        gid: metadata.gid,
    }
}

/// 哪些 metadata 没能带上。**逐项点名**：说「失败」而不说丢了什么，等于把猜测留给用户。
fn unpreserved_attributes(target: &SftpFileStat, staged: &SftpFileStat) -> Vec<String> {
    let mut missing = Vec::new();
    if target.mode.is_some() && staged.mode != target.mode {
        missing.push("mode".to_string());
    }
    if target.modified.is_some() && staged.modified != target.modified {
        missing.push("mtime".to_string());
    }
    if target.uid.is_some() && staged.uid != target.uid {
        missing.push("owner".to_string());
    }
    if target.gid.is_some() && staged.gid != target.gid {
        missing.push("group".to_string());
    }
    missing
}

/// 一次远端 `stat` 探针：SFTP 的属性里没有链接数，所以只能问一次 shell。
///
/// GNU（Linux）用 `-c %h`，BSD/macOS 用 `-f %l`，两个都试。路径必须是绝对的（策略已经保证），
/// 并且整段用单引号引用 —— 见 [`shell_single_quote`]。
pub fn link_count_probe_command(path: &str) -> Option<String> {
    if !path.starts_with('/') || path.contains('\n') {
        return None;
    }
    let quoted = shell_single_quote(path);
    Some(format!(
        "stat -c %h {quoted} 2>/dev/null || stat -f %l {quoted} 2>/dev/null"
    ))
}

/// POSIX 单引号引用：只有 `'` 需要处理，写成 `'\''` 是唯一正确的形式。
pub fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// 探针输出 → 链接数。只接受「退出码 0 + 一个非负整数」，其它一律 `None`（＝不知道）。
pub fn parse_link_count(stdout: &[u8], exit_code: i32) -> Option<u64> {
    if exit_code != 0 {
        return None;
    }
    std::str::from_utf8(stdout).ok()?.trim().parse().ok()
}

fn atomic_staging_path(path: &str, process_id: u32, nonce: u128) -> Option<String> {
    let (parent, name) = path.rsplit_once('/')?;
    if !path.starts_with('/') || name.is_empty() {
        return None;
    }
    let leaf = format!(".yukinal-write-{process_id}-{nonce}.tmp");
    Some(if parent == "/" {
        format!("/{leaf}")
    } else {
        format!("{parent}/{leaf}")
    })
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

#[cfg(test)]
mod tests {
    use super::{
        atomic_staging_path, link_count_probe_command, parse_link_count, shell_single_quote,
        unpreserved_attributes, SftpEntryKind, SftpFileStat,
    };

    #[test]
    fn atomic_staging_path_stays_in_the_target_directory() {
        assert_eq!(
            atomic_staging_path("/etc/app.env", 7, 9).as_deref(),
            Some("/etc/.yukinal-write-7-9.tmp")
        );
        assert_eq!(
            atomic_staging_path("/app.env", 7, 9).as_deref(),
            Some("/.yukinal-write-7-9.tmp")
        );
        assert_eq!(atomic_staging_path("relative/app.env", 7, 9), None);
        assert_eq!(atomic_staging_path("/etc/", 7, 9), None);
        assert_eq!(atomic_staging_path("app.env", 7, 9), None);
    }

    /// 路径来自用户与模型，所以它必须能安全地进入一条 shell 命令 —— 这是引用规则唯一的存在理由。
    #[test]
    fn a_remote_path_is_quoted_so_it_cannot_end_the_quotation() {
        assert_eq!(shell_single_quote("/etc/app.env"), "'/etc/app.env'");
        assert_eq!(shell_single_quote("/tmp/it's here"), "'/tmp/it'\\''s here'");
        assert_eq!(
            shell_single_quote("/tmp/$(rm -rf /)"),
            "'/tmp/$(rm -rf /)'",
            "命令替换必须留在引号里，而不是被执行"
        );
    }

    #[test]
    fn the_link_count_probe_tries_gnu_then_bsd_and_refuses_relative_paths() {
        assert_eq!(
            link_count_probe_command("/etc/app.env").as_deref(),
            Some("stat -c %h '/etc/app.env' 2>/dev/null || stat -f %l '/etc/app.env' 2>/dev/null")
        );
        assert_eq!(
            link_count_probe_command("/tmp/a'; rm -rf /'"),
            Some(
                "stat -c %h '/tmp/a'\\''; rm -rf /'\\''' 2>/dev/null || stat -f %l \
                 '/tmp/a'\\''; rm -rf /'\\''' 2>/dev/null"
                    .to_string()
            ),
            "引号里的内容不能被当成命令"
        );
        // 策略要求绝对路径，探针也照着这条规则拒绝：相对的路径没有唯一答案。
        assert_eq!(link_count_probe_command("etc/app.env"), None);
        assert_eq!(link_count_probe_command("/etc/app\n.env"), None);
    }

    #[test]
    fn only_a_clean_numeric_answer_counts_as_a_link_count() {
        assert_eq!(parse_link_count(b"1\n", 0), Some(1));
        assert_eq!(parse_link_count(b"  12  ", 0), Some(12));
        assert_eq!(parse_link_count(b"1\n", 1), None, "退出码说明探针没跑成");
        assert_eq!(parse_link_count(b"", 0), None);
        assert_eq!(parse_link_count(b"not a number", 0), None);
        // BSD 的 `stat -f %l` 在失败时可能打印诊断信息：不解析成数字就是「不知道」。
        assert_eq!(parse_link_count(b"stat: illegal option", 0), None);
    }

    #[test]
    fn metadata_that_did_not_survive_is_named_one_by_one() {
        let target = SftpFileStat {
            kind: SftpEntryKind::File,
            size: 10,
            modified: Some(1_700_000_000),
            mode: Some(0o640),
            uid: Some(1000),
            gid: Some(1000),
        };
        assert!(unpreserved_attributes(&target, &target).is_empty());

        let staged = SftpFileStat {
            mode: Some(0o600),
            modified: Some(1_700_000_123),
            uid: None,
            gid: Some(1000),
            ..target
        };
        assert_eq!(
            unpreserved_attributes(&target, &staged),
            vec!["mode", "mtime", "owner"],
            "缺的每一项都要点名，未缺的不能出现在里面"
        );
    }
}
