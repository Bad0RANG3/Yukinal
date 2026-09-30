use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};

use super::types::{
    ConflictAction, ConflictActionKind, ConflictPolicy, RecoveredTransfer, RemoteTransferBackend,
    RemoteTransferEntry, TransferDirection, TransferError, TransferFuture, TransferHistoryStore,
    TransferId, TransferReader, TransferSnapshot, TransferStagingFile, TransferStatus,
    TransferWriter,
};
use super::TransferManager;
use crate::service::{RemoteEntryKind, RemoteStat};

type TransferHistoryMap = HashMap<String, (TransferSnapshot, Vec<TransferStagingFile>)>;

#[derive(Clone, Default)]
struct MemoryTransferHistory {
    rows: Arc<Mutex<TransferHistoryMap>>,
}

impl TransferHistoryStore for MemoryTransferHistory {
    fn insert(
        &self,
        snapshot: &TransferSnapshot,
        staging: &[TransferStagingFile],
    ) -> Result<(), String> {
        let mut rows = self.rows.lock().map_err(|error| error.to_string())?;
        if rows
            .insert(
                snapshot.transfer_id.0.clone(),
                (snapshot.clone(), staging.to_vec()),
            )
            .is_some()
        {
            return Err("duplicate transfer".into());
        }
        Ok(())
    }

    fn update(
        &self,
        snapshot: &TransferSnapshot,
        staging: &[TransferStagingFile],
    ) -> Result<(), String> {
        self.rows.lock().map_err(|error| error.to_string())?.insert(
            snapshot.transfer_id.0.clone(),
            (snapshot.clone(), staging.to_vec()),
        );
        Ok(())
    }

    fn get(&self, transfer_id: &TransferId) -> Result<Option<TransferSnapshot>, String> {
        Ok(self
            .rows
            .lock()
            .map_err(|error| error.to_string())?
            .get(&transfer_id.0)
            .map(|(snapshot, _)| snapshot.clone()))
    }

    fn list(&self, server_id: Option<&str>, limit: usize) -> Result<Vec<TransferSnapshot>, String> {
        let rows = self.rows.lock().map_err(|error| error.to_string())?;
        let mut snapshots = rows
            .values()
            .map(|(snapshot, _)| snapshot.clone())
            .filter(|snapshot| server_id.is_none_or(|server| snapshot.server_id == server))
            .collect::<Vec<_>>();
        snapshots.sort_by_key(|snapshot| std::cmp::Reverse(snapshot.updated_at_epoch_ms));
        snapshots.truncate(limit);
        Ok(snapshots)
    }

    fn recover(&self, now_epoch_ms: u64) -> Result<Vec<RecoveredTransfer>, String> {
        let mut rows = self.rows.lock().map_err(|error| error.to_string())?;
        let mut recovered = Vec::new();
        for (snapshot, staging) in rows.values_mut() {
            if !snapshot.status.is_terminal() {
                snapshot.status = TransferStatus::Interrupted;
                snapshot.active_conflict = None;
                snapshot.current_item = None;
                snapshot.current_item_bytes = 0;
                snapshot.current_item_total_bytes = None;
                snapshot.updated_at_epoch_ms = now_epoch_ms;
            }
            if !staging.is_empty() {
                recovered.push(RecoveredTransfer {
                    snapshot: snapshot.clone(),
                    staging: staging.clone(),
                });
            }
        }
        Ok(recovered)
    }

    fn staging(&self, transfer_id: &TransferId) -> Result<Vec<TransferStagingFile>, String> {
        Ok(self
            .rows
            .lock()
            .map_err(|error| error.to_string())?
            .get(&transfer_id.0)
            .map(|(_, staging)| staging.clone())
            .unwrap_or_default())
    }
}

#[derive(Debug, Clone)]
struct Node {
    kind: RemoteEntryKind,
    bytes: Vec<u8>,
}

#[derive(Clone)]
struct FakeRemote {
    nodes: Arc<Mutex<HashMap<String, Node>>>,
    slow_reads: Arc<AtomicBool>,
    fail_upload_stage_cleanup: bool,
    failed_stage_cleanups: Arc<Mutex<HashSet<String>>>,
    publish_race: Option<(String, Vec<u8>)>,
    publish_unsupported: bool,
    atomic_replace: bool,
}

impl FakeRemote {
    fn new(atomic_replace: bool) -> Self {
        Self {
            nodes: Arc::new(Mutex::new(HashMap::from([(
                "/".to_string(),
                Node {
                    kind: RemoteEntryKind::Directory,
                    bytes: Vec::new(),
                },
            )]))),
            slow_reads: Arc::new(AtomicBool::new(false)),
            fail_upload_stage_cleanup: false,
            failed_stage_cleanups: Arc::new(Mutex::new(HashSet::new())),
            publish_race: None,
            publish_unsupported: false,
            atomic_replace,
        }
    }

    fn insert(&self, path: &str, kind: RemoteEntryKind, bytes: Vec<u8>) {
        self.nodes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(path.to_string(), Node { kind, bytes });
    }

    fn contents(&self, path: &str) -> Option<Vec<u8>> {
        self.nodes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(path)
            .filter(|node| node.kind == RemoteEntryKind::File)
            .map(|node| node.bytes.clone())
    }

    fn fail_upload_stage_cleanup(&mut self) {
        self.fail_upload_stage_cleanup = true;
    }

    fn race_target_on_publish(&mut self, path: &str, bytes: &[u8]) {
        self.publish_race = Some((path.to_string(), bytes.to_vec()));
    }

    fn refuse_publish(&mut self) {
        self.publish_unsupported = true;
    }
}

impl RemoteTransferBackend for FakeRemote {
    fn supports_atomic_replace(&self) -> bool {
        self.atomic_replace
    }

    fn lstat<'a>(
        &'a self,
        _server_id: &'a str,
        path: &'a str,
    ) -> TransferFuture<'a, Option<RemoteStat>> {
        let nodes = Arc::clone(&self.nodes);
        let path = path.to_string();
        Box::pin(async move {
            let nodes = nodes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            Ok(nodes.get(&path).map(|node| RemoteStat {
                kind: node.kind,
                size: node.bytes.len() as u64,
                modified: Some(1),
            }))
        })
    }

    fn list_dir<'a>(
        &'a self,
        _server_id: &'a str,
        path: &'a str,
    ) -> TransferFuture<'a, Vec<RemoteTransferEntry>> {
        let nodes = Arc::clone(&self.nodes);
        let path = path.trim_end_matches('/').to_string();
        Box::pin(async move {
            let nodes = nodes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let prefix = if path.is_empty() {
                "/".to_string()
            } else {
                format!("{path}/")
            };
            let mut entries = nodes
                .iter()
                .filter_map(|(candidate, node)| {
                    let remaining = candidate.strip_prefix(&prefix)?;
                    if remaining.is_empty() || remaining.contains('/') {
                        return None;
                    }
                    Some(RemoteTransferEntry {
                        name: remaining.to_string(),
                        stat: RemoteStat {
                            kind: node.kind,
                            size: node.bytes.len() as u64,
                            modified: Some(1),
                        },
                    })
                })
                .collect::<Vec<_>>();
            entries.sort_by(|left, right| left.name.cmp(&right.name));
            Ok(entries)
        })
    }

    fn open_read<'a>(
        &'a self,
        _server_id: &'a str,
        path: &'a str,
    ) -> TransferFuture<'a, TransferReader> {
        let nodes = Arc::clone(&self.nodes);
        let slow = Arc::clone(&self.slow_reads);
        let path = path.to_string();
        Box::pin(async move {
            let bytes = nodes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&path)
                .filter(|node| node.kind == RemoteEntryKind::File)
                .map(|node| node.bytes.clone())
                .ok_or_else(|| TransferError::Remote(format!("file not found: {path}")))?;
            if slow.load(Ordering::Relaxed) {
                let (reader, mut writer) = tokio::io::duplex(32 * 1024);
                tokio::spawn(async move {
                    for chunk in bytes.chunks(32 * 1024) {
                        if writer.write_all(chunk).await.is_err() {
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }
                });
                Ok(Box::new(reader) as TransferReader)
            } else {
                Ok(Box::new(CursorReader(Cursor::new(bytes))) as TransferReader)
            }
        })
    }

    fn create_exclusive<'a>(
        &'a self,
        _server_id: &'a str,
        path: &'a str,
    ) -> TransferFuture<'a, TransferWriter> {
        let nodes = Arc::clone(&self.nodes);
        let path = path.to_string();
        Box::pin(async move {
            let mut state = nodes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.contains_key(&path) {
                return Err(TransferError::AlreadyExists);
            }
            state.insert(
                path.clone(),
                Node {
                    kind: RemoteEntryKind::File,
                    bytes: Vec::new(),
                },
            );
            drop(state);
            Ok(Box::new(MemoryWriter { nodes, path }) as TransferWriter)
        })
    }

    fn create_dir<'a>(&'a self, _server_id: &'a str, path: &'a str) -> TransferFuture<'a, ()> {
        let nodes = Arc::clone(&self.nodes);
        let path = path.to_string();
        Box::pin(async move {
            let mut nodes = nodes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if nodes.contains_key(&path) {
                return Err(TransferError::AlreadyExists);
            }
            nodes.insert(
                path,
                Node {
                    kind: RemoteEntryKind::Directory,
                    bytes: Vec::new(),
                },
            );
            Ok(())
        })
    }

    fn remove_file<'a>(&'a self, _server_id: &'a str, path: &'a str) -> TransferFuture<'a, ()> {
        let nodes = Arc::clone(&self.nodes);
        let failed_stage_cleanups = Arc::clone(&self.failed_stage_cleanups);
        let path = path.to_string();
        Box::pin(async move {
            if failed_stage_cleanups
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .contains(&path)
            {
                return Err(TransferError::Remote(
                    "test backend refused staging cleanup".into(),
                ));
            }
            let mut nodes = nodes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match nodes.remove(&path) {
                Some(_) => Ok(()),
                None => Err(TransferError::Remote("staging file disappeared".into())),
            }
        })
    }

    fn publish_no_replace<'a>(
        &'a self,
        _server_id: &'a str,
        source: &'a str,
        destination: &'a str,
    ) -> TransferFuture<'a, ()> {
        let nodes = Arc::clone(&self.nodes);
        let failed_stage_cleanups = Arc::clone(&self.failed_stage_cleanups);
        let fail_upload_stage_cleanup = self.fail_upload_stage_cleanup;
        let publish_race = self.publish_race.clone();
        let publish_unsupported = self.publish_unsupported;
        let source = source.to_string();
        let destination = destination.to_string();
        Box::pin(async move {
            if publish_unsupported {
                return Err(TransferError::Unsupported(
                    "the server has no negotiated no-replace publication capability".into(),
                ));
            }
            let mut nodes = nodes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some((race_path, race_bytes)) = publish_race {
                if race_path == destination {
                    nodes.entry(race_path).or_insert(Node {
                        kind: RemoteEntryKind::File,
                        bytes: race_bytes,
                    });
                }
            }
            if nodes.contains_key(&destination) {
                return Err(TransferError::AlreadyExists);
            }
            let node = nodes
                .get(&source)
                .cloned()
                .ok_or_else(|| TransferError::Remote("staging file disappeared".into()))?;
            nodes.insert(destination, node);
            drop(nodes);
            if fail_upload_stage_cleanup {
                failed_stage_cleanups
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(source);
            }
            Ok(())
        })
    }

    fn rename_replace<'a>(
        &'a self,
        _server_id: &'a str,
        source: &'a str,
        destination: &'a str,
    ) -> TransferFuture<'a, ()> {
        let nodes = Arc::clone(&self.nodes);
        let source = source.to_string();
        let destination = destination.to_string();
        Box::pin(async move {
            let mut nodes = nodes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let node = nodes
                .remove(&source)
                .ok_or_else(|| TransferError::Remote("staging file disappeared".into()))?;
            nodes.insert(destination, node);
            Ok(())
        })
    }

    fn remove_recovered_staging<'a>(
        &'a self,
        _server_id: &'a str,
        path: &'a str,
    ) -> TransferFuture<'a, ()> {
        let nodes = Arc::clone(&self.nodes);
        let path = path.to_string();
        Box::pin(async move {
            let mut nodes = nodes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match nodes.get(&path) {
                Some(node) if node.kind == RemoteEntryKind::File => {
                    nodes.remove(&path);
                    Ok(())
                }
                None => Ok(()),
                Some(_) => Err(TransferError::InvalidInput(
                    "recovered staging entry is not a regular file".into(),
                )),
            }
        })
    }
}

struct CursorReader(Cursor<Vec<u8>>);

impl AsyncRead for CursorReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let cursor = &mut self.0;
        let remaining = cursor
            .get_ref()
            .len()
            .saturating_sub(cursor.position() as usize);
        let length = remaining.min(buffer.remaining());
        if length > 0 {
            let start = cursor.position() as usize;
            buffer.put_slice(&cursor.get_ref()[start..start + length]);
            cursor.set_position((start + length) as u64);
        }
        Poll::Ready(Ok(()))
    }
}

struct MemoryWriter {
    nodes: Arc<Mutex<HashMap<String, Node>>>,
    path: String,
}

impl AsyncWrite for MemoryWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let mut nodes = this
            .nodes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(node) = nodes.get_mut(&this.path) else {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "staging file disappeared",
            )));
        };
        node.bytes.extend_from_slice(bytes);
        Poll::Ready(Ok(bytes.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

fn temp_dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("temporary directory")
}

async fn wait_for_status(
    manager: &TransferManager<FakeRemote>,
    transfer_id: &TransferId,
    expected: TransferStatus,
) -> TransferSnapshot {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = manager.get(transfer_id).await.expect("transfer snapshot");
            if snapshot.status == expected {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("transfer should reach expected state")
}

#[tokio::test]
async fn large_binary_upload_and_download_streams_without_text_limits() {
    let remote = FakeRemote::new(true);
    remote.insert("/drop", RemoteEntryKind::Directory, Vec::new());
    let manager = TransferManager::new(remote.clone());
    let local_source = temp_dir();
    let local_dest = temp_dir();
    let payload = (0..(3 * 1024 * 1024 + 317))
        .map(|index| ((index * 31 + 17) % 256) as u8)
        .collect::<Vec<_>>();
    let source_path = local_source.path().join("binary data.bin");
    tokio::fs::write(&source_path, &payload)
        .await
        .expect("write source");

    let upload_id = manager
        .start_upload("srv_test", "/drop", vec![source_path], ConflictPolicy::Ask)
        .expect("queue upload");
    let upload = wait_for_status(&manager, &upload_id, TransferStatus::Completed).await;
    assert_eq!(upload.verified_files, 1);
    assert_eq!(upload.transferred_bytes, payload.len() as u64);
    assert_eq!(
        remote.contents("/drop/binary data.bin"),
        Some(payload.clone())
    );

    let download_id = manager
        .start_download(
            "srv_test",
            vec!["/drop/binary data.bin".to_string()],
            local_dest.path().to_path_buf(),
            ConflictPolicy::Ask,
        )
        .expect("queue download");
    let download = wait_for_status(&manager, &download_id, TransferStatus::Completed).await;
    assert_eq!(download.verified_files, 1);
    let downloaded = tokio::fs::read(local_dest.path().join("binary data.bin"))
        .await
        .expect("read downloaded file");
    assert_eq!(downloaded, payload);
}

#[tokio::test]
async fn published_upload_is_complete_when_staging_link_cleanup_fails() {
    let mut remote = FakeRemote::new(false);
    remote.insert("/drop", RemoteEntryKind::Directory, Vec::new());
    remote.fail_upload_stage_cleanup();
    let manager = TransferManager::new(remote.clone());
    let local_source = temp_dir();
    let payload = b"verified upload".to_vec();
    let source_path = local_source.path().join("published.txt");
    tokio::fs::write(&source_path, &payload)
        .await
        .expect("write source");

    let id = manager
        .start_upload("srv_test", "/drop", vec![source_path], ConflictPolicy::Ask)
        .expect("queue upload");
    let result = wait_for_status(&manager, &id, TransferStatus::Partial).await;

    assert_eq!(result.completed_files, 1);
    assert_eq!(result.verified_files, 1);
    assert_eq!(
        remote.contents("/drop/published.txt"),
        Some(payload.clone())
    );
    assert_eq!(result.failures.len(), 1);
    assert!(result.failures[0]
        .message
        .contains("was published and verified"));
    let residue = result
        .staging_residue
        .first()
        .expect("staging alias cleanup is recorded")
        .clone();
    assert!(residue.starts_with("/drop/.published.txt.yukinal-"));
    assert_eq!(remote.contents(&residue), Some(payload));
}

#[tokio::test]
async fn target_appearing_after_preflight_is_not_overwritten() {
    let mut remote = FakeRemote::new(false);
    remote.insert("/drop", RemoteEntryKind::Directory, Vec::new());
    remote.race_target_on_publish("/drop/raced.bin", b"concurrent writer");
    let manager = TransferManager::new(remote.clone());
    let local_source = temp_dir();
    let source_path = local_source.path().join("raced.bin");
    tokio::fs::write(&source_path, b"our upload")
        .await
        .expect("write source");

    let id = manager
        .start_upload("srv_test", "/drop", vec![source_path], ConflictPolicy::Ask)
        .expect("queue upload while the target is initially absent");
    let result = wait_for_status(&manager, &id, TransferStatus::Failed).await;

    assert_eq!(result.completed_files, 0);
    assert_eq!(
        remote.contents("/drop/raced.bin"),
        Some(b"concurrent writer".to_vec())
    );
    assert!(result.staging_residue.is_empty());
    assert!(!remote
        .nodes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .keys()
        .any(|path| path.starts_with("/drop/.raced.bin.yukinal-")));
}

#[tokio::test]
async fn unsupported_publish_cleans_the_upload_stage_and_fails_explicitly() {
    let mut remote = FakeRemote::new(false);
    remote.insert("/drop", RemoteEntryKind::Directory, Vec::new());
    remote.refuse_publish();
    let manager = TransferManager::new(remote.clone());
    let local_source = temp_dir();
    let source_path = local_source.path().join("unsupported.bin");
    tokio::fs::write(&source_path, b"bytes that must not be published")
        .await
        .expect("write source");

    let id = manager
        .start_upload("srv_test", "/drop", vec![source_path], ConflictPolicy::Ask)
        .expect("queue upload");
    let result = wait_for_status(&manager, &id, TransferStatus::Failed).await;

    assert_eq!(result.failures.len(), 1);
    assert_eq!(
        result.failures[0].kind,
        super::types::TransferFailureKind::Unsupported
    );
    assert!(result.staging_residue.is_empty());
    assert_eq!(remote.contents("/drop/unsupported.bin"), None);
    assert!(!remote
        .nodes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .keys()
        .any(|path| path.starts_with("/drop/.unsupported.bin.yukinal-")));
}

#[tokio::test]
async fn conflict_pauses_for_user_and_supports_skip_or_rename() {
    let remote = FakeRemote::new(true);
    remote.insert("/drop", RemoteEntryKind::Directory, Vec::new());
    remote.insert("/drop/a.bin", RemoteEntryKind::File, b"old".to_vec());
    let manager = TransferManager::new(remote.clone());
    let source_dir = temp_dir();
    let source = source_dir.path().join("a.bin");
    tokio::fs::write(&source, b"incoming")
        .await
        .expect("write source");

    let id = manager
        .start_upload("srv_test", "/drop", vec![source], ConflictPolicy::Ask)
        .expect("queue upload");
    let waiting = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = manager.get(&id).await.expect("snapshot");
            if snapshot.status == TransferStatus::WaitingConflict {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("conflict should pause the transfer");
    let conflict = waiting.active_conflict.expect("active conflict");
    assert!(conflict.allowed_actions.contains(&ConflictActionKind::Skip));
    assert!(conflict
        .allowed_actions
        .contains(&ConflictActionKind::Rename));
    manager
        .resolve_conflict(
            &id,
            conflict.item_index,
            ConflictAction::Rename {
                name: "a copy.bin".into(),
            },
        )
        .await
        .expect("resolve as rename");
    let done = wait_for_status(&manager, &id, TransferStatus::Completed).await;
    assert_eq!(done.verified_files, 1);
    assert_eq!(remote.contents("/drop/a.bin"), Some(b"old".to_vec()));
    assert_eq!(
        remote.contents("/drop/a copy.bin"),
        Some(b"incoming".to_vec())
    );
}

#[tokio::test]
async fn cancel_removes_download_staging_and_never_publishes_partial_target() {
    let remote = FakeRemote::new(true);
    remote.insert("/drop", RemoteEntryKind::Directory, Vec::new());
    remote.insert(
        "/drop/slow.bin",
        RemoteEntryKind::File,
        vec![0xA5; 2 * 1024 * 1024],
    );
    remote.slow_reads.store(true, Ordering::Relaxed);
    let manager = TransferManager::new(remote);
    let local_dest = temp_dir();
    let id = manager
        .start_download(
            "srv_test",
            vec!["/drop/slow.bin".into()],
            local_dest.path().to_path_buf(),
            ConflictPolicy::Ask,
        )
        .expect("queue download");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = manager.get(&id).await.expect("snapshot");
            if snapshot.transferred_bytes > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("download should begin before cancellation");
    manager.cancel(&id).expect("cancel transfer");
    let cancelled = wait_for_status(&manager, &id, TransferStatus::Cancelled).await;
    assert!(cancelled.staging_residue.is_empty());
    assert!(!local_dest.path().join("slow.bin").exists());
    let names = std::fs::read_dir(local_dest.path())
        .expect("read destination")
        .map(|entry| entry.expect("directory entry").file_name())
        .collect::<Vec<_>>();
    assert!(
        names.is_empty(),
        "cancelled staging file was left behind: {names:?}"
    );
}

#[tokio::test]
async fn unsupported_remote_overwrite_is_not_offered_or_attempted() {
    let remote = FakeRemote::new(false);
    remote.insert("/drop", RemoteEntryKind::Directory, Vec::new());
    remote.insert("/drop/a.bin", RemoteEntryKind::File, b"old".to_vec());
    let manager = TransferManager::new(remote.clone());
    let source_dir = temp_dir();
    let source = source_dir.path().join("a.bin");
    tokio::fs::write(&source, b"incoming")
        .await
        .expect("write source");

    let id = manager
        .start_upload("srv_test", "/drop", vec![source], ConflictPolicy::Ask)
        .expect("queue upload");
    let waiting = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = manager.get(&id).await.expect("snapshot");
            if snapshot.status == TransferStatus::WaitingConflict {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("conflict should pause");
    let conflict = waiting.active_conflict.expect("active conflict");
    assert!(!conflict
        .allowed_actions
        .contains(&ConflictActionKind::Overwrite));
    manager
        .resolve_conflict(&id, conflict.item_index, ConflictAction::Skip)
        .await
        .expect("skip is safe");
    let result = wait_for_status(&manager, &id, TransferStatus::Partial).await;
    assert_eq!(result.skipped_files, 1);
    assert_eq!(remote.contents("/drop/a.bin"), Some(b"old".to_vec()));
}

#[tokio::test]
async fn startup_recovery_cleans_only_recorded_stages_and_never_restores_success() {
    let directory = tempfile::tempdir().expect("create staging directory");
    let history = MemoryTransferHistory::default();
    let active_id = TransferId("tr_1_2_3".into());
    let completed_id = TransferId("tr_4_5_6".into());
    let active_stage = directory.path().join(".active.yukinal-tr_1_2_3-0-0.part");
    let completed_stage = directory.path().join(".done.yukinal-tr_4_5_6-0-0.part");
    let unrelated = directory.path().join(".not-owned.part");
    std::fs::write(&active_stage, b"partial").expect("write active stage");
    std::fs::write(&completed_stage, b"orphan").expect("write completed residue");
    std::fs::write(&unrelated, b"keep").expect("write unrelated file");

    let mut active = TransferSnapshot::new(
        active_id.clone(),
        "srv_recovery".into(),
        TransferDirection::Download,
    );
    active.status = TransferStatus::Running;
    active.current_item = Some("secret-name.txt".into());
    active.current_item_bytes = 7;
    let mut completed = TransferSnapshot::new(
        completed_id.clone(),
        "srv_recovery".into(),
        TransferDirection::Download,
    );
    completed.status = TransferStatus::Completed;
    history
        .insert(
            &active,
            &[TransferStagingFile::Local {
                path: active_stage.clone(),
                item_index: 0,
            }],
        )
        .expect("persist active transfer");
    history
        .insert(
            &completed,
            &[TransferStagingFile::Local {
                path: completed_stage.clone(),
                item_index: 0,
            }],
        )
        .expect("persist completed transfer with cleanup residue");

    let manager = TransferManager::with_history(FakeRemote::new(false), Arc::new(history.clone()))
        .expect("recover transfers on startup");
    assert!(!active_stage.exists(), "recorded active stage was cleaned");
    assert!(
        !completed_stage.exists(),
        "recorded terminal residue was cleaned"
    );
    assert!(
        unrelated.exists(),
        "unrelated same-directory files are preserved"
    );
    let interrupted = manager
        .get(&active_id)
        .await
        .expect("active row still queryable");
    assert_eq!(interrupted.status, TransferStatus::Interrupted);
    let public_snapshot = serde_json::to_string(&interrupted).expect("serialize public snapshot");
    assert!(!public_snapshot.contains(&active_stage.to_string_lossy().to_string()));
    assert_eq!(
        manager
            .get(&completed_id)
            .await
            .expect("completed row still queryable")
            .status,
        TransferStatus::Completed,
        "startup cleanup must never promote or rewrite terminal success"
    );
    let staging = history.staging(&active_id).expect("read cleaned inventory");
    assert!(staging.is_empty());
}

#[tokio::test]
async fn remote_recovery_is_reported_until_a_connected_backend_cleans_owned_stage() {
    let history = MemoryTransferHistory::default();
    let id = TransferId("tr_7_8_9".into());
    let path = "/srv/.remote.yukinal-tr_7_8_9-2-0.part";
    let mut snapshot = TransferSnapshot::new(
        id.clone(),
        "srv_remote_recovery".into(),
        TransferDirection::Upload,
    );
    snapshot.status = TransferStatus::Running;
    history
        .insert(
            &snapshot,
            &[TransferStagingFile::Remote {
                server_id: "srv_remote_recovery".into(),
                path: path.into(),
                item_index: 2,
            }],
        )
        .expect("persist remote staging ownership");
    let backend = FakeRemote::new(false);
    backend.insert(path, RemoteEntryKind::File, b"partial".to_vec());
    let manager = TransferManager::with_history(backend.clone(), Arc::new(history.clone()))
        .expect("mark interrupted and preserve remote stage");

    let interrupted = manager.get(&id).await.expect("recovered snapshot");
    assert_eq!(interrupted.status, TransferStatus::Interrupted);
    assert_eq!(
        interrupted.staging_residue,
        vec![".remote.yukinal-tr_7_8_9-2-0.part"]
    );
    assert!(backend.contents(path).is_some());

    manager.reconcile_remote_staging().await;
    assert!(backend.contents(path).is_none());
    let cleaned = manager.get(&id).await.expect("query after remote cleanup");
    assert!(cleaned.staging_residue.is_empty());
    assert!(cleaned.failures.is_empty());
    assert!(history
        .staging(&id)
        .expect("read staging inventory")
        .is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn upload_rejects_symlinks_without_following_them() {
    use std::os::unix::fs::symlink;

    let remote = FakeRemote::new(true);
    remote.insert("/drop", RemoteEntryKind::Directory, Vec::new());
    let source_dir = temp_dir();
    let actual = source_dir.path().join("actual.bin");
    let link = source_dir.path().join("link.bin");
    tokio::fs::write(&actual, b"secret")
        .await
        .expect("write source");
    symlink(&actual, &link).expect("create symlink");
    let manager = TransferManager::new(remote.clone());

    let id = manager
        .start_upload("srv_test", "/drop", vec![link], ConflictPolicy::Ask)
        .expect("queue upload");
    let result = wait_for_status(&manager, &id, TransferStatus::Failed).await;
    assert_eq!(result.failures.len(), 1);
    assert_eq!(remote.contents("/drop/link.bin"), None);
}

#[tokio::test]
async fn progress_snapshots_are_published_through_bounded_subscription() {
    let remote = FakeRemote::new(true);
    remote.insert("/drop", RemoteEntryKind::Directory, Vec::new());
    let manager = TransferManager::new(remote);
    let mut updates = manager.subscribe_updates();
    let source_dir = temp_dir();
    let source = source_dir.path().join("note.bin");
    tokio::fs::write(&source, b"payload")
        .await
        .expect("write source");
    let id = manager
        .start_upload("srv_test", "/drop", vec![source], ConflictPolicy::Ask)
        .expect("queue upload");

    let mut observed = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let update = updates.recv().await.expect("snapshot update");
            let status = update.status;
            observed.push(status);
            if status == TransferStatus::Completed {
                break;
            }
        }
    })
    .await
    .expect("terminal update");
    assert!(observed.contains(&TransferStatus::Queued));
    assert!(observed.contains(&TransferStatus::Running));
    assert_eq!(observed.last(), Some(&TransferStatus::Completed));
    assert_eq!(
        manager.get(&id).await.expect("final snapshot").status,
        TransferStatus::Completed
    );
}
