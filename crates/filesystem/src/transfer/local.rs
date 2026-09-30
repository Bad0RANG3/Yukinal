use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use tokio::fs::{self, File, OpenOptions};

use super::types::{is_owned_staging_name, TransferError, TransferId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LocalFingerprint {
    pub size: u64,
    pub modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl LocalFingerprint {
    pub fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        Self {
            size: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        }
    }
}

pub(super) async fn source_fingerprint(path: &Path) -> Result<LocalFingerprint, TransferError> {
    let metadata = fs::symlink_metadata(path)
        .await
        .map_err(TransferError::local)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(TransferError::InvalidInput(
            "only regular selected files can be transferred".to_string(),
        ));
    }
    Ok(LocalFingerprint::from_metadata(&metadata))
}

pub(super) async fn open_source(path: &Path) -> Result<(File, LocalFingerprint), TransferError> {
    let before = source_fingerprint(path).await?;
    let std_file = open_regular_file_nofollow(path).map_err(TransferError::local)?;
    let handle_metadata = std_file.metadata().map_err(TransferError::local)?;
    if !handle_metadata.is_file() || LocalFingerprint::from_metadata(&handle_metadata) != before {
        return Err(TransferError::SourceChanged(
            "selected local source changed before it was opened".to_string(),
        ));
    }
    Ok((File::from_std(std_file), before))
}

pub(super) async fn verify_source_unchanged(
    path: &Path,
    file: &File,
    initial: &LocalFingerprint,
) -> Result<(), TransferError> {
    let current_handle = file.metadata().await.map_err(TransferError::local)?;
    let current_path = fs::symlink_metadata(path)
        .await
        .map_err(TransferError::local)?;
    if current_path.file_type().is_symlink()
        || !current_path.is_file()
        || LocalFingerprint::from_metadata(&current_handle) != *initial
        || LocalFingerprint::from_metadata(&current_path) != *initial
    {
        return Err(TransferError::SourceChanged(
            "selected local source changed while it was being uploaded".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn open_regular_file_nofollow(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        // FILE_FLAG_OPEN_REPARSE_POINT opens the link itself instead of its target. Inspect the
        // handle metadata below and reject reparse points before reading any bytes.
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let file = options.open(path)?;
        if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "reparse points cannot be transferred",
            ));
        }
        Ok(file)
    }
    #[cfg(not(windows))]
    options.open(path)
}

pub(super) async fn read_directory(path: &Path) -> Result<Vec<PathBuf>, TransferError> {
    let metadata = fs::symlink_metadata(path)
        .await
        .map_err(TransferError::local)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(TransferError::InvalidInput(
            "only real directories can be traversed".to_string(),
        ));
    }
    let mut directory = fs::read_dir(path).await.map_err(TransferError::local)?;
    let mut paths = Vec::new();
    while let Some(entry) = directory.next_entry().await.map_err(TransferError::local)? {
        paths.push(entry.path());
    }
    paths.sort();
    Ok(paths)
}

pub(super) async fn create_staging_file(
    target: &Path,
    transfer_id: &str,
    item_index: u64,
) -> Result<(PathBuf, File), TransferError> {
    let parent = target.parent().ok_or_else(|| {
        TransferError::InvalidInput("download destination has no parent directory".to_string())
    })?;
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            TransferError::InvalidInput(
                "download destination file name is not valid UTF-8".to_string(),
            )
        })?;
    for attempt in 0..32_u32 {
        let staging = parent.join(format!(
            ".{name}.yukinal-{}-{item_index}-{attempt}.part",
            transfer_id.replace('-', "")
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        set_no_follow(&mut options);
        match options.open(&staging).await {
            Ok(file) => return Ok((staging, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(TransferError::local(error)),
        }
    }
    Err(TransferError::Unsupported(
        "could not allocate a unique local staging file after 32 attempts".to_string(),
    ))
}

pub(super) async fn publish_local_no_replace(
    staging: &Path,
    destination: &Path,
) -> Result<(), TransferError> {
    // A same-directory hard link is an atomic no-replace publication: it fails if another entry
    // appeared at the destination after the last lstat. The staging link is removed only after
    // publication succeeds.
    fs::hard_link(staging, destination).await.map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            TransferError::InvalidInput("destination appeared during publication".to_string())
        } else {
            TransferError::Unsupported(format!(
                "this filesystem cannot publish a download without replacement: {error}"
            ))
        }
    })?;
    fs::remove_file(staging).await.map_err(TransferError::local)
}

pub(super) async fn publish_local_replace(
    staging: &Path,
    destination: &Path,
    expected: &LocalFingerprint,
) -> Result<(), TransferError> {
    let current = fs::symlink_metadata(destination)
        .await
        .map_err(TransferError::local)?;
    if current.file_type().is_symlink() || !current.is_file() {
        return Err(TransferError::InvalidInput(
            "overwrite target is no longer a regular file".to_string(),
        ));
    }
    if LocalFingerprint::from_metadata(&current) != *expected {
        return Err(TransferError::InvalidInput(
            "overwrite target changed while the transfer was staged".to_string(),
        ));
    }
    // This is an atomic same-directory replace on the supported desktop filesystems. There is no
    // compare-and-swap primitive, so an external writer can still race the check above.
    fs::rename(staging, destination)
        .await
        .map_err(TransferError::local)
}

pub(super) async fn remove_staging(path: &Path) -> Result<(), TransferError> {
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(TransferError::local(error)),
    }
}

/// Restart recovery only deletes a regular file in a real parent directory when
/// its basename carries the exact owner transfer ID and generated numeric suffix.
/// It never follows or removes a symlink/reparse point and treats an absent stage
/// as already cleaned (for example, publication completed just before the crash).
pub(super) fn remove_recovered_staging(
    path: &Path,
    transfer_id: &TransferId,
) -> Result<(), TransferError> {
    if !path.is_absolute()
        || !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| is_owned_staging_name(name, transfer_id))
    {
        return Err(TransferError::InvalidInput(
            "recovered staging path is not owned by this transfer".into(),
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        TransferError::InvalidInput("recovered staging file has no parent".into())
    })?;
    let parent_metadata = std::fs::symlink_metadata(parent).map_err(TransferError::local)?;
    if is_link_or_reparse(&parent_metadata) || !parent_metadata.is_dir() {
        return Err(TransferError::InvalidInput(
            "recovered staging parent is not a real directory".into(),
        ));
    }
    let file_metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(TransferError::local(error)),
    };
    if is_link_or_reparse(&file_metadata) || !file_metadata.is_file() {
        return Err(TransferError::InvalidInput(
            "recovered staging entry is not a regular file".into(),
        ));
    }
    std::fs::remove_file(path).map_err(TransferError::local)
}

fn is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub(super) async fn ensure_download_directory(path: &Path) -> Result<(), TransferError> {
    match fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => Err(
            TransferError::InvalidInput("download destination is not a real directory".to_string()),
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(TransferError::InvalidInput(
            "download destination does not exist".to_string(),
        )),
        Err(error) => Err(TransferError::local(error)),
    }
}

pub(super) async fn path_fingerprint_if_regular(
    path: &Path,
) -> Result<Option<LocalFingerprint>, TransferError> {
    match fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(TransferError::InvalidInput(
            "destination is a symbolic link or reparse point".to_string(),
        )),
        Ok(metadata) if !metadata.is_file() => Err(TransferError::InvalidInput(
            "destination already exists and is not a regular file".to_string(),
        )),
        Ok(metadata) => Ok(Some(LocalFingerprint::from_metadata(&metadata))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(TransferError::local(error)),
    }
}

fn set_no_follow(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
}
