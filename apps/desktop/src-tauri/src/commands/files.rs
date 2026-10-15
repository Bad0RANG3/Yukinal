//! UI's remote file browser: directory listing, one bounded read, and metadata.
//!
//! The rules and limits live in `yukinal-filesystem`; this module adapts the C++ SFTP service to
//! the capability transport trait and maps results onto the JSON shape these commands publish.
//! The Agent's host tools reach the same adapter through [`remote_file_service`], so both callers
//! share one transport implementation.

pub mod drag_out;
pub mod local_paths;
pub mod transfer;

pub(crate) use local_paths::{emit_local_drop, LocalPathHandles};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tauri::{AppHandle, State};

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

/// Desktop-side transport for bounded file operations through the C++ SFTP service.
///
/// This adapter needs `AppState`, the local trust decision and credentials. The file capability
/// sees only [`RemoteFileTransport`]; it cannot choose or widen the SSH target.
pub(crate) struct TerminalFileTransport<'a> {
    state: &'a AppState,
    app: Option<AppHandle>,
}

/// The file capability's assembly point: `AppState` → remote file service.
pub(crate) fn remote_file_service<'a>(
    state: &'a AppState,
    app: Option<&AppHandle>,
) -> RemoteFileService<TerminalFileTransport<'a>> {
    RemoteFileService::new(TerminalFileTransport {
        state,
        app: app.cloned(),
    })
}

fn cpp_ssh_file_operations_supported(
    state: &AppState,
    server_id: &str,
) -> Result<bool, String> {
    crate::commands::ssh_service::supports_cpp_target(state, server_id)
}

impl TerminalFileTransport<'_> {
    fn cpp_app(&self, server_id: &str) -> TransportResult<&AppHandle> {
        if !cpp_ssh_file_operations_supported(self.state, server_id)
            .map_err(TransportError::new)?
        {
            return Err(TransportError::new(
                "the selected target is not supported by the C++ SSH service",
            ));
        }
        self.app.as_ref().ok_or_else(|| {
            TransportError::new("the C++ SSH service is unavailable in this host context")
        })
    }
}

impl RemoteFileTransport for TerminalFileTransport<'_> {
    async fn list(&self, server_id: &str, path: &str) -> TransportResult<Vec<ListedEntry>> {
        let app = self.cpp_app(server_id)?;
        let entries = crate::commands::ssh_service::sftp_list(app, self.state, server_id, path)
            .await
            .map_err(TransportError::new)?;
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
        let app = self.cpp_app(server_id)?;
        crate::commands::ssh_service::sftp_read_bounded(app, self.state, server_id, path, max_bytes)
            .await
            .map_err(TransportError::new)
    }

    async fn write(&self, server_id: &str, path: &str, data: &[u8]) -> TransportResult<()> {
        let app = self.cpp_app(server_id)?;
        crate::commands::ssh_service::sftp_write(app, self.state, server_id, path, data)
            .await
            .map_err(TransportError::new)
    }

    async fn create_exclusive(
        &self,
        server_id: &str,
        path: &str,
        data: &[u8],
    ) -> TransportResult<()> {
        let app = self.cpp_app(server_id)?;
        crate::commands::ssh_service::sftp_create_exclusive(app, self.state, server_id, path, data)
            .await
            .map_err(TransportError::new)
    }

    async fn remove_file(&self, server_id: &str, path: &str) -> TransportResult<()> {
        let app = self.cpp_app(server_id)?;
        crate::commands::ssh_service::sftp_remove_file(app, self.state, server_id, path)
            .await
            .map_err(TransportError::new)
    }

    async fn stat(&self, server_id: &str, path: &str) -> TransportResult<RemoteStat> {
        let app = self.cpp_app(server_id)?;
        crate::commands::ssh_service::sftp_stat(app, self.state, server_id, path)
            .await
            .map_err(TransportError::new)
    }

    async fn link_count(&self, server_id: &str, path: &str) -> TransportResult<Option<u64>> {
        let app = self.cpp_app(server_id)?;
        crate::commands::ssh_service::sftp_link_count(app, self.state, server_id, path)
            .await
            .map_err(TransportError::new)
    }

    async fn replace_guarded(
        &self,
        server_id: &str,
        path: &str,
        guard: &ReplaceGuard,
        data: &[u8],
    ) -> Result<ReplacedFile, ReplaceError> {
        let app = self.cpp_app(server_id).map_err(ReplaceError::Transport)?;
        crate::commands::ssh_service::sftp_replace_guarded(
            app, self.state, server_id, path, guard, data,
        )
        .await
    }
}

#[tauri::command]
pub async fn remote_file_list(
    app: AppHandle,
    state: State<'_, AppState>,
    server_id: String,
    path: String,
) -> Result<RemoteFileListResponse, String> {
    let listing = remote_file_service(&state, Some(&app))
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
    app: AppHandle,
    state: State<'_, AppState>,
    server_id: String,
    path: String,
) -> Result<RemoteFileReadResponse, String> {
    let read = remote_file_service(&state, Some(&app))
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
    app: AppHandle,
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
    let transport = TerminalFileTransport {
        state: &state,
        app: Some(app),
    };
    let stat = transport
        .stat(&server_id, &path)
        .await
        .map_err(|error| error.to_string())?;
    if stat.kind != RemoteEntryKind::File {
        return Err("only regular remote files can be previewed".to_string());
    }
    let bytes = transport
        .read_bounded(&server_id, &path, MAX_FILE_PREVIEW_BYTES)
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
