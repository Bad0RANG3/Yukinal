use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rand::Rng as _;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;

const LOCAL_HANDLE_TTL_MS: u64 = 15 * 60 * 1_000;
const MAX_LOCAL_HANDLES: usize = 2_048;
const MAX_PICKED_PATHS: usize = 256;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalPathHandle {
    pub handle_id: String,
    pub name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

#[derive(Debug, Clone)]
struct LocalPathEntry {
    path: PathBuf,
    kind: LocalPathKind,
    expires_at_epoch_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalPathKind {
    File,
    Directory,
}

#[derive(Default)]
pub struct LocalPathHandles {
    entries: Mutex<HashMap<String, LocalPathEntry>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalFileDropEvent {
    handles: Vec<LocalPathHandle>,
    rejected_count: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalPathPickResponse {
    handles: Vec<LocalPathHandle>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LocalPathPickKind {
    UploadFiles,
    UploadDirectory,
    DownloadDirectory,
}

impl LocalPathHandles {
    pub(super) fn register_paths(
        &self,
        paths: impl IntoIterator<Item = PathBuf>,
    ) -> (Vec<LocalPathHandle>, u32) {
        let now = epoch_millis();
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        entries.retain(|_, entry| entry.expires_at_epoch_ms > now);
        if entries.len() >= MAX_LOCAL_HANDLES {
            return (Vec::new(), 1);
        }

        let mut accepted = Vec::new();
        let mut rejected = 0;
        for path in paths {
            if accepted.len() >= MAX_PICKED_PATHS || entries.len() >= MAX_LOCAL_HANDLES {
                rejected += 1;
                continue;
            }
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                rejected += 1;
                continue;
            };
            let (kind, kind_name, size) = if metadata.file_type().is_symlink() {
                rejected += 1;
                continue;
            } else if metadata.is_file() {
                (LocalPathKind::File, "file", Some(metadata.len()))
            } else if metadata.is_dir() {
                (LocalPathKind::Directory, "directory", None)
            } else {
                rejected += 1;
                continue;
            };
            let name = path
                .file_name()
                .map(|value| value.to_string_lossy().into_owned())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| {
                    if metadata.is_dir() {
                        "根目录".to_string()
                    } else {
                        "未命名文件".to_string()
                    }
                });
            let handle_id = new_handle_id();
            entries.insert(
                handle_id.clone(),
                LocalPathEntry {
                    path,
                    kind,
                    expires_at_epoch_ms: now.saturating_add(LOCAL_HANDLE_TTL_MS),
                },
            );
            accepted.push(LocalPathHandle {
                handle_id,
                name,
                kind: kind_name.to_string(),
                size,
            });
        }
        (accepted, rejected)
    }

    pub fn take_upload_sources(&self, handle_ids: &[String]) -> Result<Vec<PathBuf>, String> {
        if handle_ids.is_empty() || handle_ids.len() > MAX_PICKED_PATHS {
            return Err("select between 1 and 256 local files or folders".to_string());
        }
        let now = epoch_millis();
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        entries.retain(|_, entry| entry.expires_at_epoch_ms > now);
        let mut unique = HashSet::new();
        let mut paths = Vec::with_capacity(handle_ids.len());
        for handle_id in handle_ids {
            if !unique.insert(handle_id) {
                return Err("a local file handle was repeated".to_string());
            }
            let entry = entries
                .get(handle_id)
                .ok_or_else(|| "local file handle expired or was not found".to_string())?;
            let metadata =
                std::fs::symlink_metadata(&entry.path).map_err(|error| error.to_string())?;
            if metadata.file_type().is_symlink()
                || (entry.kind == LocalPathKind::File && !metadata.is_file())
                || (entry.kind == LocalPathKind::Directory && !metadata.is_dir())
            {
                return Err("selected local path changed type; select it again".to_string());
            }
            paths.push(entry.path.clone());
        }
        for handle_id in handle_ids {
            entries.remove(handle_id);
        }
        Ok(paths)
    }

    pub fn take_download_directory(&self, handle_id: &str) -> Result<PathBuf, String> {
        let now = epoch_millis();
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        entries.retain(|_, entry| entry.expires_at_epoch_ms > now);
        let entry = entries
            .get(handle_id)
            .ok_or_else(|| "local destination handle expired or was not found".to_string())?;
        let metadata = std::fs::symlink_metadata(&entry.path).map_err(|error| error.to_string())?;
        if entry.kind != LocalPathKind::Directory
            || metadata.file_type().is_symlink()
            || !metadata.is_dir()
        {
            return Err("selected download destination is no longer a real directory".to_string());
        }
        // A chosen destination remains reusable until its short TTL expires. The UI keeps this
        // opaque handle selected so successive drops can go to the same folder; every transfer
        // still revalidates the directory and opens its own temporary child files safely.
        Ok(entry.path.clone())
    }

    pub(super) fn preview_target(&self, handle_id: &str) -> Result<(PathBuf, String, u64), String> {
        let now = epoch_millis();
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        entries.retain(|_, entry| entry.expires_at_epoch_ms > now);
        let entry = entries
            .get(handle_id)
            .ok_or_else(|| "local file handle expired or was not found".to_string())?;
        if entry.kind != LocalPathKind::File {
            return Err("only regular files can be previewed".to_string());
        }
        let metadata = std::fs::symlink_metadata(&entry.path).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err("selected local file is no longer a regular file".to_string());
        }
        let name = entry
            .path
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| "未命名文件".to_string());
        Ok((entry.path.clone(), name, metadata.len()))
    }

    pub(super) async fn read_preview(
        &self,
        handle_id: &str,
        max_bytes: usize,
    ) -> Result<(String, u64, Vec<u8>), String> {
        use tokio::io::AsyncReadExt as _;

        let (path, name, size) = self.preview_target(handle_id)?;
        let file = open_regular_file_nofollow(&path).map_err(|error| error.to_string())?;
        let file_metadata = file.metadata().map_err(|error| error.to_string())?;
        if !file_metadata.is_file() {
            return Err("selected local file is no longer a regular file".to_string());
        }
        let file = tokio::fs::File::from_std(file);
        let mut bytes = Vec::new();
        file.take(max_bytes.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| error.to_string())?;
        Ok((name, size, bytes))
    }
}

fn open_regular_file_nofollow(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let file = options.open(path)?;
        if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "reparse points cannot be previewed",
            ));
        }
        Ok(file)
    }
    #[cfg(not(windows))]
    {
        options.open(path)
    }
}

fn epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn new_handle_id() -> String {
    let mut bytes = [0_u8; 24];
    rand::rng().fill_bytes(&mut bytes);
    let mut id = String::with_capacity(12 + bytes.len() * 2);
    id.push_str("local_path_");
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(id, "{byte:02x}");
    }
    id
}

pub(crate) fn emit_local_drop(app: &AppHandle, paths: Vec<PathBuf>) {
    let (handles, rejected_count) = app.state::<LocalPathHandles>().register_paths(paths);
    let payload = LocalFileDropEvent {
        handles,
        rejected_count,
    };
    let _ = app.emit(
        &crate::commands::tauri_event_name("file.local_dropped"),
        payload,
    );
}

#[tauri::command]
pub async fn local_path_pick(
    app: AppHandle,
    registry: State<'_, LocalPathHandles>,
    kind: LocalPathPickKind,
) -> Result<LocalPathPickResponse, String> {
    let paths = match kind {
        LocalPathPickKind::UploadFiles => app
            .dialog()
            .file()
            .blocking_pick_files()
            .unwrap_or_default()
            .into_iter()
            .map(|path| path.into_path().map_err(|error| error.to_string()))
            .collect::<Result<Vec<_>, _>>()?,
        LocalPathPickKind::UploadDirectory | LocalPathPickKind::DownloadDirectory => app
            .dialog()
            .file()
            .blocking_pick_folder()
            .map(|path| path.into_path().map_err(|error| error.to_string()))
            .transpose()?
            .into_iter()
            .collect(),
    };
    let (handles, rejected) = registry.register_paths(paths);
    if rejected > 0 && handles.is_empty() {
        return Err(
            "the selected paths could not be opened; select regular files or folders".to_string(),
        );
    }
    Ok(LocalPathPickResponse { handles })
}
