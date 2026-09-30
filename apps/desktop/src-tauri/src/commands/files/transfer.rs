//! Native adapter for the streaming transfer capability.
//!
//! Tauri commands only exchange path handles and metadata. The transfer manager owns the byte
//! streams and talks to the already-authenticated SSH session through this adapter.

use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::broadcast;
use yukinal_filesystem::transfer::{
    ConflictAction, ConflictPolicy, RecoveredTransfer, RemoteTransferBackend, RemoteTransferEntry,
    TransferError, TransferFuture, TransferHistoryStore, TransferId, TransferManager,
    TransferReader, TransferSnapshot, TransferStagingFile, TransferWriter,
};
use yukinal_filesystem::{RemoteEntryKind, RemoteStat};
use yukinal_ssh::{SftpEntryKind, SshBackend};

use crate::commands::files::local_paths::LocalPathHandles;
use crate::commands::terminal::ensure_session;
use crate::state::AppState;

#[derive(Clone)]
pub struct FileTransferState {
    manager: TransferManager<SftpTransferBackend>,
}

impl FileTransferState {
    /// Open persistent transfer history and reconcile interrupted tasks before
    /// the application admits new file operations. `AppState` must already be
    /// managed by Tauri when this constructor runs.
    pub fn open(app: AppHandle) -> Result<Self, String> {
        let history: Arc<dyn TransferHistoryStore> =
            Arc::new(AppTransferHistoryStore { app: app.clone() });
        let manager = TransferManager::with_history(SftpTransferBackend { app }, history)
            .map_err(|error| error.to_string())?;
        Ok(Self { manager })
    }

    pub async fn shutdown(&self) {
        self.manager.shutdown().await;
    }

    pub fn start_download(
        &self,
        server_id: &str,
        remote_paths: Vec<String>,
        local_dir: PathBuf,
        policy: ConflictPolicy,
    ) -> Result<TransferId, String> {
        self.manager
            .start_download(server_id, remote_paths, local_dir, policy)
            .map_err(|error| error.to_string())
    }

    pub async fn get(&self, transfer_id: &TransferId) -> Option<TransferSnapshot> {
        self.manager.get(transfer_id).await
    }

    pub fn cancel(&self, transfer_id: &TransferId) -> Result<(), String> {
        self.manager
            .cancel(transfer_id)
            .map_err(|error| error.to_string())
    }

    pub async fn wait_for_terminal(
        &self,
        transfer_id: &TransferId,
        timeout: Duration,
    ) -> Result<TransferSnapshot, String> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut receiver = self.manager.subscribe_updates();
        loop {
            let snapshot = self
                .manager
                .get_result(transfer_id)
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "transfer was not found".to_string())?;
            if snapshot.status.is_terminal() {
                return Ok(snapshot);
            }
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(deadline) => {
                    return Err("timed out waiting for file transfer".into());
                }
                event = receiver.recv() => match event {
                    Ok(update) if update.transfer_id == *transfer_id => {}
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err("file transfer updates are unavailable".into());
                    }
                }
            }
        }
    }
}

#[derive(Clone)]
struct AppTransferHistoryStore {
    app: AppHandle,
}

impl AppTransferHistoryStore {
    fn to_record(
        snapshot: &TransferSnapshot,
        staging: &[TransferStagingFile],
    ) -> Result<yukinal_database::repositories::FileTransferRecord, String> {
        let snapshot_json = serde_json::to_string(snapshot).map_err(|error| error.to_string())?;
        let staging_json = serde_json::to_string(staging).map_err(|error| error.to_string())?;
        Ok(yukinal_database::repositories::FileTransferRecord {
            transfer_id: snapshot.transfer_id.0.clone(),
            server_id: snapshot.server_id.clone(),
            direction: snapshot.direction.as_str().into(),
            status: snapshot.status.as_str().into(),
            started_at_epoch_ms: snapshot.started_at_epoch_ms,
            updated_at_epoch_ms: snapshot.updated_at_epoch_ms,
            snapshot_json,
            staging_json,
        })
    }

    fn decode_record(
        record: yukinal_database::repositories::FileTransferRecord,
    ) -> Result<(TransferSnapshot, Vec<TransferStagingFile>), String> {
        let snapshot: TransferSnapshot =
            serde_json::from_str(&record.snapshot_json).map_err(|error| error.to_string())?;
        let staging: Vec<TransferStagingFile> =
            serde_json::from_str(&record.staging_json).map_err(|error| error.to_string())?;
        if snapshot.transfer_id.0 != record.transfer_id
            || snapshot.server_id != record.server_id
            || snapshot.direction.as_str() != record.direction
            || snapshot.status.as_str() != record.status
            || snapshot.started_at_epoch_ms != record.started_at_epoch_ms
            || snapshot.updated_at_epoch_ms != record.updated_at_epoch_ms
        {
            return Err("file transfer history row does not match its snapshot".into());
        }
        Ok((snapshot, staging))
    }
}

impl TransferHistoryStore for AppTransferHistoryStore {
    fn insert(
        &self,
        snapshot: &TransferSnapshot,
        staging: &[TransferStagingFile],
    ) -> Result<(), String> {
        let record = Self::to_record(snapshot, staging)?;
        self.app
            .state::<AppState>()
            .database
            .file_transfers()
            .insert(&record)
            .map_err(|error| error.to_string())
    }

    fn update(
        &self,
        snapshot: &TransferSnapshot,
        staging: &[TransferStagingFile],
    ) -> Result<(), String> {
        let record = Self::to_record(snapshot, staging)?;
        self.app
            .state::<AppState>()
            .database
            .file_transfers()
            .update(&record)
            .map_err(|error| error.to_string())
    }

    fn get(&self, transfer_id: &TransferId) -> Result<Option<TransferSnapshot>, String> {
        self.app
            .state::<AppState>()
            .database
            .file_transfers()
            .get(&transfer_id.0)
            .map_err(|error| error.to_string())?
            .map(|record| Self::decode_record(record).map(|(snapshot, _)| snapshot))
            .transpose()
    }

    fn list(&self, server_id: Option<&str>, limit: usize) -> Result<Vec<TransferSnapshot>, String> {
        let (records, _) = self
            .app
            .state::<AppState>()
            .database
            .file_transfers()
            .list(server_id, None, limit)
            .map_err(|error| error.to_string())?;
        records
            .into_iter()
            .map(|record| Self::decode_record(record).map(|(snapshot, _)| snapshot))
            .collect()
    }

    fn recover(&self, now_epoch_ms: u64) -> Result<Vec<RecoveredTransfer>, String> {
        self.app
            .state::<AppState>()
            .database
            .file_transfers()
            .interrupt_nonterminal(now_epoch_ms)
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|record| {
                let (snapshot, staging) = Self::decode_record(record)?;
                Ok(RecoveredTransfer { snapshot, staging })
            })
            .collect()
    }

    fn staging(&self, transfer_id: &TransferId) -> Result<Vec<TransferStagingFile>, String> {
        let staging_json = self
            .app
            .state::<AppState>()
            .database
            .file_transfers()
            .staging_json(&transfer_id.0)
            .map_err(|error| error.to_string())?;
        staging_json
            .map(|json| serde_json::from_str(&json).map_err(|error| error.to_string()))
            .transpose()
            .map(Option::unwrap_or_default)
    }
}

#[derive(Clone)]
struct SftpTransferBackend {
    app: AppHandle,
}

impl RemoteTransferBackend for SftpTransferBackend {
    fn supports_atomic_replace(&self) -> bool {
        false
    }

    fn lstat<'a>(
        &'a self,
        server_id: &'a str,
        path: &'a str,
    ) -> TransferFuture<'a, Option<RemoteStat>> {
        Box::pin(async move {
            let state = self.app.state::<AppState>();
            let (session, client) = sftp_client(&state, server_id).await?;
            let stat = state
                .ssh
                .sftp_lstat_optional(&client, path)
                .await
                .map_err(remote_error)?;
            drop(session);
            Ok(stat.map(map_stat))
        })
    }

    fn list_dir<'a>(
        &'a self,
        server_id: &'a str,
        path: &'a str,
    ) -> TransferFuture<'a, Vec<RemoteTransferEntry>> {
        Box::pin(async move {
            let state = self.app.state::<AppState>();
            let (_session, client) = sftp_client(&state, server_id).await?;
            let entries = state
                .ssh
                .sftp_list_dir_stat(&client, path)
                .await
                .map_err(remote_error)?;
            Ok(entries
                .into_iter()
                .map(|(name, stat)| RemoteTransferEntry {
                    name,
                    stat: map_stat(stat),
                })
                .collect())
        })
    }

    fn open_read<'a>(
        &'a self,
        server_id: &'a str,
        path: &'a str,
    ) -> TransferFuture<'a, TransferReader> {
        Box::pin(async move {
            let state = self.app.state::<AppState>();
            let (_session, client) = sftp_client(&state, server_id).await?;
            state
                .ssh
                .sftp_open_read_stream(&client, path)
                .await
                .map_err(remote_error)
        })
    }

    fn create_exclusive<'a>(
        &'a self,
        server_id: &'a str,
        path: &'a str,
    ) -> TransferFuture<'a, TransferWriter> {
        Box::pin(async move {
            let state = self.app.state::<AppState>();
            let (_session, client) = sftp_client(&state, server_id).await?;
            state
                .ssh
                .sftp_create_exclusive_stream(&client, path)
                .await
                .map_err(remote_error)
        })
    }

    fn create_dir<'a>(&'a self, server_id: &'a str, path: &'a str) -> TransferFuture<'a, ()> {
        Box::pin(async move {
            let state = self.app.state::<AppState>();
            let (_session, client) = sftp_client(&state, server_id).await?;
            state
                .ssh
                .sftp_create_dir(&client, path)
                .await
                .map_err(remote_error)
        })
    }

    fn remove_file<'a>(&'a self, server_id: &'a str, path: &'a str) -> TransferFuture<'a, ()> {
        Box::pin(async move {
            let state = self.app.state::<AppState>();
            let (_session, client) = sftp_client(&state, server_id).await?;
            state
                .ssh
                .sftp_remove_file(&client, path)
                .await
                .map_err(remote_error)
        })
    }

    fn publish_no_replace<'a>(
        &'a self,
        server_id: &'a str,
        source: &'a str,
        destination: &'a str,
    ) -> TransferFuture<'a, ()> {
        Box::pin(async move {
            let state = self.app.state::<AppState>();
            let (_session, client) = sftp_client(&state, server_id).await?;
            state
                .ssh
                .sftp_publish_no_replace(&client, source, destination)
                .await
                .map_err(|error| match error {
                    yukinal_ssh::Error::Configuration(message) => {
                        TransferError::Unsupported(message)
                    }
                    error => remote_error(error),
                })
        })
    }

    fn rename_replace<'a>(
        &'a self,
        _server_id: &'a str,
        _source: &'a str,
        _destination: &'a str,
    ) -> TransferFuture<'a, ()> {
        Box::pin(async {
            Err(TransferError::Unsupported(
                "the negotiated SFTP backend has no explicit atomic replace operation".to_string(),
            ))
        })
    }

    fn remove_recovered_staging<'a>(
        &'a self,
        server_id: &'a str,
        path: &'a str,
    ) -> TransferFuture<'a, ()> {
        Box::pin(async move {
            let state = self.app.state::<AppState>();
            let session = state.terminals.cached_session(server_id).map_err(|_| {
                TransferError::Unsupported(
                    "remote staging cleanup is waiting for an authenticated session".into(),
                )
            })?;
            let client = state.ssh.sftp(&session).await.map_err(remote_error)?;
            match state
                .ssh
                .sftp_lstat_optional(&client, path)
                .await
                .map_err(remote_error)?
            {
                None => Ok(()),
                Some(stat) if stat.kind == SftpEntryKind::File => state
                    .ssh
                    .sftp_remove_file(&client, path)
                    .await
                    .map_err(remote_error),
                Some(_) => Err(TransferError::InvalidInput(
                    "recovered remote staging entry is not a regular file".into(),
                )),
            }
        })
    }
}

async fn sftp_client(
    state: &AppState,
    server_id: &str,
) -> Result<(yukinal_ssh::Session, yukinal_ssh::SftpClient), TransferError> {
    ensure_session(state, server_id)
        .await
        .map_err(TransferError::Remote)?;
    let session = state
        .terminals
        .cached_session(server_id)
        .map_err(|error| TransferError::Remote(error.to_string()))?;
    let client = state.ssh.sftp(&session).await.map_err(remote_error)?;
    Ok((session, client))
}

fn remote_error(error: yukinal_ssh::Error) -> TransferError {
    TransferError::Remote(error.to_string())
}

fn map_stat(stat: yukinal_ssh::SftpFileStat) -> RemoteStat {
    RemoteStat {
        kind: match stat.kind {
            SftpEntryKind::File => RemoteEntryKind::File,
            SftpEntryKind::Directory => RemoteEntryKind::Directory,
            SftpEntryKind::Symlink => RemoteEntryKind::Symlink,
            SftpEntryKind::Other => RemoteEntryKind::Other,
        },
        size: stat.size,
        modified: stat.modified,
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferResponse {
    transfer: TransferSnapshot,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferListResponse {
    transfers: Vec<TransferSnapshot>,
}

#[tauri::command]
pub async fn file_transfer_upload(
    state: State<'_, FileTransferState>,
    registry: State<'_, LocalPathHandles>,
    server_id: String,
    remote_directory: String,
    source_handle_ids: Vec<String>,
) -> Result<TransferResponse, String> {
    let local_paths = registry.take_upload_sources(&source_handle_ids)?;
    let id = state
        .manager
        .start_upload(
            &server_id,
            &remote_directory,
            local_paths,
            ConflictPolicy::Ask,
        )
        .map_err(|error| error.to_string())?;
    snapshot_response(&state.manager, &id).await
}

#[tauri::command]
pub async fn file_transfer_download(
    state: State<'_, FileTransferState>,
    registry: State<'_, LocalPathHandles>,
    server_id: String,
    remote_paths: Vec<String>,
    destination_handle_id: String,
) -> Result<TransferResponse, String> {
    let local_directory = registry.take_download_directory(&destination_handle_id)?;
    let id = state
        .manager
        .start_download(
            &server_id,
            remote_paths,
            local_directory,
            ConflictPolicy::Ask,
        )
        .map_err(|error| error.to_string())?;
    snapshot_response(&state.manager, &id).await
}

#[tauri::command]
pub async fn file_transfer_list(
    state: State<'_, FileTransferState>,
    server_id: Option<String>,
) -> Result<TransferListResponse, String> {
    state.manager.reconcile_remote_staging().await;
    let transfers = state
        .manager
        .list_bounded(server_id.as_deref(), 100)
        .await
        .map_err(|error| error.to_string())?;
    Ok(TransferListResponse { transfers })
}

#[tauri::command]
pub async fn file_transfer_get(
    state: State<'_, FileTransferState>,
    transfer_id: String,
) -> Result<TransferResponse, String> {
    let id = TransferId(transfer_id);
    state
        .get(&id)
        .await
        .map(|transfer| TransferResponse { transfer })
        .ok_or_else(|| "transfer was not found".to_string())
}

#[tauri::command]
pub async fn file_transfer_cancel(
    state: State<'_, FileTransferState>,
    transfer_id: String,
) -> Result<TransferResponse, String> {
    let id = TransferId(transfer_id);
    state
        .manager
        .cancel(&id)
        .map_err(|error| error.to_string())?;
    snapshot_response(&state.manager, &id).await
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", deny_unknown_fields)]
pub enum ConflictActionRequest {
    Skip,
    Overwrite,
    Rename { name: String },
}

#[tauri::command]
pub async fn file_transfer_resolve_conflict(
    state: State<'_, FileTransferState>,
    transfer_id: String,
    action: ConflictActionRequest,
) -> Result<TransferResponse, String> {
    let id = TransferId(transfer_id);
    let current = state
        .manager
        .get(&id)
        .await
        .ok_or_else(|| "transfer was not found".to_string())?;
    let conflict = current
        .active_conflict
        .ok_or_else(|| "transfer is not waiting for conflict resolution".to_string())?;
    let action = match action {
        ConflictActionRequest::Skip => ConflictAction::Skip,
        ConflictActionRequest::Overwrite => ConflictAction::Overwrite,
        ConflictActionRequest::Rename { name } => ConflictAction::Rename { name },
    };
    state
        .manager
        .resolve_conflict(&id, conflict.item_index, action)
        .await
        .map_err(|error| error.to_string())?;
    snapshot_response(&state.manager, &id).await
}

async fn snapshot_response(
    manager: &TransferManager<SftpTransferBackend>,
    id: &TransferId,
) -> Result<TransferResponse, String> {
    manager
        .get_result(id)
        .await
        .map_err(|error| error.to_string())?
        .map(|transfer| TransferResponse { transfer })
        .ok_or_else(|| "transfer was not found".to_string())
}

pub(crate) fn forward_transfer_updates(app: AppHandle) {
    let shutdown = app.state::<AppState>().shutdown.clone();
    tauri::async_runtime::spawn(async move {
        let mut receiver = app.state::<FileTransferState>().manager.subscribe_updates();
        loop {
            let event = tokio::select! {
                biased;
                _ = shutdown.cancelled() => break,
                event = receiver.recv() => event,
            };
            match event {
                Ok(transfer) => {
                    let _ = app.emit(
                        &crate::commands::tauri_event_name("file.transfer_updated"),
                        serde_json::json!({ "transfer": transfer }),
                    );
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}
