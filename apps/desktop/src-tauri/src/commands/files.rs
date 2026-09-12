//! UI's remote file browser: directory listing plus one bounded read.
//!
//! The rules and the limits live in `yukinal-filesystem`; this module only does two things:
//! adapt `TerminalService` (SFTP) to the capability's transport trait, and map results onto the
//! JSON shape these commands publish. The adapter lives here (rather than in a module of its own)
//! because this file is the file capability's command surface, and the Agent's host tools reach
//! the same adapter through [`remote_file_service`] — one transport implementation, two callers.

use serde::Serialize;
use tauri::State;

use crate::commands::terminal::ensure_session;
use crate::state::AppState;
use yukinal_filesystem::{
    ListedEntry, RemoteFileService, RemoteFileTransport, TransportError, TransportResult,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileEntry {
    pub name: String,
    pub path: String,
    pub r#type: String,
    pub size: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileListResponse {
    pub path: String,
    pub entries: Vec<RemoteFileEntry>,
}

#[derive(Debug, Serialize)]
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
