use serde::Serialize;
use tauri::State;

use crate::commands::terminal::ensure_session;
use crate::state::AppState;

const MAX_READ_BYTES: usize = 1024 * 1024;

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

#[tauri::command]
pub async fn remote_file_list(
    state: State<'_, AppState>,
    server_id: String,
    path: String,
) -> Result<RemoteFileListResponse, String> {
    ensure_session(&state, &server_id).await?;
    let entries = state
        .terminals
        .sftp_list(&server_id, &path)
        .await
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|(name, file_type, size)| RemoteFileEntry {
            path: join_remote_path(&path, &name),
            name,
            r#type: file_type,
            size,
        })
        .collect();
    Ok(RemoteFileListResponse { path, entries })
}

#[tauri::command]
pub async fn remote_file_read(
    state: State<'_, AppState>,
    server_id: String,
    path: String,
) -> Result<RemoteFileReadResponse, String> {
    ensure_session(&state, &server_id).await?;
    let bytes = state
        .terminals
        .sftp_read_bounded(&server_id, &path, MAX_READ_BYTES)
        .await
        .map_err(|error| error.to_string())?;
    let truncated = bytes.len() > MAX_READ_BYTES;
    let content = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_READ_BYTES)]).into_owned();
    Ok(RemoteFileReadResponse {
        path,
        content,
        truncated,
    })
}

fn join_remote_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else if parent.ends_with('/') {
        format!("{parent}{name}")
    } else {
        format!("{parent}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::join_remote_path;

    #[test]
    fn joins_posix_paths_without_double_slashes() {
        assert_eq!(join_remote_path("/etc", "hosts"), "/etc/hosts");
        assert_eq!(join_remote_path("/", "hosts"), "/hosts");
        assert_eq!(join_remote_path("/etc/", "hosts"), "/etc/hosts");
    }
}
