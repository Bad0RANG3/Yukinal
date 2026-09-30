//! UI's remote file browser: directory listing plus one bounded read.
//!
//! The rules and the limits live in `yukinal-filesystem`; this module only does two things:
//! adapt `TerminalService` (SFTP) to the capability's transport trait, and map results onto the
//! JSON shape these commands publish. The adapter lives here (rather than in a module of its own)
//! because this file is the file capability's command surface, and the Agent's host tools reach
//! the same adapter through [`remote_file_service`] — one transport implementation, two callers.

pub mod drag_out;
pub mod local_paths;
pub mod transfer;

pub(crate) use local_paths::{emit_local_drop, LocalPathHandles};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tauri::State;

use crate::commands::terminal::ensure_session;
use crate::state::AppState;
use yukinal_filesystem::{
    ListedEntry, RemoteEntryKind, RemoteFileService, RemoteFileTransport, RemoteStat, ReplaceError,
    ReplaceGuard, ReplacedFile, TransportError, TransportResult,
};

const MAX_FILE_PREVIEW_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FilePreviewKind {
    Text,
    Image,
    Binary,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FilePreviewResponse {
    pub name: String,
    pub size: u64,
    pub kind: FilePreviewKind,
    pub truncated: bool,
    pub text: Option<String>,
    pub data_url: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileEntry {
    pub name: String,
    pub path: String,
    pub r#type: String,
    pub size: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileListResponse {
    pub path: String,
    pub entries: Vec<RemoteFileEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileReadResponse {
    pub path: String,
    pub content: String,
    pub truncated: bool,
}

/// Desktop-side transport: `TerminalService`'s SFTP calls.
///
/// It lives on this side of the boundary because it needs `AppState` and `ensure_session` —
/// connection and credential resolution belong to the desktop layer, and the capability only sees
/// [`RemoteFileTransport`]. Every operation establishes the session first; a cached session makes
/// that a map lookup, so the extra call costs nothing on the hot path.
pub(crate) struct TerminalFileTransport<'a> {
    state: &'a AppState,
}

/// The file capability's assembly point: `AppState` → remote file service.
pub(crate) fn remote_file_service(
    state: &AppState,
) -> RemoteFileService<TerminalFileTransport<'_>> {
    RemoteFileService::new(TerminalFileTransport { state })
}

impl RemoteFileTransport for TerminalFileTransport<'_> {
    async fn list(&self, server_id: &str, path: &str) -> TransportResult<Vec<ListedEntry>> {
        ensure_session(self.state, server_id)
            .await
            .map_err(TransportError::new)?;
        let entries = self
            .state
            .terminals
            .sftp_list(server_id, path)
            .await
            .map_err(|error| TransportError::new(error.to_string()))?;
        Ok(entries
            .into_iter()
            .map(|(name, file_type, size)| ListedEntry {
                name,
                file_type,
                size,
            })
            .collect())
    }

    async fn read_bounded(
        &self,
        server_id: &str,
        path: &str,
        max_bytes: usize,
    ) -> TransportResult<Vec<u8>> {
        ensure_session(self.state, server_id)
            .await
            .map_err(TransportError::new)?;
        self.state
            .terminals
            .sftp_read_bounded(server_id, path, max_bytes)
            .await
            .map_err(|error| TransportError::new(error.to_string()))
    }

    async fn write(&self, server_id: &str, path: &str, data: &[u8]) -> TransportResult<()> {
        ensure_session(self.state, server_id)
            .await
            .map_err(TransportError::new)?;
        self.state
            .terminals
            .sftp_write(server_id, path, data)
            .await
            .map_err(|error| TransportError::new(error.to_string()))
    }

    async fn create_exclusive(
        &self,
        server_id: &str,
        path: &str,
        data: &[u8],
    ) -> TransportResult<()> {
        ensure_session(self.state, server_id)
            .await
            .map_err(TransportError::new)?;
        self.state
            .terminals
            .sftp_create_exclusive(server_id, path, data)
            .await
            .map_err(|error| TransportError::new(error.to_string()))
    }

    async fn remove_file(&self, server_id: &str, path: &str) -> TransportResult<()> {
        ensure_session(self.state, server_id)
            .await
            .map_err(TransportError::new)?;
        self.state
            .terminals
            .sftp_remove_file(server_id, path)
            .await
            .map_err(|error| TransportError::new(error.to_string()))
    }

    async fn stat(&self, server_id: &str, path: &str) -> TransportResult<RemoteStat> {
        ensure_session(self.state, server_id)
            .await
            .map_err(TransportError::new)?;
        let stat = self
            .state
            .terminals
            .sftp_stat(server_id, path)
            .await
            .map_err(|error| TransportError::new(error.to_string()))?;
        Ok(RemoteStat {
            kind: match stat.kind {
                yukinal_ssh::SftpEntryKind::File => RemoteEntryKind::File,
                yukinal_ssh::SftpEntryKind::Directory => RemoteEntryKind::Directory,
                yukinal_ssh::SftpEntryKind::Symlink => RemoteEntryKind::Symlink,
                yukinal_ssh::SftpEntryKind::Other => RemoteEntryKind::Other,
            },
            size: stat.size,
            modified: stat.modified,
        })
    }

    async fn link_count(&self, server_id: &str, path: &str) -> TransportResult<Option<u64>> {
        ensure_session(self.state, server_id)
            .await
            .map_err(TransportError::new)?;
        self.state
            .terminals
            .sftp_link_count(server_id, path)
            .await
            .map_err(|error| TransportError::new(error.to_string()))
    }

    async fn replace_guarded(
        &self,
        server_id: &str,
        path: &str,
        guard: &ReplaceGuard,
        data: &[u8],
    ) -> Result<ReplacedFile, ReplaceError> {
        ensure_session(self.state, server_id)
            .await
            .map_err(|error| ReplaceError::Transport(TransportError::new(error)))?;
        let outcome = self
            .state
            .terminals
            .sftp_replace_guarded(
                server_id,
                path,
                yukinal_ssh::SftpReplaceGuard {
                    size: guard.size,
                    modified: guard.modified,
                    content_digest: guard.content_digest,
                },
                data,
            )
            .await;
        match outcome {
            Ok(replacement) => Ok(ReplacedFile {
                size: replacement.size,
                modified: replacement.modified,
            }),
            Err(yukinal_core::terminal::SftpReplaceFailure::ConcurrentChange(detail)) => {
                Err(ReplaceError::ConcurrentChange(detail))
            }
            Err(yukinal_core::terminal::SftpReplaceFailure::Unsupported(detail)) => {
                Err(ReplaceError::Unsupported(detail))
            }
            Err(yukinal_core::terminal::SftpReplaceFailure::MetadataNotPreserved {
                message,
                missing,
            }) => Err(ReplaceError::MetadataNotPreserved { message, missing }),
            Err(yukinal_core::terminal::SftpReplaceFailure::Failed(detail)) => {
                Err(ReplaceError::Transport(TransportError::new(detail)))
            }
        }
    }
}

#[tauri::command]
pub async fn remote_file_list(
    state: State<'_, AppState>,
    server_id: String,
    path: String,
) -> Result<RemoteFileListResponse, String> {
    let listing = remote_file_service(&state)
        .list(&server_id, &path)
        .await
        .map_err(|error| error.to_string())?;
    Ok(RemoteFileListResponse {
        path: listing.path,
        entries: listing
            .entries
            .into_iter()
            .map(|entry| RemoteFileEntry {
                name: entry.name,
                path: entry.path,
                r#type: entry.file_type,
                size: entry.size,
            })
            .collect(),
    })
}

#[tauri::command]
pub async fn remote_file_read(
    state: State<'_, AppState>,
    server_id: String,
    path: String,
) -> Result<RemoteFileReadResponse, String> {
    let read = remote_file_service(&state)
        .browse_read(&server_id, &path)
        .await
        .map_err(|error| error.to_string())?;
    Ok(RemoteFileReadResponse {
        path: read.path,
        content: read.content,
        truncated: read.truncated,
    })
}

#[tauri::command]
pub async fn local_file_preview(
    registry: State<'_, LocalPathHandles>,
    handle_id: String,
) -> Result<FilePreviewResponse, String> {
    let (name, size, bytes) = registry
        .read_preview(&handle_id, MAX_FILE_PREVIEW_BYTES)
        .await?;
    Ok(make_file_preview(name, size, bytes))
}

#[tauri::command]
pub async fn remote_file_preview(
    state: State<'_, AppState>,
    server_id: String,
    path: String,
) -> Result<FilePreviewResponse, String> {
    yukinal_filesystem::validate_remote_path(&path).map_err(|error| error.to_string())?;
    if path.contains('\\') || path.split('/').any(|segment| matches!(segment, "." | "..")) {
        return Err(
            "remote file path cannot contain traversal segments or backslashes".to_string(),
        );
    }
    ensure_session(&state, &server_id).await?;
    let stat = state
        .terminals
        .sftp_stat(&server_id, &path)
        .await
        .map_err(|error| error.to_string())?;
    if stat.kind != yukinal_ssh::SftpEntryKind::File {
        return Err("only regular remote files can be previewed".to_string());
    }
    let bytes = state
        .terminals
        .sftp_read_bounded(&server_id, &path, MAX_FILE_PREVIEW_BYTES)
        .await
        .map_err(|error| error.to_string())?;
    let name = Path::new(&path)
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "远端文件".to_string());
    Ok(make_file_preview(name, stat.size, bytes))
}

fn make_file_preview(name: String, size: u64, mut bytes: Vec<u8>) -> FilePreviewResponse {
    let truncated = bytes.len() > MAX_FILE_PREVIEW_BYTES;
    bytes.truncate(MAX_FILE_PREVIEW_BYTES);
    if let Some(mime) = image_mime(&bytes).filter(|_| !truncated) {
        return FilePreviewResponse {
            name,
            size,
            kind: FilePreviewKind::Image,
            truncated,
            text: None,
            data_url: Some(format!(
                "data:{mime};base64,{}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            )),
        };
    }
    if !bytes.contains(&0) {
        match std::str::from_utf8(&bytes) {
            Ok(text) => {
                return FilePreviewResponse {
                    name,
                    size,
                    kind: FilePreviewKind::Text,
                    truncated,
                    text: Some(text.to_string()),
                    data_url: None,
                };
            }
            Err(error) if error.error_len().is_none() => {
                let text = std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap_or_default();
                return FilePreviewResponse {
                    name,
                    size,
                    kind: FilePreviewKind::Text,
                    truncated: true,
                    text: Some(text.to_string()),
                    data_url: None,
                };
            }
            _ => {}
        }
    }
    FilePreviewResponse {
        name,
        size,
        kind: FilePreviewKind::Binary,
        truncated,
        text: None,
        data_url: None,
    }
}

fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}
