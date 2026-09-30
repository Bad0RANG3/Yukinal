//! Native drag-out for remote files.
//!
//! Remote bytes are first downloaded into a host-private, short-lived staging directory. The UI
//! receives only an opaque ticket; absolute local paths never cross IPC. Starting the native drag
//! consumes the ticket and keeps the staging directory alive until the operating system reports
//! that the drag ended.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rand::Rng as _;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tokio::sync::oneshot;
use tokio::sync::Semaphore;
use yukinal_filesystem::transfer::{ConflictPolicy, TransferStatus};
use yukinal_ssh::{SftpEntryKind, SshBackend};

use crate::commands::files::transfer::FileTransferState;
use crate::commands::terminal::ensure_session;
use crate::commands::EmptyResponse;
use crate::state::AppState;

const DRAG_TICKET_TTL: Duration = Duration::from_secs(5 * 60);
const DRAG_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_DRAG_FILE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_PREPARED_DRAG_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_PARALLEL_PREPARATIONS: usize = 2;
const DRAG_DIRECTORY: &str = "native-remote-drag";
const DRAG_DIR_PREFIX: &str = "yukinal-remote-drag-";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedRemoteDragResponse {
    drag_id: String,
    name: String,
    size: u64,
}

#[derive(Clone)]
pub struct NativeRemoteDrags {
    root: Option<PathBuf>,
    prepared: Arc<Mutex<HashMap<String, PreparedDrag>>>,
    preparation_slots: Arc<Semaphore>,
}

struct PreparedDrag {
    _directory: tempfile::TempDir,
    files: Vec<PathBuf>,
    size: u64,
    expires_at_epoch_ms: u64,
}

impl NativeRemoteDrags {
    pub fn open(app: &AppHandle) -> Result<Self, String> {
        let root = app
            .path()
            .app_cache_dir()
            .map_err(|error| error.to_string())?
            .join(DRAG_DIRECTORY);
        fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let metadata = fs::symlink_metadata(&root).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err("native drag staging location is not a private directory".to_string());
        }
        remove_stale_drag_directories(&root)?;
        Ok(Self {
            root: Some(root),
            prepared: Arc::default(),
            preparation_slots: Arc::new(Semaphore::new(MAX_PARALLEL_PREPARATIONS)),
        })
    }

    fn create_stage(&self) -> Result<tempfile::TempDir, String> {
        let root = self
            .root
            .as_deref()
            .ok_or_else(|| "native drag staging is unavailable".to_string())?;
        tempfile::Builder::new()
            .prefix(DRAG_DIR_PREFIX)
            .tempdir_in(root)
            .map_err(|error| error.to_string())
    }

    fn insert(&self, drag: PreparedDrag) -> Result<String, String> {
        let now = epoch_millis();
        let mut prepared = self
            .prepared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        prepared.retain(|_, item| item.expires_at_epoch_ms > now);
        if prepared.len() >= 64 {
            return Err(
                "too many remote files are prepared for drag-out; retry shortly".to_string(),
            );
        }
        let prepared_bytes = prepared
            .values()
            .fold(0_u64, |total, item| total.saturating_add(item.size));
        if prepared_bytes.saturating_add(drag.size) > MAX_PREPARED_DRAG_BYTES {
            return Err("prepared remote drag files exceed the 16 GiB staging limit".to_string());
        }
        let drag_id = new_drag_id();
        prepared.insert(drag_id.clone(), drag);
        Ok(drag_id)
    }

    fn take(&self, drag_id: &str) -> Result<PreparedDrag, String> {
        let now = epoch_millis();
        let mut prepared = self
            .prepared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        prepared.retain(|_, item| item.expires_at_epoch_ms > now);
        prepared
            .remove(drag_id)
            .ok_or_else(|| "remote drag ticket expired or was already used".to_string())
    }

    fn discard_expired(prepared: &Mutex<HashMap<String, PreparedDrag>>) {
        let now = epoch_millis();
        prepared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|_, item| item.expires_at_epoch_ms > now);
    }
}

pub(crate) fn start_cleanup_loop(app: AppHandle) {
    let prepared = app.state::<NativeRemoteDrags>().prepared.clone();
    let shutdown = app.state::<AppState>().shutdown.clone();
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => break,
                _ = interval.tick() => NativeRemoteDrags::discard_expired(&prepared),
            }
        }
    });
}

#[tauri::command]
pub async fn file_prepare_remote_drag(
    state: State<'_, AppState>,
    transfers: State<'_, FileTransferState>,
    drags: State<'_, NativeRemoteDrags>,
    server_id: String,
    path: String,
) -> Result<PreparedRemoteDragResponse, String> {
    validate_remote_file_path(&path)?;
    let _preparation = drags
        .preparation_slots
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| "native drag preparation is shutting down".to_string())?;
    ensure_session(&state, &server_id).await?;
    let session = state
        .terminals
        .cached_session(&server_id)
        .map_err(|error| error.to_string())?;
    let client = state
        .ssh
        .sftp(&session)
        .await
        .map_err(|error| error.to_string())?;
    let stat = state
        .ssh
        .sftp_lstat_optional(&client, &path)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "remote file no longer exists".to_string())?;
    drop(session);
    if stat.kind != SftpEntryKind::File {
        return Err("only regular remote files can be dragged to the desktop".to_string());
    }
    if stat.size > MAX_DRAG_FILE_BYTES {
        return Err(
            "remote files larger than 8 GiB cannot be prepared for native drag-out".to_string(),
        );
    }

    let name = remote_file_name(&path)?;
    let directory = drags.create_stage()?;
    let transfer_id = transfers
        .start_download(
            &server_id,
            vec![path],
            directory.path().to_path_buf(),
            ConflictPolicy::Ask,
        )
        .map_err(|error| error.to_string())?;
    let snapshot = match transfers
        .wait_for_terminal(&transfer_id, DRAG_DOWNLOAD_TIMEOUT)
        .await
    {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let _ = transfers.cancel(&transfer_id);
            if transfers
                .wait_for_terminal(&transfer_id, Duration::from_secs(60))
                .await
                .is_err()
            {
                // A hung remote backend must not race `TempDir` cleanup. Leave this private
                // directory for the next startup sweep if cancellation cannot be observed.
                let _ = directory.keep();
            }
            return Err(error);
        }
    };
    if snapshot.status != TransferStatus::Completed || snapshot.completed_files != 1 {
        let detail = snapshot
            .failures
            .first()
            .map(|failure| failure.message.as_str())
            .unwrap_or("download did not complete successfully");
        return Err(format!(
            "could not prepare remote file for native drag: {detail}"
        ));
    }

    let files = collect_staged_regular_files(&directory)?;
    if files.len() != 1 {
        return Err("remote download did not produce exactly one regular file".to_string());
    }
    let staged_file = &files[0];
    let metadata = fs::symlink_metadata(staged_file).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() != stat.size {
        return Err("staged remote file failed its local type or size check".to_string());
    }
    let canonical_root = fs::canonicalize(directory.path()).map_err(|error| error.to_string())?;
    let canonical_file = fs::canonicalize(staged_file).map_err(|error| error.to_string())?;
    if !canonical_file.starts_with(&canonical_root) {
        return Err("staged file escaped the private drag directory".to_string());
    }

    let size = metadata.len();
    let files = vec![canonical_file];
    let drag_id = drags.insert(PreparedDrag {
        _directory: directory,
        files,
        size,
        expires_at_epoch_ms: epoch_millis().saturating_add(DRAG_TICKET_TTL.as_millis() as u64),
    })?;
    Ok(PreparedRemoteDragResponse {
        drag_id,
        name,
        size,
    })
}

#[tauri::command]
pub async fn file_drag_out_start(
    app: AppHandle,
    drags: State<'_, NativeRemoteDrags>,
    drag_id: String,
) -> Result<EmptyResponse, String> {
    if drag_id.len() != 76 || !drag_id.starts_with("remote_drag_") {
        return Err("invalid remote drag ticket".to_string());
    }
    let prepared = drags.take(&drag_id)?;
    let files = prepared.files.clone();
    let (result_tx, result_rx) = oneshot::channel();
    let app_for_main = app.clone();
    app.run_on_main_thread(move || {
        let Some(window) = app_for_main.get_webview_window("main") else {
            let _ = result_tx.send(Err("main application window is not available".to_string()));
            return;
        };
        let keep_alive = Mutex::new(Some(prepared));
        let result = start_native_drag(&window, files, move |_, _| {
            if let Ok(mut item) = keep_alive.lock() {
                item.take();
            }
        });
        let _ = result_tx.send(result);
    })
    .map_err(|error| error.to_string())?;
    result_rx
        .await
        .map_err(|_| "native drag could not be started".to_string())??;
    Ok(EmptyResponse {})
}

fn start_native_drag<F>(
    window: &tauri::WebviewWindow,
    files: Vec<PathBuf>,
    on_drop: F,
) -> Result<(), String>
where
    F: Fn(drag::DragResult, drag::CursorPosition) + Send + 'static,
{
    #[cfg(target_os = "linux")]
    let native_window = window.gtk_window().map_err(|error| error.to_string())?;
    #[cfg(not(target_os = "linux"))]
    let native_window = window;

    drag::start_drag(
        &native_window,
        drag::DragItem::Files(files),
        drag::Image::Raw(include_bytes!("../../../icons/icon.png").to_vec()),
        on_drop,
        drag::Options::default(),
    )
    .map_err(|error| error.to_string())
}

fn collect_staged_regular_files(directory: &tempfile::TempDir) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for entry in fs::read_dir(directory.path()).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let metadata = fs::symlink_metadata(entry.path()).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() || metadata.is_dir() {
            return Err("staged download contains a directory or symbolic link".to_string());
        }
        if metadata.is_file() {
            files.push(entry.path());
        }
    }
    Ok(files)
}

fn validate_remote_file_path(path: &str) -> Result<(), String> {
    if !path.starts_with('/')
        || path.len() > 4096
        || path.contains('\\')
        || path.contains('\0')
        || path.split('/').any(|segment| matches!(segment, "." | ".."))
        || path.ends_with('/')
    {
        return Err("remote drag path must be a safe absolute file path".to_string());
    }
    Ok(())
}

fn remote_file_name(path: &str) -> Result<String, String> {
    path.rsplit('/')
        .next()
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .map(ToOwned::to_owned)
        .ok_or_else(|| "remote file path has no name".to_string())
}

fn remove_stale_drag_directories(root: &Path) -> Result<(), String> {
    for entry in fs::read_dir(root).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(DRAG_DIR_PREFIX)
        {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path()).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() {
            fs::remove_file(entry.path()).map_err(|error| error.to_string())?;
        } else if metadata.is_dir() {
            fs::remove_dir_all(entry.path()).map_err(|error| error.to_string())?;
        } else {
            fs::remove_file(entry.path()).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn new_drag_id() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let mut id = String::with_capacity(76);
    id.push_str("remote_drag_");
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(id, "{byte:02x}");
    }
    id
}

fn epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::{remote_file_name, validate_remote_file_path, PreparedRemoteDragResponse};
    use crate::commands::EmptyResponse;

    #[test]
    fn only_safe_absolute_remote_file_paths_are_accepted() {
        for path in [
            "relative.txt",
            "/srv/../secret",
            "/srv/./file",
            "/srv\\secret",
            "/srv/",
        ] {
            assert!(
                validate_remote_file_path(path).is_err(),
                "accepted {path:?}"
            );
        }
        assert!(validate_remote_file_path("/srv/app/config.yml").is_ok());
        assert_eq!(
            remote_file_name("/srv/app/config.yml").unwrap(),
            "config.yml"
        );
    }

    #[test]
    fn native_drag_responses_match_the_shared_ipc_fixtures() {
        let prepared = serde_json::to_value(PreparedRemoteDragResponse {
            drag_id: format!("remote_drag_{}", "0".repeat(64)),
            name: "docker-compose.yml".into(),
            size: 135,
        })
        .expect("serialize prepared drag response");
        let prepared_fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../../packages/shared/fixtures/ipc/file_prepare_remote_drag.json"
        ))
        .expect("prepared drag fixture");
        assert_eq!(prepared, prepared_fixture);

        let empty = serde_json::to_value(EmptyResponse {}).expect("serialize empty response");
        let empty_fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../../packages/shared/fixtures/ipc/file_drag_out_start.json"
        ))
        .expect("drag-start fixture");
        assert_eq!(empty, empty_fixture);
    }
}
