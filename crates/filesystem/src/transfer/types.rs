use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::service::RemoteStat;

/// Future used by the small SFTP capability seam.
pub type TransferFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, TransferError>> + Send + 'a>>;

/// A reader returned by a remote SFTP backend. It is an owned stream, not a byte buffer.
pub type TransferReader = Box<dyn AsyncRead + Send + Unpin + 'static>;

/// A writer for an exclusively-created remote staging file.
pub type TransferWriter = Box<dyn AsyncWrite + Send + Unpin + 'static>;

/// Remote directory entry returned by `lstat`-aware recursive traversal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteTransferEntry {
    pub name: String,
    pub stat: RemoteStat,
}

/// Minimal remote filesystem capability required by the transfer lifecycle.
///
/// Implementations must use SFTP streaming file handles. `lstat` must not follow symbolic links;
/// `create_exclusive` must fail if any entry already occupies the path. `publish_no_replace` must
/// atomically create the destination only when absent or return `Unsupported`; it leaves the
/// source staging file in place for explicit cleanup. `rename_replace` must perform a
/// same-directory atomic rename over a regular file or return `Unsupported`. In particular, an
/// implementation must not emulate either publication mode by copying bytes into the destination.
///
/// A normal SFTP rename has no compare-and-swap guarantee. The manager rechecks target metadata
/// immediately before publication, but a remote writer can still race that final check.
pub trait RemoteTransferBackend: Send + Sync + 'static {
    /// Whether the remote server supports an explicit atomic overwrite rename. Ordinary SFTP v3
    /// `SSH_FXP_RENAME` does not establish this guarantee, so the default is fail-closed.
    fn supports_atomic_replace(&self) -> bool {
        false
    }

    fn lstat<'a>(
        &'a self,
        server_id: &'a str,
        path: &'a str,
    ) -> TransferFuture<'a, Option<RemoteStat>>;

    fn list_dir<'a>(
        &'a self,
        server_id: &'a str,
        path: &'a str,
    ) -> TransferFuture<'a, Vec<RemoteTransferEntry>>;

    fn open_read<'a>(
        &'a self,
        server_id: &'a str,
        path: &'a str,
    ) -> TransferFuture<'a, TransferReader>;

    fn create_exclusive<'a>(
        &'a self,
        server_id: &'a str,
        path: &'a str,
    ) -> TransferFuture<'a, TransferWriter>;

    fn create_dir<'a>(&'a self, server_id: &'a str, path: &'a str) -> TransferFuture<'a, ()>;

    fn remove_file<'a>(&'a self, server_id: &'a str, path: &'a str) -> TransferFuture<'a, ()>;

    fn publish_no_replace<'a>(
        &'a self,
        server_id: &'a str,
        source: &'a str,
        destination: &'a str,
    ) -> TransferFuture<'a, ()>;

    fn rename_replace<'a>(
        &'a self,
        server_id: &'a str,
        source: &'a str,
        destination: &'a str,
    ) -> TransferFuture<'a, ()>;

    /// Remove one host-recorded remote staging file only when the backend can
    /// prove it is a regular file owned by this transfer. Called only after the
    /// manager has validated the generated transfer-scoped basename. Backends
    /// must fail closed when there is no already-authenticated session.
    fn remove_recovered_staging<'a>(
        &'a self,
        _server_id: &'a str,
        _path: &'a str,
    ) -> TransferFuture<'a, ()> {
        Box::pin(async {
            Err(TransferError::Unsupported(
                "the backend cannot safely reconcile remote staging files".into(),
            ))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TransferId(pub String);

impl std::fmt::Display for TransferId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Host-only ownership marker for a staging file. Never add this value to a
/// [`TransferSnapshot`] or an IPC response: local paths are private to Rust.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "location", rename_all = "camelCase")]
pub enum TransferStagingFile {
    Local {
        path: PathBuf,
        item_index: u64,
    },
    Remote {
        server_id: String,
        path: String,
        item_index: u64,
    },
}

/// Storage seam used by the desktop host to persist bounded transfer snapshots
/// and its private recovery inventory. Implementations must keep staging data
/// out of all IPC-facing snapshot serialization.
pub trait TransferHistoryStore: Send + Sync + 'static {
    fn insert(
        &self,
        snapshot: &TransferSnapshot,
        staging: &[TransferStagingFile],
    ) -> Result<(), String>;

    fn update(
        &self,
        snapshot: &TransferSnapshot,
        staging: &[TransferStagingFile],
    ) -> Result<(), String>;

    fn get(&self, transfer_id: &TransferId) -> Result<Option<TransferSnapshot>, String>;

    fn list(&self, server_id: Option<&str>, limit: usize) -> Result<Vec<TransferSnapshot>, String>;

    /// Atomically transitions every nonterminal stored row to `interrupted` and
    /// returns its host-private staging inventory for safe cleanup attempts.
    fn recover(&self, now_epoch_ms: u64) -> Result<Vec<RecoveredTransfer>, String>;

    fn staging(&self, transfer_id: &TransferId) -> Result<Vec<TransferStagingFile>, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredTransfer {
    pub snapshot: TransferSnapshot,
    pub staging: Vec<TransferStagingFile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TransferDirection {
    Upload,
    Download,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TransferStatus {
    Queued,
    Running,
    WaitingConflict,
    Completed,
    Partial,
    Failed,
    Cancelled,
    Interrupted,
}

impl TransferStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Partial | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::WaitingConflict => "waitingConflict",
            Self::Completed => "completed",
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }
}

impl TransferDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Upload => "upload",
            Self::Download => "download",
        }
    }
}

/// Conflict handling chosen when a transfer is started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictPolicy {
    /// Pause on each collision until the user chooses an action.
    Ask,
    /// Keep the existing destination and count the item as skipped.
    Skip,
    /// Replace existing regular files after showing the conflict to the user at start time.
    Overwrite,
}

/// Per-conflict resolution for a transfer using [`ConflictPolicy::Ask`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum ConflictAction {
    Skip,
    Overwrite,
    Rename { name: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictActionKind {
    Skip,
    Overwrite,
    Rename,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictRequest {
    pub item_index: u64,
    pub source_name: String,
    pub target_name: String,
    pub existing_size: u64,
    pub incoming_size: u64,
    pub existing_modified_epoch_seconds: Option<u64>,
    pub allowed_actions: Vec<ConflictActionKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TransferFailureKind {
    InvalidInput,
    LocalIo,
    RemoteIo,
    Verification,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferItemFailure {
    pub item_index: Option<u64>,
    pub item: String,
    pub kind: TransferFailureKind,
    pub message: String,
    pub staging_residue: Option<String>,
}

/// Snapshot returned to the UI. It contains metadata and progress only, never file bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferSnapshot {
    pub transfer_id: TransferId,
    pub server_id: String,
    pub direction: TransferDirection,
    pub status: TransferStatus,
    pub started_at_epoch_ms: u64,
    pub updated_at_epoch_ms: u64,
    pub total_files: Option<u64>,
    pub completed_files: u64,
    pub skipped_files: u64,
    pub total_bytes: Option<u64>,
    pub transferred_bytes: u64,
    pub current_item: Option<String>,
    pub current_item_bytes: u64,
    pub current_item_total_bytes: Option<u64>,
    pub active_conflict: Option<ConflictRequest>,
    pub verified_files: u64,
    pub unverified_files: u64,
    pub failures: Vec<TransferItemFailure>,
    pub staging_residue: Vec<String>,
}

impl TransferSnapshot {
    pub(crate) fn new(
        transfer_id: TransferId,
        server_id: String,
        direction: TransferDirection,
    ) -> Self {
        let now = epoch_millis();
        Self {
            transfer_id,
            server_id,
            direction,
            status: TransferStatus::Queued,
            started_at_epoch_ms: now,
            updated_at_epoch_ms: now,
            total_files: None,
            completed_files: 0,
            skipped_files: 0,
            total_bytes: None,
            transferred_bytes: 0,
            current_item: None,
            current_item_bytes: 0,
            current_item_total_bytes: None,
            active_conflict: None,
            verified_files: 0,
            unverified_files: 0,
            failures: Vec::new(),
            staging_residue: Vec::new(),
        }
    }

    pub(crate) fn touch(&mut self) {
        self.updated_at_epoch_ms = epoch_millis();
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error("invalid transfer input: {0}")]
    InvalidInput(String),
    #[error("local file error: {0}")]
    Local(String),
    #[error("remote file error: {0}")]
    Remote(String),
    #[error("destination already exists")]
    AlreadyExists,
    #[error("transfer backend does not support this safe operation: {0}")]
    Unsupported(String),
    #[error("file changed while it was being transferred: {0}")]
    SourceChanged(String),
    #[error("content verification failed: {0}")]
    Verification(String),
    #[error("transfer cancelled")]
    Cancelled,
}

impl TransferError {
    pub(crate) fn failure_kind(&self) -> TransferFailureKind {
        match self {
            Self::InvalidInput(_) => TransferFailureKind::InvalidInput,
            Self::Local(_) | Self::SourceChanged(_) => TransferFailureKind::LocalIo,
            Self::Remote(_) | Self::AlreadyExists => TransferFailureKind::RemoteIo,
            Self::Unsupported(_) => TransferFailureKind::Unsupported,
            Self::Verification(_) => TransferFailureKind::Verification,
            Self::Cancelled => TransferFailureKind::RemoteIo,
        }
    }

    pub(crate) fn remote(message: impl Into<String>) -> Self {
        Self::Remote(message.into())
    }

    pub(crate) fn local(error: io::Error) -> Self {
        Self::Local(error.to_string())
    }
}

pub(crate) fn epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

pub(crate) fn safe_name(name: &str) -> Result<(), TransferError> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '\0'))
        || name.ends_with(' ')
        || name.ends_with('.')
    {
        return Err(TransferError::InvalidInput(format!(
            "file name cannot be transferred safely: {name:?}"
        )));
    }
    #[cfg(windows)]
    {
        let stem = name
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        if matches!(
            stem.as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        ) {
            return Err(TransferError::InvalidInput(format!(
                "file name is reserved on Windows: {name:?}"
            )));
        }
    }
    Ok(())
}

pub(crate) fn validate_remote_path(path: &str) -> Result<(), TransferError> {
    crate::policy::validate_remote_path(path).map_err(TransferError::InvalidInput)?;
    let segments = path.split('/').collect::<Vec<_>>();
    if path.contains('\\')
        || path.contains("//")
        || segments
            .iter()
            .any(|segment| *segment == "." || *segment == "..")
    {
        return Err(TransferError::InvalidInput(
            "remote paths cannot contain traversal segments or backslashes".to_string(),
        ));
    }
    Ok(())
}

/// A staging basename must include the owning transfer ID and numeric item/attempt
/// suffix. This check is used on restart before the host considers unlinking a path
/// read from its local database.
pub(super) fn is_owned_staging_name(name: &str, transfer_id: &TransferId) -> bool {
    let Some((target_prefix, suffix)) = name.rsplit_once(".yukinal-") else {
        return false;
    };
    if target_prefix.is_empty() {
        return false;
    }
    let Some(suffix) = suffix.strip_suffix(".part") else {
        return false;
    };
    let mut parts = suffix.split('-');
    let Some(owner) = parts.next() else {
        return false;
    };
    let Some(item_index) = parts.next() else {
        return false;
    };
    let Some(attempt) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && owner == transfer_id.0.replace('-', "")
        && !item_index.is_empty()
        && item_index.bytes().all(|byte| byte.is_ascii_digit())
        && !attempt.is_empty()
        && attempt.bytes().all(|byte| byte.is_ascii_digit())
}

pub(crate) fn remote_child(parent: &str, name: &str) -> Result<String, TransferError> {
    safe_name(name)?;
    validate_remote_path(parent)?;
    let child = if parent == "/" {
        format!("/{name}")
    } else if parent.ends_with('/') {
        format!("{parent}{name}")
    } else {
        format!("{parent}/{name}")
    };
    validate_remote_path(&child)?;
    Ok(child)
}
