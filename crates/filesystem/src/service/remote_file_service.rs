//! `RemoteFileService`：策略 + 上限 + 有界解码，套在一个 `RemoteFileTransport` 外面。

use crate::limits::{BROWSER_READ_BYTES, MAX_AGENT_BACKUP_BYTES, MAX_AGENT_EDIT_BYTES};
use crate::revision::{content_digest, content_revision};

use super::error::{Error, Result};
use super::helpers::{byte_match_offsets, count_lines, join_remote_path, read_result};
use super::request::{
    AgentCleanupBackupRequest, AgentEditRequest, AgentReadRequest, AgentWriteRequest,
};
use super::types::{
    RemoteBackup, RemoteBackupCleanup, RemoteEdit, RemoteEntry, RemoteEntryKind,
    RemoteFileTransport, RemoteListing, RemoteRead, RemoteRestore, RemoteWrite, ReplaceError,
    ReplaceGuard,
};

/// 远端文件服务：策略 + 上限 + 有界解码，套在一个 [`RemoteFileTransport`] 外面。
pub struct RemoteFileService<T> {
    transport: T,
}

impl<T: RemoteFileTransport> RemoteFileService<T> {
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    /// 目录列表（UI 浏览器用；Agent 目前没有列表工具）。
    ///
    /// 刻意不套用凭据黑名单，也不做路径形状校验：浏览器的操作者是人，而 Agent 的策略不是
    /// 给人用的（见 crate 文档）。这里唯一生效的规则是「名字归一化成绝对路径」。
    pub async fn list(&self, server_id: &str, path: &str) -> Result<RemoteListing> {
        let entries = self.transport.list(server_id, path).await?;
        Ok(RemoteListing {
            path: path.to_string(),
            entries: entries
                .into_iter()
                .map(|entry| RemoteEntry {
                    path: join_remote_path(path, &entry.name),
                    name: entry.name,
                    file_type: entry.file_type,
                    size: entry.size,
                })
                .collect(),
        })
    }

    /// UI 浏览器的读取：上限固定为 [`BROWSER_READ_BYTES`]，调用方无法要求更多，也不查黑名单。
    ///
    /// 它同样会算出 revision（与 Agent 的读取共用 [`read_result`]，算法只有一份），但命令层不
    /// 发布它：浏览器没有编辑入口，多一个字段只是没人用的契约。
    pub async fn browse_read(&self, server_id: &str, path: &str) -> Result<RemoteRead> {
        let bytes = self
            .transport
            .read_bounded(server_id, path, BROWSER_READ_BYTES)
            .await?;
        Ok(read_result(path, &bytes, BROWSER_READ_BYTES))
    }

    /// Agent `filesystem.read`：请求已经带过策略与上限，这里只做有界读取。
    pub async fn agent_read(
        &self,
        server_id: &str,
        request: &AgentReadRequest,
    ) -> Result<RemoteRead> {
        let bytes = self
            .transport
            .read_bounded(server_id, &request.path, request.max_bytes)
            .await?;
        Ok(read_result(&request.path, &bytes, request.max_bytes))
    }

    /// Agent `filesystem.write`（覆盖写）。
    pub async fn agent_write(
        &self,
        server_id: &str,
        request: &AgentWriteRequest,
    ) -> Result<RemoteWrite> {
        self.transport
            .write(server_id, &request.path, request.content.as_bytes())
            .await?;
        Ok(RemoteWrite {
            path: request.path.clone(),
            bytes_written: request.content.len(),
        })
    }

    /// Agent `filesystem.backup`: read one complete regular file and create a host-derived sibling
    /// without replacing an existing recovery copy.
    pub async fn agent_backup(
        &self,
        server_id: &str,
        request: &super::request::AgentBackupRequest,
    ) -> Result<RemoteBackup> {
        let before = self.transport.stat(server_id, &request.path).await?;
        ensure_copyable_file(&request.path, before, "backup")?;
        ensure_single_link(
            &request.path,
            self.transport.link_count(server_id, &request.path).await?,
        )?;
        let bytes = self
            .transport
            .read_bounded(server_id, &request.path, MAX_AGENT_BACKUP_BYTES)
            .await?;
        if bytes.len() > MAX_AGENT_BACKUP_BYTES {
            return Err(Error::FileTooLargeToBackup {
                limit: MAX_AGENT_BACKUP_BYTES,
            });
        }
        let after = self.transport.stat(server_id, &request.path).await?;
        if before.size != after.size || before.modified != after.modified {
            return Err(Error::ConcurrentChange(format!(
                "{} changed while the backup was being read; re-read it and create a new backup",
                request.path
            )));
        }
        let revision = content_revision(&bytes);
        self.transport
            .create_exclusive(server_id, &request.backup_path, &bytes)
            .await?;
        Ok(RemoteBackup {
            path: request.path.clone(),
            backup_path: request.backup_path.clone(),
            revision,
            bytes_backed_up: bytes.len(),
        })
    }

    /// Agent `filesystem.restore`: atomically replace the target with a host-owned backup only if
    /// the target still has the caller's expected revision.
    pub async fn agent_restore(
        &self,
        server_id: &str,
        request: &super::request::AgentRestoreRequest,
    ) -> Result<RemoteRestore> {
        let target_before = self.transport.stat(server_id, &request.path).await?;
        ensure_copyable_file(&request.path, target_before, "restore")?;
        ensure_single_link(
            &request.path,
            self.transport.link_count(server_id, &request.path).await?,
        )?;
        let target_bytes = self
            .transport
            .read_bounded(server_id, &request.path, MAX_AGENT_BACKUP_BYTES)
            .await?;
        if target_bytes.len() > MAX_AGENT_BACKUP_BYTES {
            return Err(Error::FileTooLargeToBackup {
                limit: MAX_AGENT_BACKUP_BYTES,
            });
        }
        let actual = content_revision(&target_bytes);
        if !actual.eq_ignore_ascii_case(&request.expected_revision) {
            return Err(Error::RevisionMismatch {
                expected: request.expected_revision.clone(),
                actual,
            });
        }
        let target_digest = content_digest(&target_bytes);
        let target_after = self.transport.stat(server_id, &request.path).await?;
        if target_before.size != target_after.size
            || target_before.modified != target_after.modified
        {
            return Err(Error::ConcurrentChange(format!(
                "{} changed while the restore guard was being checked; re-read it and retry",
                request.path
            )));
        }

        let backup_stat = self.transport.stat(server_id, &request.backup_path).await?;
        ensure_copyable_file(&request.backup_path, backup_stat, "restore source")?;
        ensure_single_link(
            &request.backup_path,
            self.transport
                .link_count(server_id, &request.backup_path)
                .await?,
        )?;
        let backup_bytes = self
            .transport
            .read_bounded(server_id, &request.backup_path, MAX_AGENT_BACKUP_BYTES)
            .await?;
        if backup_bytes.len() > MAX_AGENT_BACKUP_BYTES {
            return Err(Error::FileTooLargeToBackup {
                limit: MAX_AGENT_BACKUP_BYTES,
            });
        }
        let backup_after = self.transport.stat(server_id, &request.backup_path).await?;
        if backup_stat.size != backup_after.size || backup_stat.modified != backup_after.modified {
            return Err(Error::ConcurrentChange(format!(
                "{} changed while the restore source was being read; create a new recovery plan",
                request.backup_path
            )));
        }

        let replaced = self
            .transport
            .replace_guarded(
                server_id,
                &request.path,
                &ReplaceGuard {
                    size: target_before.size,
                    modified: target_before.modified,
                    content_digest: target_digest,
                },
                &backup_bytes,
            )
            .await
            .map_err(map_replace_error)?;
        if replaced.size != backup_bytes.len() as u64 {
            return Err(Error::ConcurrentChange(format!(
                "{} is {} bytes after restore instead of the {} bytes from the backup",
                request.path,
                replaced.size,
                backup_bytes.len()
            )));
        }
        Ok(RemoteRestore {
            path: request.path.clone(),
            backup_path: request.backup_path.clone(),
            revision: content_revision(&backup_bytes),
            bytes_before: target_bytes.len(),
            bytes_after: backup_bytes.len(),
        })
    }

    /// Agent `filesystem.backup.cleanup`: verify the host-owned recovery copy
    /// still contains the expected bytes, then remove that exact sibling path.
    /// The caller must separately prove ownership through the host ledger.
    pub async fn agent_cleanup_backup(
        &self,
        server_id: &str,
        request: &AgentCleanupBackupRequest,
    ) -> Result<RemoteBackupCleanup> {
        let before = self.transport.stat(server_id, &request.backup_path).await?;
        ensure_copyable_file(&request.backup_path, before, "backup cleanup")?;
        ensure_single_link(
            &request.backup_path,
            self.transport
                .link_count(server_id, &request.backup_path)
                .await?,
        )?;
        let bytes = self
            .transport
            .read_bounded(server_id, &request.backup_path, MAX_AGENT_BACKUP_BYTES)
            .await?;
        if bytes.len() > MAX_AGENT_BACKUP_BYTES {
            return Err(Error::FileTooLargeToBackup {
                limit: MAX_AGENT_BACKUP_BYTES,
            });
        }
        let actual = content_revision(&bytes);
        if !actual.eq_ignore_ascii_case(&request.expected_revision) {
            return Err(Error::RevisionMismatch {
                expected: request.expected_revision.clone(),
                actual,
            });
        }
        let after = self.transport.stat(server_id, &request.backup_path).await?;
        if before.size != after.size || before.modified != after.modified {
            return Err(Error::ConcurrentChange(format!(
                "{} changed while the backup was being checked for cleanup; preserve it and reconcile",
                request.backup_path
            )));
        }
        self.transport
            .remove_file(server_id, &request.backup_path)
            .await?;
        Ok(RemoteBackupCleanup {
            path: request.path.clone(),
            backup_path: request.backup_path.clone(),
            revision: content_revision(&bytes),
            bytes_deleted: bytes.len(),
        })
    }

    /// Agent `filesystem.edit`：一次**有守卫的精确替换**（先读后改）。
    ///
    /// # 顺序
    /// 读全文 → 拒绝超限文件 → 校验 revision → 定位 `oldString`（必须恰好一次）→ 写回。
    /// 每一步的失败都在写回**之前**返回，所以被拒绝的编辑不会碰到文件 —— 测试用假传输的调用
    /// 记录来证明这一点，而不只是看错误类型。
    ///
    /// # 它保证什么
    /// - 只有当文件的内容与 `expectedRevision` 完全一致时才写；
    /// - 只有当 `oldString` 恰好出现一次时才写（零次或多次都会拒绝，让模型重新读、给出更多
    ///   上下文，而不是让它猜）；
    /// - 只有整份文件都读进来了才写（见 [`MAX_AGENT_EDIT_BYTES`]）—— 前缀不足以写回；
    /// - 写回的是原始**字节**：读路径的有损 UTF-8 解码只用于「看一眼」，不参与编辑，所以
    ///   一份非 UTF-8 文件不会被编辑悄悄改写成替换字符。
    ///
    /// # 它不保证什么（必须说清楚）
    /// **替换阶段是原子的，但整条守卫仍不是比较并交换**（ADR 0017）。SFTP 没有 CAS，所以：
    /// - 检查与 rename 之间仍有一个窗口；rename 前会复核大小、mtime 和内容 SHA-256，因此
    ///   同一秒、同样大小的改写也会拒绝。SFTP 没有 CAS，最后一次内容复核之后到 rename 之间
    ///   仍有一个很窄的窗口；
    /// - metadata 必须逐项保留，保不住就整次失败并点名（[`Error::MetadataNotPreserved`]）；
    /// - symlink、硬链接、以及**无法确认硬链接**的远端一律拒绝（[`Error::UnsafeRemoteWrite`]），
    ///   而不是换一种更弱的写法继续。
    pub async fn agent_edit(
        &self,
        server_id: &str,
        request: &AgentEditRequest,
    ) -> Result<RemoteEdit> {
        let bytes = self
            .transport
            .read_bounded(server_id, &request.path, MAX_AGENT_EDIT_BYTES)
            .await?;
        // 传输用「多给一个字节」表示还有更多（见 `decode_bounded`），所以「超过上限」就是
        // `>`：等于上限的文件是完整的，可以编辑。
        if bytes.len() > MAX_AGENT_EDIT_BYTES {
            return Err(Error::FileTooLargeToEdit {
                limit: MAX_AGENT_EDIT_BYTES,
            });
        }

        let actual = content_revision(&bytes);
        // 大小写不敏感：Agent 可能把 revision 原样抄成大写，那不是「文件变了」。
        if !actual.eq_ignore_ascii_case(&request.expected_revision) {
            return Err(Error::RevisionMismatch {
                expected: request.expected_revision.to_ascii_lowercase(),
                actual,
            });
        }
        let original_digest = content_digest(&bytes);

        let old = request.old_string.as_bytes();
        let start = match byte_match_offsets(&bytes, old).as_slice() {
            [] => {
                return Err(Error::InvalidInput(
                    "oldString was not found in the file; re-read it and pass text that appears in it verbatim"
                        .to_string(),
                ))
            }
            [offset] => *offset,
            matches => {
                return Err(Error::InvalidInput(format!(
                    "oldString occurs {} times in the file; re-read it and include enough surrounding context to make it unique",
                    matches.len()
                )))
            }
        };

        let mut updated = Vec::with_capacity(bytes.len() + request.new_string.len());
        updated.extend_from_slice(&bytes[..start]);
        updated.extend_from_slice(request.new_string.as_bytes());
        updated.extend_from_slice(&bytes[start + old.len()..]);
        // 一次编辑不能产出 `write` 写不了的文件：否则它就成了绕过写入上限的路径。
        if updated.len() > MAX_AGENT_EDIT_BYTES {
            return Err(Error::InvalidInput(format!(
                "the edit would produce a file of {} bytes, over the {MAX_AGENT_EDIT_BYTES}-byte cap for one operation; filesystem.write must be used deliberately instead",
                updated.len()
            )));
        }

        // 读取之后才取守卫：这之前的改动已经由 revision 检查覆盖，之后的改动由守卫覆盖。
        let target = self.transport.stat(server_id, &request.path).await?;
        match target.kind {
            RemoteEntryKind::File => {}
            RemoteEntryKind::Symlink => {
                return Err(Error::UnsafeRemoteWrite(format!(
                    "{} is a symlink; replacing the path would replace the link itself, so this tool refuses. Read the target path directly if that is what you meant",
                    request.path
                )))
            }
            _ => {
                return Err(Error::InvalidInput(format!(
                    "{} is not a regular file, so there is nothing to edit",
                    request.path
                )))
            }
        }
        if target.size != bytes.len() as u64 {
            return Err(Error::ConcurrentChange(format!(
                "{} changed between the read ({} bytes) and the metadata check ({} bytes); re-read it and retry",
                request.path,
                bytes.len(),
                target.size
            )));
        }
        // 硬链接：rename 会让其他名字留在旧内容上，所以要么拒绝、要么原位写。这里拒绝，
        // 因为原位写会丢掉这个工具承诺的原子发布；`filesystem.write` 才是明确要求覆盖写的工具。
        match self.transport.link_count(server_id, &request.path).await? {
            Some(1) => {}
            Some(count) => {
                return Err(Error::UnsafeRemoteWrite(format!(
                    "{} has {count} hard links; replacing it would leave the other names pointing at the old content. Edit a path with a single name, or use filesystem.write if overwriting in place is what you mean",
                    request.path
                )))
            }
            None => {
                return Err(Error::UnsafeRemoteWrite(format!(
                    "the server does not report how many names point at {}, so a hard link cannot be ruled out and this tool refuses to replace it. filesystem.write is the deliberate in-place overwrite",
                    request.path
                )))
            }
        }

        let replaced = self
            .transport
            .replace_guarded(
                server_id,
                &request.path,
                &ReplaceGuard {
                    size: target.size,
                    modified: target.modified,
                    content_digest: original_digest,
                },
                &updated,
            )
            .await
            .map_err(map_replace_error)?;
        // 发布复核：rename 之后目标必须就是我们写进去的那一份。
        if replaced.size != updated.len() as u64 {
            return Err(Error::ConcurrentChange(format!(
                "{} is {} bytes after the replacement instead of the {} bytes that were written; another writer replaced it during the edit, so its content is not the one this call produced",
                request.path,
                replaced.size,
                updated.len()
            )));
        }
        Ok(RemoteEdit {
            path: request.path.clone(),
            revision: content_revision(&updated),
            bytes_before: bytes.len(),
            bytes_after: updated.len(),
            line_delta: count_lines(&updated) - count_lines(&bytes),
        })
    }
}

fn ensure_copyable_file(path: &str, stat: super::types::RemoteStat, operation: &str) -> Result<()> {
    if stat.kind != RemoteEntryKind::File {
        return Err(Error::UnsafeRemoteWrite(format!(
            "{path} is not a regular file, so it cannot be used for {operation}"
        )));
    }
    // A backup or restore published through rename must not silently leave another hard-linked
    // name pointing at the old bytes. Unknown link counts fail closed just like filesystem.edit.
    // The caller performs the transport probe separately so this helper only owns the shape check.
    Ok(())
}

fn ensure_single_link(path: &str, links: Option<u64>) -> Result<()> {
    match links {
        Some(1) => Ok(()),
        Some(count) => Err(Error::UnsafeRemoteWrite(format!(
            "{path} has {count} hard links; copying or restoring it would not describe one independent file"
        ))),
        None => Err(Error::UnsafeRemoteWrite(format!(
            "the server did not report how many names point at {path}, so the file operation refuses to guess"
        ))),
    }
}

/// [`ReplaceError`] → 服务层的失败分类。三类分开传，不合并成一句话。
fn map_replace_error(error: ReplaceError) -> Error {
    match error {
        ReplaceError::ConcurrentChange(message) => Error::ConcurrentChange(message),
        ReplaceError::Unsupported(message) => Error::UnsafeRemoteWrite(message),
        ReplaceError::MetadataNotPreserved { message, missing } => {
            Error::MetadataNotPreserved { message, missing }
        }
        ReplaceError::Transport(error) => Error::Transport(error),
    }
}

#[cfg(test)]
mod tests;
