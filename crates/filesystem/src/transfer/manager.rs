use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::runtime::Handle;
use tokio::sync::{broadcast, mpsc, Mutex, MutexGuard, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::local;
use super::types::{
    remote_child, safe_name, validate_remote_path, ConflictAction, ConflictActionKind,
    ConflictPolicy, ConflictRequest, RemoteTransferBackend, TransferDirection, TransferError,
    TransferFailureKind, TransferHistoryStore, TransferId, TransferItemFailure, TransferSnapshot,
    TransferStagingFile, TransferStatus,
};
use crate::service::{RemoteEntryKind, RemoteStat};

const COPY_BUFFER_BYTES: usize = 64 * 1024;
const MAX_PARALLEL_TRANSFERS: usize = 3;
const MAX_STAGING_ATTEMPTS: u32 = 32;
static NEXT_TRANSFER_ID: AtomicU64 = AtomicU64::new(1);

struct Job {
    snapshot: Arc<SnapshotCell>,
    cancel: CancellationToken,
    decision_tx: mpsc::UnboundedSender<ConflictDecision>,
    join: StdMutex<Option<JoinHandle<()>>>,
}

#[derive(Clone)]
struct ConflictDecision {
    item_index: u64,
    action: ConflictAction,
}

/// Shared state for one worker. Passing it through the transfer pipeline keeps backend,
/// cancellation, policy and progress reporting tied to the same transfer.
struct TransferExecutionContext<B> {
    backend: Arc<B>,
    transfer_id: TransferId,
    server_id: String,
    snapshot: Arc<SnapshotCell>,
    cancel: CancellationToken,
    policy: ConflictPolicy,
}

struct UploadItem<'a> {
    source: &'a Path,
    target: &'a str,
    display_name: &'a str,
    index: u64,
}

struct DownloadItem<'a> {
    source: &'a str,
    initial_target: &'a Path,
    display_name: &'a str,
    initial_stat: RemoteStat,
    index: u64,
}

struct ManagerInner<B> {
    backend: Arc<B>,
    history: Option<Arc<dyn TransferHistoryStore>>,
    pending_recovery: StdMutex<HashMap<TransferId, Vec<TransferStagingFile>>>,
    jobs: StdMutex<HashMap<TransferId, Arc<Job>>>,
    permits: Arc<Semaphore>,
    updates: broadcast::Sender<TransferSnapshot>,
    shutting_down: AtomicBool,
}

struct SnapshotCell {
    inner: Mutex<TransferSnapshot>,
    staging: StdMutex<Vec<TransferStagingFile>>,
    history: Option<Arc<dyn TransferHistoryStore>>,
    updates: broadcast::Sender<TransferSnapshot>,
    last_publish: StdMutex<(Instant, TransferStatus)>,
}

struct SnapshotWriteGuard<'a> {
    inner: MutexGuard<'a, TransferSnapshot>,
    staging: &'a StdMutex<Vec<TransferStagingFile>>,
    history: Option<&'a Arc<dyn TransferHistoryStore>>,
    updates: &'a broadcast::Sender<TransferSnapshot>,
    last_publish: &'a StdMutex<(Instant, TransferStatus)>,
    force_persist: bool,
}

impl SnapshotCell {
    fn new(
        snapshot: TransferSnapshot,
        updates: broadcast::Sender<TransferSnapshot>,
        history: Option<Arc<dyn TransferHistoryStore>>,
    ) -> Self {
        Self {
            inner: Mutex::new(snapshot.clone()),
            staging: StdMutex::new(Vec::new()),
            history,
            updates,
            last_publish: StdMutex::new((Instant::now() - Duration::from_secs(1), snapshot.status)),
        }
    }

    async fn read(&self) -> MutexGuard<'_, TransferSnapshot> {
        self.inner.lock().await
    }

    async fn write(&self) -> SnapshotWriteGuard<'_> {
        SnapshotWriteGuard {
            inner: self.inner.lock().await,
            staging: &self.staging,
            history: self.history.as_ref(),
            updates: &self.updates,
            last_publish: &self.last_publish,
            force_persist: false,
        }
    }

    async fn write_force_persist(&self) -> SnapshotWriteGuard<'_> {
        let mut guard = self.write().await;
        guard.force_persist = true;
        guard
    }

    fn persist_initial(&self) -> Result<(), TransferError> {
        let Some(history) = &self.history else {
            return Ok(());
        };
        let snapshot = self.inner.try_lock().map_err(|_| {
            TransferError::Local("new transfer snapshot was unexpectedly locked".into())
        })?;
        let staging = self
            .staging
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        history.insert(&snapshot, &staging).map_err(|error| {
            TransferError::Local(format!("could not persist transfer history: {error}"))
        })
    }

    async fn persist_now(&self) -> Result<(), TransferError> {
        let Some(history) = &self.history else {
            return Ok(());
        };
        let snapshot = self.inner.lock().await;
        let staging = self.staging_snapshot();
        history.update(&snapshot, &staging).map_err(|error| {
            TransferError::Local(format!("could not persist transfer state: {error}"))
        })
    }

    async fn add_staging(&self, artifact: TransferStagingFile) -> Result<(), TransferError> {
        self.staging
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(artifact);
        let mut snapshot = self.write_force_persist().await;
        snapshot.touch();
        drop(snapshot);
        self.persist_now().await
    }

    async fn remove_staging(&self, artifact: &TransferStagingFile) -> Result<(), TransferError> {
        self.staging
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|current| current != artifact);
        let mut snapshot = self.write_force_persist().await;
        snapshot.touch();
        drop(snapshot);
        self.persist_now().await
    }

    async fn finalize_status(&self, requested_status: TransferStatus) {
        let mut snapshot = self.inner.lock().await;
        snapshot.current_item = None;
        snapshot.current_item_bytes = 0;
        snapshot.current_item_total_bytes = None;
        snapshot.active_conflict = None;
        snapshot.status = requested_status;
        snapshot.touch();

        if let Some(history) = &self.history {
            let staging = self.staging_snapshot();
            if history.update(&snapshot, &staging).is_err() {
                // Never publish a successful terminal state if the durable ledger
                // could not record it. The previous stored state will reconcile to
                // Interrupted on restart; this live view is explicitly failed.
                snapshot.status = TransferStatus::Failed;
                snapshot.failures.push(TransferItemFailure {
                    item_index: None,
                    item: "transfer history".into(),
                    kind: TransferFailureKind::LocalIo,
                    message: "the final transfer state could not be saved; verify the destination before retrying".into(),
                    staging_residue: None,
                });
                snapshot.touch();
                let _ = history.update(&snapshot, &staging);
            }
        }

        let status = snapshot.status;
        *self
            .last_publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = (Instant::now(), status);
        let _ = self.updates.send(snapshot.clone());
    }

    fn staging_snapshot(&self) -> Vec<TransferStagingFile> {
        self.staging
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn publish_initial(&self) {
        let updates = self.updates.clone();
        let snapshot = self.inner.try_lock().ok().map(|snapshot| snapshot.clone());
        if let Some(snapshot) = snapshot {
            let _ = updates.send(snapshot);
        }
    }
}

impl Deref for SnapshotWriteGuard<'_> {
    type Target = TransferSnapshot;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for SnapshotWriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl Drop for SnapshotWriteGuard<'_> {
    fn drop(&mut self) {
        let status = self.inner.status;
        let (should_publish, should_persist) = {
            let mut last = self
                .last_publish
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if last.1 != status || last.0.elapsed() >= Duration::from_millis(100) {
                *last = (Instant::now(), status);
                (true, true)
            } else {
                (false, self.force_persist)
            }
        };
        if should_persist {
            if let Some(history) = self.history {
                let staging = self
                    .staging
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let _ = history.update(&self.inner, &staging);
            }
        }
        if should_publish {
            let _ = self.updates.send(self.inner.clone());
        }
    }
}

/// Owns transfer task lifetimes and their latest queryable snapshots.
///
/// The manager is cloneable and uses short-held internal locks, so Tauri can keep it in shared
/// state and expose `get`/`cancel`/conflict resolution commands without holding a lock over I/O.
/// It limits simultaneous transfers to three by default. Each transfer itself streams one file at
/// a time with a fixed 64 KiB copy buffer.
pub struct TransferManager<B> {
    inner: Arc<ManagerInner<B>>,
}

impl<B> Clone for TransferManager<B> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<B: RemoteTransferBackend> TransferManager<B> {
    #[must_use]
    pub fn new(backend: B) -> Self {
        Self::with_parallel_limit(backend, MAX_PARALLEL_TRANSFERS)
    }

    #[must_use]
    pub fn with_parallel_limit(backend: B, max_parallel_transfers: usize) -> Self {
        Self::build(backend, max_parallel_transfers, None)
    }

    /// Create a manager backed by durable host history. Startup recovery is
    /// synchronous and fail-closed: active rows become `interrupted` before the
    /// manager can accept new work, then only validated local stage files are
    /// removed. Remote stages remain recorded until an authenticated session is
    /// already available; startup never auto-connects to a server.
    pub fn with_history(
        backend: B,
        history: Arc<dyn TransferHistoryStore>,
    ) -> Result<Self, TransferError> {
        let manager = Self::build(backend, MAX_PARALLEL_TRANSFERS, Some(Arc::clone(&history)));
        let recovered = history
            .recover(super::types::epoch_millis())
            .map_err(|error| TransferError::Local(format!("transfer recovery failed: {error}")))?;
        for record in recovered {
            let mut snapshot = record.snapshot;
            let mut pending = Vec::new();
            for artifact in record.staging {
                match &artifact {
                    TransferStagingFile::Local { path, item_index } => {
                        match local::remove_recovered_staging(path, &snapshot.transfer_id) {
                            Ok(()) => {}
                            Err(error) => {
                                push_recovery_failure(
                                    &mut snapshot,
                                    *item_index,
                                    path.file_name()
                                        .and_then(|name| name.to_str())
                                        .unwrap_or("local staging file"),
                                    TransferFailureKind::LocalIo,
                                    format!(
                                        "startup recovery could not clean local staging: {error}"
                                    ),
                                );
                                pending.push(artifact);
                            }
                        }
                    }
                    TransferStagingFile::Remote {
                        server_id,
                        path,
                        item_index,
                    } => {
                        let safe_path = server_id == &snapshot.server_id
                            && validate_remote_path(path).is_ok()
                            && path.rsplit('/').next().is_some_and(|name| {
                                super::types::is_owned_staging_name(name, &snapshot.transfer_id)
                            });
                        if !safe_path {
                            push_recovery_failure(
                                &mut snapshot,
                                *item_index,
                                "remote staging record",
                                TransferFailureKind::RemoteIo,
                                "startup recovery rejected a remote staging record that did not match its owner".into(),
                            );
                        } else {
                            push_recovery_failure(
                                &mut snapshot,
                                *item_index,
                                path.rsplit('/').next().unwrap_or("remote staging file"),
                                TransferFailureKind::RemoteIo,
                                "remote staging cleanup is pending an authenticated server session"
                                    .into(),
                            );
                        }
                        pending.push(artifact);
                    }
                }
            }
            if !pending.is_empty() {
                manager
                    .inner
                    .pending_recovery
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(snapshot.transfer_id.clone(), pending.clone());
            }
            history.update(&snapshot, &pending).map_err(|error| {
                TransferError::Local(format!("could not persist transfer recovery: {error}"))
            })?;
        }
        Ok(manager)
    }

    fn build(
        backend: B,
        max_parallel_transfers: usize,
        history: Option<Arc<dyn TransferHistoryStore>>,
    ) -> Self {
        let (updates, _) = broadcast::channel(128);
        Self {
            inner: Arc::new(ManagerInner {
                backend: Arc::new(backend),
                history,
                pending_recovery: StdMutex::new(HashMap::new()),
                jobs: StdMutex::new(HashMap::new()),
                permits: Arc::new(Semaphore::new(max_parallel_transfers.max(1))),
                updates,
                shutting_down: AtomicBool::new(false),
            }),
        }
    }

    /// Queue local files/directories for upload into one remote directory.
    ///
    /// Local paths stay inside the Rust host. The method accepts no file contents, and the worker
    /// opens regular files with no-follow semantics before streaming them to an exclusive remote
    /// staging file.
    pub fn start_upload(
        &self,
        server_id: &str,
        remote_dir: &str,
        local_paths: Vec<PathBuf>,
        policy: ConflictPolicy,
    ) -> Result<TransferId, TransferError> {
        validate_start(server_id, remote_dir, &local_paths)?;
        let runtime = Handle::try_current().map_err(|_| {
            TransferError::InvalidInput(
                "transfers must be started from the host async runtime".into(),
            )
        })?;
        let id = new_transfer_id();
        let snapshot = Arc::new(SnapshotCell::new(
            TransferSnapshot::new(id.clone(), server_id.to_string(), TransferDirection::Upload),
            self.inner.updates.clone(),
            self.inner.history.clone(),
        ));
        let cancel = CancellationToken::new();
        let (decision_tx, decision_rx) = mpsc::unbounded_channel();
        let job = Arc::new(Job {
            snapshot: Arc::clone(&snapshot),
            cancel: cancel.clone(),
            decision_tx,
            join: StdMutex::new(None),
        });
        job.snapshot.persist_initial()?;
        self.insert_job(id.clone(), Arc::clone(&job))?;
        job.snapshot.publish_initial();

        let inner = Arc::clone(&self.inner);
        let worker_id = id.clone();
        let server_id = server_id.to_string();
        let remote_dir = remote_dir.to_string();
        let work_transfer_id = id.clone();
        let worker_job = Arc::clone(&job);
        let handle = runtime.spawn(async move {
            run_job(
                inner,
                worker_id,
                worker_job,
                decision_rx,
                async move |backend, snapshot, cancel, rx| {
                    upload_batch(
                        TransferExecutionContext {
                            backend,
                            transfer_id: work_transfer_id,
                            server_id,
                            snapshot,
                            cancel,
                            policy,
                        },
                        remote_dir,
                        local_paths,
                        rx,
                    )
                    .await
                },
            )
            .await;
        });
        *job.join
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(handle);
        Ok(id)
    }

    /// Queue remote files/directories for download into an existing local directory.
    pub fn start_download(
        &self,
        server_id: &str,
        remote_paths: Vec<String>,
        local_dir: PathBuf,
        policy: ConflictPolicy,
    ) -> Result<TransferId, TransferError> {
        validate_download_start(server_id, &remote_paths, &local_dir)?;
        let runtime = Handle::try_current().map_err(|_| {
            TransferError::InvalidInput(
                "transfers must be started from the host async runtime".into(),
            )
        })?;
        let id = new_transfer_id();
        let snapshot = Arc::new(SnapshotCell::new(
            TransferSnapshot::new(
                id.clone(),
                server_id.to_string(),
                TransferDirection::Download,
            ),
            self.inner.updates.clone(),
            self.inner.history.clone(),
        ));
        let cancel = CancellationToken::new();
        let (decision_tx, decision_rx) = mpsc::unbounded_channel();
        let job = Arc::new(Job {
            snapshot: Arc::clone(&snapshot),
            cancel: cancel.clone(),
            decision_tx,
            join: StdMutex::new(None),
        });
        job.snapshot.persist_initial()?;
        self.insert_job(id.clone(), Arc::clone(&job))?;
        job.snapshot.publish_initial();

        let inner = Arc::clone(&self.inner);
        let worker_id = id.clone();
        let server_id = server_id.to_string();
        let work_transfer_id = id.clone();
        let worker_job = Arc::clone(&job);
        let handle = runtime.spawn(async move {
            run_job(
                inner,
                worker_id,
                worker_job,
                decision_rx,
                async move |backend, snapshot, cancel, rx| {
                    download_batch(
                        TransferExecutionContext {
                            backend,
                            transfer_id: work_transfer_id,
                            server_id,
                            snapshot,
                            cancel,
                            policy,
                        },
                        remote_paths,
                        local_dir,
                        rx,
                    )
                    .await
                },
            )
            .await;
        });
        *job.join
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(handle);
        Ok(id)
    }

    /// Latest stable snapshot for one transfer. It can be polled after progress events are missed.
    pub async fn get(&self, transfer_id: &TransferId) -> Option<TransferSnapshot> {
        self.get_result(transfer_id).await.ok().flatten()
    }

    pub async fn get_result(
        &self,
        transfer_id: &TransferId,
    ) -> Result<Option<TransferSnapshot>, TransferError> {
        if let Some(job) = self.job(transfer_id) {
            let snapshot = job.snapshot.read().await.clone();
            return Ok(Some(snapshot));
        }
        self.inner
            .history
            .as_ref()
            .map(|history| {
                history.get(transfer_id).map_err(|error| {
                    TransferError::Local(format!("could not read transfer history: {error}"))
                })
            })
            .transpose()
            .map(Option::flatten)
    }

    pub async fn list(&self) -> Vec<TransferSnapshot> {
        self.list_bounded(None, 100).await.unwrap_or_default()
    }

    pub async fn list_bounded(
        &self,
        server_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<TransferSnapshot>, TransferError> {
        if limit == 0 || limit > 100 {
            return Err(TransferError::InvalidInput(
                "transfer history limit must be between 1 and 100".into(),
            ));
        }
        let mut snapshots = HashMap::new();
        if let Some(history) = &self.inner.history {
            for snapshot in history.list(server_id, limit).map_err(|error| {
                TransferError::Local(format!("could not list transfer history: {error}"))
            })? {
                snapshots.insert(snapshot.transfer_id.clone(), snapshot);
            }
        }
        let jobs = self
            .inner
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for job in jobs {
            let snapshot = job.snapshot.read().await.clone();
            if server_id.is_none_or(|server| snapshot.server_id == server) {
                snapshots.insert(snapshot.transfer_id.clone(), snapshot);
            }
        }
        let mut snapshots = snapshots.into_values().collect::<Vec<_>>();
        snapshots.sort_by(|left, right| {
            right
                .updated_at_epoch_ms
                .cmp(&left.updated_at_epoch_ms)
                .then_with(|| right.transfer_id.0.cmp(&left.transfer_id.0))
        });
        snapshots.truncate(limit);
        Ok(snapshots)
    }

    /// Retry remote startup-stage cleanup for servers that are already connected.
    /// The SFTP host adapter deliberately refuses to open a new session here.
    pub async fn reconcile_remote_staging(&self) {
        let pending = self
            .inner
            .pending_recovery
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        for (transfer_id, artifacts) in pending {
            let mut remaining = Vec::new();
            let mut cleaned_names = Vec::new();
            for artifact in artifacts {
                match &artifact {
                    TransferStagingFile::Remote {
                        server_id, path, ..
                    } if path.rsplit('/').next().is_some_and(|name| {
                        super::types::is_owned_staging_name(name, &transfer_id)
                    }) && validate_remote_path(path).is_ok() =>
                    {
                        match self
                            .inner
                            .backend
                            .remove_recovered_staging(server_id, path)
                            .await
                        {
                            Ok(()) => cleaned_names.push(
                                path.rsplit('/')
                                    .next()
                                    .unwrap_or("remote staging file")
                                    .to_string(),
                            ),
                            Err(_) => remaining.push(artifact),
                        }
                    }
                    _ => remaining.push(artifact),
                }
            }
            if cleaned_names.is_empty() {
                continue;
            }
            if let Some(history) = &self.inner.history {
                let mut snapshot = match history.get(&transfer_id) {
                    Ok(Some(snapshot)) => snapshot,
                    _ => continue,
                };
                snapshot
                    .staging_residue
                    .retain(|name| !cleaned_names.contains(name));
                snapshot.failures.retain(|failure| {
                    !(failure.message
                        == "remote staging cleanup is pending an authenticated server session"
                        && failure
                            .staging_residue
                            .as_ref()
                            .is_some_and(|name| cleaned_names.contains(name)))
                });
                snapshot.touch();
                let _ = history.update(&snapshot, &remaining);
            }
            self.inner
                .pending_recovery
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(transfer_id, remaining);
        }
    }

    /// Request cancellation. The worker stops at the next stream operation and removes its
    /// staging file; a failed cleanup is reported in `stagingResidue`.
    pub fn cancel(&self, transfer_id: &TransferId) -> Result<(), TransferError> {
        let job = self
            .job(transfer_id)
            .ok_or_else(|| TransferError::InvalidInput("transfer was not found".to_string()))?;
        job.cancel.cancel();
        Ok(())
    }

    /// Subscribe to bounded snapshot updates. A lagged receiver can recover the latest state via
    /// `get`; intermediate progress snapshots are intentionally coalesced to at most 10 per second
    /// per transfer while lifecycle state changes publish immediately.
    pub fn subscribe_updates(&self) -> broadcast::Receiver<TransferSnapshot> {
        self.inner.updates.subscribe()
    }

    /// Resolve the conflict currently shown in the snapshot. Stale item indexes are rejected.
    pub async fn resolve_conflict(
        &self,
        transfer_id: &TransferId,
        item_index: u64,
        action: ConflictAction,
    ) -> Result<(), TransferError> {
        let job = self
            .job(transfer_id)
            .ok_or_else(|| TransferError::InvalidInput("transfer was not found".to_string()))?;
        let snapshot = job.snapshot.read().await;
        let Some(conflict) = &snapshot.active_conflict else {
            return Err(TransferError::InvalidInput(
                "transfer is not waiting for a conflict decision".to_string(),
            ));
        };
        if snapshot.status != TransferStatus::WaitingConflict || conflict.item_index != item_index {
            return Err(TransferError::InvalidInput(
                "conflict decision does not match the active transfer item".to_string(),
            ));
        }
        let action_kind = match &action {
            ConflictAction::Skip => ConflictActionKind::Skip,
            ConflictAction::Overwrite => ConflictActionKind::Overwrite,
            ConflictAction::Rename { .. } => ConflictActionKind::Rename,
        };
        if !conflict.allowed_actions.contains(&action_kind) {
            return Err(TransferError::InvalidInput(
                "conflict action is not allowed for this target".to_string(),
            ));
        }
        if let ConflictAction::Rename { name } = &action {
            safe_name(name)?;
        }
        drop(snapshot);
        job.decision_tx
            .send(ConflictDecision { item_index, action })
            .map_err(|_| TransferError::Cancelled)
    }

    /// Stop admitting work, cancel active transfers, wait for their staging cleanup, and mark
    /// unfinished snapshots interrupted. Call this from the Rust host's shutdown path.
    pub async fn shutdown(&self) {
        if self.inner.shutting_down.swap(true, Ordering::AcqRel) {
            return;
        }
        let jobs = self
            .inner
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut previously_active = Vec::new();
        for job in &jobs {
            let snapshot = job.snapshot.read().await;
            if !snapshot.status.is_terminal() {
                previously_active.push(Arc::clone(job));
                job.cancel.cancel();
            }
        }
        for job in &jobs {
            let handle = job
                .join
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            if let Some(handle) = handle {
                let _ = handle.await;
            }
        }
        for job in previously_active {
            let mut snapshot = job.snapshot.write().await;
            snapshot.status = TransferStatus::Interrupted;
            snapshot.active_conflict = None;
            snapshot.current_item = None;
            snapshot.touch();
        }
    }

    fn insert_job(&self, id: TransferId, job: Arc<Job>) -> Result<(), TransferError> {
        if self.inner.shutting_down.load(Ordering::Acquire) {
            return Err(TransferError::Cancelled);
        }
        self.inner
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id, job);
        Ok(())
    }

    fn job(&self, id: &TransferId) -> Option<Arc<Job>> {
        self.inner
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(id)
            .cloned()
    }
}

fn validate_start(
    server_id: &str,
    remote_dir: &str,
    local_paths: &[PathBuf],
) -> Result<(), TransferError> {
    if server_id.trim().is_empty() {
        return Err(TransferError::InvalidInput(
            "serverId is required".to_string(),
        ));
    }
    validate_remote_path(remote_dir)?;
    if local_paths.is_empty() {
        return Err(TransferError::InvalidInput(
            "at least one local source is required".to_string(),
        ));
    }
    if local_paths.iter().any(|path| !path.is_absolute()) {
        return Err(TransferError::InvalidInput(
            "local source paths must be absolute host paths".to_string(),
        ));
    }
    Ok(())
}

fn validate_download_start(
    server_id: &str,
    remote_paths: &[String],
    local_dir: &Path,
) -> Result<(), TransferError> {
    if server_id.trim().is_empty() {
        return Err(TransferError::InvalidInput(
            "serverId is required".to_string(),
        ));
    }
    if remote_paths.is_empty() {
        return Err(TransferError::InvalidInput(
            "at least one remote source is required".to_string(),
        ));
    }
    for path in remote_paths {
        validate_remote_path(path)?;
    }
    if !local_dir.is_absolute() {
        return Err(TransferError::InvalidInput(
            "local destination must be an absolute host path".to_string(),
        ));
    }
    Ok(())
}

fn new_transfer_id() -> TransferId {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_TRANSFER_ID.fetch_add(1, Ordering::Relaxed);
    TransferId(format!(
        "tr_{:x}_{:x}_{}",
        std::process::id(),
        timestamp,
        sequence
    ))
}

async fn run_job<B, F, Fut>(
    inner: Arc<ManagerInner<B>>,
    transfer_id: TransferId,
    job: Arc<Job>,
    decision_rx: mpsc::UnboundedReceiver<ConflictDecision>,
    work: F,
) where
    B: RemoteTransferBackend,
    F: FnOnce(
        Arc<B>,
        Arc<SnapshotCell>,
        CancellationToken,
        mpsc::UnboundedReceiver<ConflictDecision>,
    ) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let permit = tokio::select! {
        _ = job.cancel.cancelled() => {
            finish_job(&job, TransferStatus::Cancelled).await;
            remove_terminal_job(&inner, &transfer_id);
            return;
        }
        permit = Arc::clone(&inner.permits).acquire_owned() => match permit {
            Ok(permit) => permit,
            Err(_) => {
                finish_job(&job, TransferStatus::Interrupted).await;
                remove_terminal_job(&inner, &transfer_id);
                return;
            }
        }
    };
    {
        let mut snapshot = job.snapshot.write().await;
        if job.cancel.is_cancelled() {
            snapshot.status = TransferStatus::Cancelled;
            snapshot.touch();
            return;
        }
        snapshot.status = TransferStatus::Running;
        snapshot.touch();
    }
    work(
        Arc::clone(&inner.backend),
        Arc::clone(&job.snapshot),
        job.cancel.clone(),
        decision_rx,
    )
    .await;
    drop(permit);

    let final_status = {
        let snapshot = job.snapshot.read().await;
        if job.cancel.is_cancelled() {
            TransferStatus::Cancelled
        } else if !snapshot.failures.is_empty() || snapshot.skipped_files > 0 {
            if snapshot.completed_files > 0 || snapshot.skipped_files > 0 {
                TransferStatus::Partial
            } else {
                TransferStatus::Failed
            }
        } else {
            TransferStatus::Completed
        }
    };
    job.snapshot.finalize_status(final_status).await;
    remove_terminal_job(&inner, &transfer_id);
}

async fn finish_job(job: &Job, status: TransferStatus) {
    job.snapshot.finalize_status(status).await;
}

fn remove_terminal_job<B: RemoteTransferBackend>(
    inner: &ManagerInner<B>,
    transfer_id: &TransferId,
) {
    if inner.history.is_some() {
        inner
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(transfer_id);
    }
}

async fn upload_batch<B: RemoteTransferBackend>(
    context: TransferExecutionContext<B>,
    remote_dir: String,
    local_paths: Vec<PathBuf>,
    mut decisions: mpsc::UnboundedReceiver<ConflictDecision>,
) {
    let backend = context.backend.as_ref();
    let server_id = context.server_id.as_str();
    let snapshot = context.snapshot.as_ref();
    let cancel = &context.cancel;
    let result = ensure_remote_directory(backend, server_id, &remote_dir).await;
    if let Err(error) = result {
        push_failure(snapshot, failure_for(None, &remote_dir, error, None)).await;
        return;
    }

    for source in local_paths {
        if cancel.is_cancelled() {
            break;
        }
        let name = match source.file_name().and_then(|name| name.to_str()) {
            Some(name) => name.to_string(),
            None => {
                push_failure(
                    snapshot,
                    failure_for(
                        None,
                        "selected source file",
                        TransferError::InvalidInput("local path has no portable UTF-8 name".into()),
                        None,
                    ),
                )
                .await;
                continue;
            }
        };
        if let Err(error) = safe_name(&name) {
            push_failure(snapshot, failure_for(None, &name, error, None)).await;
            continue;
        }
        let metadata = match tokio::fs::symlink_metadata(&source).await {
            Ok(metadata) => metadata,
            Err(error) => {
                push_failure(
                    snapshot,
                    failure_for(None, &name, TransferError::local(error), None),
                )
                .await;
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            push_failure(
                snapshot,
                failure_for(
                    None,
                    &name,
                    TransferError::InvalidInput(
                        "symbolic links and reparse points are not followed".into(),
                    ),
                    None,
                ),
            )
            .await;
            continue;
        }
        let remote_root = match remote_child(&remote_dir, &name) {
            Ok(path) => path,
            Err(error) => {
                push_failure(snapshot, failure_for(None, &name, error, None)).await;
                continue;
            }
        };
        if metadata.is_file() {
            let index = allocate_file(snapshot, metadata.len()).await;
            if let Err(error) = upload_file(
                &context,
                UploadItem {
                    source: &source,
                    target: &remote_root,
                    display_name: &name,
                    index,
                },
                &mut decisions,
            )
            .await
            {
                push_failure(snapshot, failure_for(Some(index), &name, error, None)).await;
            }
        } else if metadata.is_dir() {
            if let Err(error) = ensure_remote_directory(backend, server_id, &remote_root).await {
                push_failure(snapshot, failure_for(None, &name, error, None)).await;
                continue;
            }
            let mut stack = vec![(source, remote_root, name)];
            while let Some((local_dir, remote_dir, display_prefix)) = stack.pop() {
                if cancel.is_cancelled() {
                    break;
                }
                let children = match local::read_directory(&local_dir).await {
                    Ok(children) => children,
                    Err(error) => {
                        push_failure(snapshot, failure_for(None, &display_prefix, error, None))
                            .await;
                        continue;
                    }
                };
                for child in children.into_iter().rev() {
                    let child_name = match child.file_name().and_then(|name| name.to_str()) {
                        Some(name) => name.to_string(),
                        None => {
                            push_failure(
                                snapshot,
                                failure_for(
                                    None,
                                    &display_prefix,
                                    TransferError::InvalidInput(
                                        "directory entry name is not UTF-8".into(),
                                    ),
                                    None,
                                ),
                            )
                            .await;
                            continue;
                        }
                    };
                    if let Err(error) = safe_name(&child_name) {
                        push_failure(snapshot, failure_for(None, &child_name, error, None)).await;
                        continue;
                    }
                    let child_meta = match tokio::fs::symlink_metadata(&child).await {
                        Ok(metadata) => metadata,
                        Err(error) => {
                            push_failure(
                                snapshot,
                                failure_for(None, &child_name, TransferError::local(error), None),
                            )
                            .await;
                            continue;
                        }
                    };
                    if child_meta.file_type().is_symlink() {
                        push_failure(
                            snapshot,
                            failure_for(
                                None,
                                &format!("{display_prefix}/{child_name}"),
                                TransferError::InvalidInput(
                                    "symbolic links and reparse points are not followed".into(),
                                ),
                                None,
                            ),
                        )
                        .await;
                        continue;
                    }
                    let child_remote = match remote_child(&remote_dir, &child_name) {
                        Ok(path) => path,
                        Err(error) => {
                            push_failure(snapshot, failure_for(None, &child_name, error, None))
                                .await;
                            continue;
                        }
                    };
                    let display = format!("{display_prefix}/{child_name}");
                    if child_meta.is_dir() {
                        if let Err(error) =
                            ensure_remote_directory(backend, server_id, &child_remote).await
                        {
                            push_failure(snapshot, failure_for(None, &display, error, None)).await;
                        } else {
                            stack.push((child, child_remote, display));
                        }
                    } else if child_meta.is_file() {
                        let index = allocate_file(snapshot, child_meta.len()).await;
                        if let Err(error) = upload_file(
                            &context,
                            UploadItem {
                                source: &child,
                                target: &child_remote,
                                display_name: &display,
                                index,
                            },
                            &mut decisions,
                        )
                        .await
                        {
                            push_failure(snapshot, failure_for(Some(index), &display, error, None))
                                .await;
                        }
                    } else {
                        push_failure(
                            snapshot,
                            failure_for(
                                None,
                                &display,
                                TransferError::InvalidInput(
                                    "special files cannot be transferred".into(),
                                ),
                                None,
                            ),
                        )
                        .await;
                    }
                }
            }
        } else {
            push_failure(
                snapshot,
                failure_for(
                    None,
                    &name,
                    TransferError::InvalidInput("special files cannot be transferred".into()),
                    None,
                ),
            )
            .await;
        }
    }
}

async fn download_batch<B: RemoteTransferBackend>(
    context: TransferExecutionContext<B>,
    remote_paths: Vec<String>,
    local_dir: PathBuf,
    mut decisions: mpsc::UnboundedReceiver<ConflictDecision>,
) {
    let backend = context.backend.as_ref();
    let server_id = context.server_id.as_str();
    let snapshot = context.snapshot.as_ref();
    let cancel = &context.cancel;
    if let Err(error) = local::ensure_download_directory(&local_dir).await {
        push_failure(
            snapshot,
            failure_for(None, "local download destination", error, None),
        )
        .await;
        return;
    }
    for source in remote_paths {
        if cancel.is_cancelled() {
            break;
        }
        let name = source.rsplit('/').next().unwrap_or_default().to_string();
        if let Err(error) = safe_name(&name) {
            push_failure(snapshot, failure_for(None, &source, error, None)).await;
            continue;
        }
        let stat = match backend.lstat(server_id, &source).await {
            Ok(Some(stat)) => stat,
            Ok(None) => {
                push_failure(
                    snapshot,
                    failure_for(
                        None,
                        &source,
                        TransferError::Remote("source does not exist".into()),
                        None,
                    ),
                )
                .await;
                continue;
            }
            Err(error) => {
                push_failure(snapshot, failure_for(None, &source, error, None)).await;
                continue;
            }
        };
        match stat.kind {
            RemoteEntryKind::File => {
                let target = local_dir.join(&name);
                let index = allocate_file(snapshot, stat.size).await;
                if let Err(error) = download_file(
                    &context,
                    DownloadItem {
                        source: &source,
                        initial_target: &target,
                        display_name: &name,
                        initial_stat: stat,
                        index,
                    },
                    &mut decisions,
                )
                .await
                {
                    push_failure(snapshot, failure_for(Some(index), &source, error, None)).await;
                }
            }
            RemoteEntryKind::Directory => {
                let root_target = local_dir.join(&name);
                if let Err(error) = ensure_local_directory(&root_target).await {
                    push_failure(snapshot, failure_for(None, &source, error, None)).await;
                    continue;
                }
                let mut stack = vec![(source, root_target, name)];
                while let Some((remote_dir, target_dir, display_prefix)) = stack.pop() {
                    if cancel.is_cancelled() {
                        break;
                    }
                    let entries = match backend.list_dir(server_id, &remote_dir).await {
                        Ok(entries) => entries,
                        Err(error) => {
                            push_failure(snapshot, failure_for(None, &display_prefix, error, None))
                                .await;
                            continue;
                        }
                    };
                    for entry in entries.into_iter().rev() {
                        if entry.name == "." || entry.name == ".." {
                            continue;
                        }
                        if let Err(error) = safe_name(&entry.name) {
                            push_failure(snapshot, failure_for(None, &entry.name, error, None))
                                .await;
                            continue;
                        }
                        let child_remote = match remote_child(&remote_dir, &entry.name) {
                            Ok(path) => path,
                            Err(error) => {
                                push_failure(snapshot, failure_for(None, &entry.name, error, None))
                                    .await;
                                continue;
                            }
                        };
                        let child_stat = match backend.lstat(server_id, &child_remote).await {
                            Ok(Some(stat)) => stat,
                            Ok(None) => {
                                push_failure(
                                    snapshot,
                                    failure_for(
                                        None,
                                        &child_remote,
                                        TransferError::Remote(
                                            "entry disappeared during traversal".into(),
                                        ),
                                        None,
                                    ),
                                )
                                .await;
                                continue;
                            }
                            Err(error) => {
                                push_failure(
                                    snapshot,
                                    failure_for(None, &child_remote, error, None),
                                )
                                .await;
                                continue;
                            }
                        };
                        let child_target = target_dir.join(&entry.name);
                        let display = format!("{display_prefix}/{}", entry.name);
                        match child_stat.kind {
                            RemoteEntryKind::Directory => {
                                if let Err(error) = ensure_local_directory(&child_target).await {
                                    push_failure(
                                        snapshot,
                                        failure_for(None, &display, error, None),
                                    )
                                    .await;
                                } else {
                                    stack.push((child_remote, child_target, display));
                                }
                            }
                            RemoteEntryKind::File => {
                                let index = allocate_file(snapshot, child_stat.size).await;
                                if let Err(error) = download_file(
                                    &context,
                                    DownloadItem {
                                        source: &child_remote,
                                        initial_target: &child_target,
                                        display_name: &display,
                                        initial_stat: child_stat,
                                        index,
                                    },
                                    &mut decisions,
                                )
                                .await
                                {
                                    push_failure(
                                        snapshot,
                                        failure_for(Some(index), &display, error, None),
                                    )
                                    .await;
                                }
                            }
                            RemoteEntryKind::Symlink | RemoteEntryKind::Other => {
                                push_failure(
                                    snapshot,
                                    failure_for(
                                        None,
                                        &display,
                                        TransferError::InvalidInput(
                                            "symbolic links and special files are not transferred"
                                                .into(),
                                        ),
                                        None,
                                    ),
                                )
                                .await;
                            }
                        }
                    }
                }
            }
            RemoteEntryKind::Symlink | RemoteEntryKind::Other => {
                push_failure(
                    snapshot,
                    failure_for(
                        None,
                        &source,
                        TransferError::InvalidInput(
                            "symbolic links and special files are not transferred".into(),
                        ),
                        None,
                    ),
                )
                .await;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DestinationChoice {
    Skip,
    Create,
    Replace,
}

async fn allocate_file(snapshot: &SnapshotCell, bytes: u64) -> u64 {
    let mut snapshot = snapshot.write().await;
    let index = snapshot.total_files.unwrap_or(0);
    snapshot.total_files = Some(index.saturating_add(1));
    snapshot.total_bytes = Some(snapshot.total_bytes.unwrap_or(0).saturating_add(bytes));
    snapshot.touch();
    index
}

async fn begin_item(snapshot: &SnapshotCell, label: &str, total_bytes: u64) {
    let mut snapshot = snapshot.write().await;
    snapshot.current_item = Some(label.to_string());
    snapshot.current_item_bytes = 0;
    snapshot.current_item_total_bytes = Some(total_bytes);
    snapshot.touch();
}

async fn complete_item(snapshot: &SnapshotCell, verified: bool) {
    let mut snapshot = snapshot.write().await;
    snapshot.completed_files = snapshot.completed_files.saturating_add(1);
    if verified {
        snapshot.verified_files = snapshot.verified_files.saturating_add(1);
    } else {
        snapshot.unverified_files = snapshot.unverified_files.saturating_add(1);
    }
    snapshot.current_item = None;
    snapshot.current_item_bytes = 0;
    snapshot.current_item_total_bytes = None;
    snapshot.touch();
}

async fn skip_item(snapshot: &SnapshotCell) {
    let mut snapshot = snapshot.write().await;
    snapshot.skipped_files = snapshot.skipped_files.saturating_add(1);
    snapshot.current_item = None;
    snapshot.current_item_bytes = 0;
    snapshot.current_item_total_bytes = None;
    snapshot.touch();
}

async fn push_failure(snapshot: &SnapshotCell, failure: TransferItemFailure) {
    let mut snapshot = snapshot.write().await;
    if let Some(residue) = &failure.staging_residue {
        snapshot.staging_residue.push(residue.clone());
    }
    snapshot.failures.push(failure);
    snapshot.current_item = None;
    snapshot.current_item_bytes = 0;
    snapshot.current_item_total_bytes = None;
    snapshot.touch();
}

fn failure_for(
    item_index: Option<u64>,
    item: &str,
    error: TransferError,
    staging_residue: Option<String>,
) -> TransferItemFailure {
    let kind = error.failure_kind();
    TransferItemFailure {
        item_index,
        item: item.to_string(),
        kind,
        message: error.to_string(),
        staging_residue,
    }
}

fn push_recovery_failure(
    snapshot: &mut TransferSnapshot,
    item_index: u64,
    item: &str,
    kind: TransferFailureKind,
    message: String,
) {
    let item = item.chars().take(256).collect::<String>();
    let message = message.chars().take(512).collect::<String>();
    if !snapshot.failures.iter().any(|failure| {
        failure.item_index == Some(item_index) && failure.item == item && failure.message == message
    }) {
        snapshot.staging_residue.push(item.clone());
        snapshot.failures.push(TransferItemFailure {
            item_index: Some(item_index),
            item,
            kind,
            message,
            staging_residue: Some(snapshot.staging_residue.last().cloned().unwrap_or_default()),
        });
    }
    snapshot.touch();
}

async fn note_staging_residue(
    snapshot: &SnapshotCell,
    item_index: u64,
    path: &str,
    error: TransferError,
) {
    push_failure(
        snapshot,
        failure_for(Some(item_index), path, error, Some(path.to_string())),
    )
    .await;
}

async fn ensure_remote_path_chain<B: RemoteTransferBackend>(
    backend: &B,
    server_id: &str,
    path: &str,
    expect_directory: bool,
) -> Result<RemoteStat, TransferError> {
    validate_remote_path(path)?;
    let components = path
        .split('/')
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    let mut current = String::from("/");
    let mut last = None;
    for (index, component) in components.iter().enumerate() {
        current = remote_child(&current, component)?;
        let stat = backend.lstat(server_id, &current).await?.ok_or_else(|| {
            TransferError::Remote(format!("remote path does not exist: {current}"))
        })?;
        let is_final = index + 1 == components.len();
        if stat.kind == RemoteEntryKind::Symlink {
            return Err(TransferError::InvalidInput(format!(
                "symbolic links cannot be traversed during transfer: {current}"
            )));
        }
        if !is_final && stat.kind != RemoteEntryKind::Directory {
            return Err(TransferError::InvalidInput(format!(
                "remote path component is not a directory: {current}"
            )));
        }
        if is_final && expect_directory && stat.kind != RemoteEntryKind::Directory {
            return Err(TransferError::InvalidInput(format!(
                "remote path is not a directory: {current}"
            )));
        }
        last = Some(stat);
    }
    if components.is_empty() {
        let stat = backend
            .lstat(server_id, "/")
            .await?
            .ok_or_else(|| TransferError::Remote("remote root does not exist".to_string()))?;
        if stat.kind != RemoteEntryKind::Directory {
            return Err(TransferError::InvalidInput(
                "remote root is not a directory".to_string(),
            ));
        }
        return Ok(stat);
    }
    last.ok_or_else(|| TransferError::InvalidInput("remote path is empty".to_string()))
}

async fn ensure_remote_directory<B: RemoteTransferBackend>(
    backend: &B,
    server_id: &str,
    path: &str,
) -> Result<(), TransferError> {
    validate_remote_path(path)?;
    if path != "/" {
        let parent = path
            .rsplit_once('/')
            .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
            .unwrap_or("/");
        ensure_remote_path_chain(backend, server_id, parent, true).await?;
    }
    match backend.lstat(server_id, path).await? {
        Some(stat) if stat.kind == RemoteEntryKind::Directory => Ok(()),
        Some(stat) if stat.kind == RemoteEntryKind::Symlink => Err(TransferError::InvalidInput(
            format!("symbolic link cannot be used as a transfer directory: {path}"),
        )),
        Some(_) => Err(TransferError::InvalidInput(format!(
            "transfer directory path is occupied by a non-directory: {path}"
        ))),
        None => {
            if let Err(create_error) = backend.create_dir(server_id, path).await {
                // A concurrent transfer may have created the directory. Accept only a real
                // directory confirmed by lstat; preserve all other errors.
                match backend.lstat(server_id, path).await? {
                    Some(stat) if stat.kind == RemoteEntryKind::Directory => return Ok(()),
                    _ => return Err(create_error),
                }
            }
            ensure_remote_path_chain(backend, server_id, path, true).await?;
            Ok(())
        }
    }
}

async fn ensure_local_directory(path: &Path) -> Result<(), TransferError> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(TransferError::InvalidInput(format!(
                "directory destination is a symbolic link or non-directory: {}",
                "selected directory"
            )))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tokio::fs::create_dir(path)
                .await
                .map_err(TransferError::local)?;
            let metadata = tokio::fs::symlink_metadata(path)
                .await
                .map_err(TransferError::local)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(TransferError::InvalidInput(format!(
                    "directory changed during creation: {}",
                    "selected directory"
                )));
            }
            Ok(())
        }
        Err(error) => Err(TransferError::local(error)),
    }
}

async fn upload_file<B: RemoteTransferBackend>(
    context: &TransferExecutionContext<B>,
    item: UploadItem<'_>,
    decisions: &mut mpsc::UnboundedReceiver<ConflictDecision>,
) -> Result<(), TransferError> {
    let backend = context.backend.as_ref();
    let server_id = context.server_id.as_str();
    let source = item.source;
    let initial_target = item.target;
    let item_index = item.index;
    let snapshot = context.snapshot.as_ref();
    let cancel = &context.cancel;
    let (mut input, source_fingerprint) = local::open_source(source).await?;
    let size = source_fingerprint.size;
    begin_item(snapshot, item.display_name, size).await;
    let initial_stat = backend.lstat(server_id, initial_target).await?;
    let (target, choice, expected_target) =
        resolve_remote_destination(context, &item, size, initial_stat, decisions).await?;
    if choice == DestinationChoice::Skip {
        skip_item(snapshot).await;
        return Ok(());
    }

    let parent = target
        .rsplit_once('/')
        .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
        .unwrap_or("/");
    ensure_remote_path_chain(backend, server_id, parent, true).await?;
    let (stage_path, mut remote_output) = create_remote_stage(
        backend,
        server_id,
        &context.transfer_id,
        &target,
        item_index,
    )
    .await?;
    let stage_artifact = TransferStagingFile::Remote {
        server_id: server_id.to_string(),
        path: stage_path.clone(),
        item_index,
    };
    if let Err(error) = snapshot.add_staging(stage_artifact.clone()).await {
        drop(remote_output);
        cleanup_remote_stage(
            backend,
            server_id,
            &stage_path,
            item_index,
            snapshot,
            &stage_artifact,
        )
        .await;
        return Err(error);
    }
    let transfer = async {
        let (bytes, local_digest) = copy_stream(
            &mut input,
            &mut remote_output,
            StreamContext {
                item_index,
                snapshot,
                cancel,
                read_side: StreamSide::Local,
                write_side: StreamSide::Remote,
            },
        )
        .await?;
        if bytes != size {
            return Err(TransferError::SourceChanged(format!(
                "{} changed size while being uploaded (expected {size} bytes, read {bytes})",
                "selected local source"
            )));
        }
        local::verify_source_unchanged(source, &input, &source_fingerprint).await?;
        tokio::select! {
            _ = cancel.cancelled() => return Err(TransferError::Cancelled),
            result = remote_output.shutdown() => result.map_err(|error| TransferError::remote(error.to_string()))?,
        }
        let stage_stat = backend
            .lstat(server_id, &stage_path)
            .await?
            .ok_or_else(|| TransferError::Remote("remote staging file disappeared".to_string()))?;
        if stage_stat.kind != RemoteEntryKind::File || stage_stat.size != bytes {
            return Err(TransferError::Verification(
                "remote staging file size or type did not match the uploaded stream".to_string(),
            ));
        }
        let remote_digest = hash_remote_file(backend, server_id, &stage_path, cancel).await?;
        if remote_digest != local_digest {
            return Err(TransferError::Verification(
                "remote staging file SHA-256 did not match the local source".to_string(),
            ));
        }
        // The create operation itself uses atomic no-replace publication. Overwrite has no CAS;
        // compare metadata immediately before its atomic replace and disclose the remaining race.
        match choice {
            DestinationChoice::Create => {
                if backend.lstat(server_id, &target).await?.is_some() {
                    return Err(TransferError::AlreadyExists);
                }
                backend.publish_no_replace(server_id, &stage_path, &target).await?;
            }
            DestinationChoice::Replace => {
                let current = backend
                    .lstat(server_id, &target)
                    .await?
                    .ok_or_else(|| TransferError::InvalidInput("overwrite target disappeared before publication".into()))?;
                if Some(current) != expected_target {
                    return Err(TransferError::InvalidInput(
                        "overwrite target changed while the upload was staged".into(),
                    ));
                }
                backend.rename_replace(server_id, &stage_path, &target).await?;
            }
            DestinationChoice::Skip => unreachable!(),
        }
        let published = backend
            .lstat(server_id, &target)
            .await?
            .ok_or_else(|| TransferError::Verification("published remote file is missing".into()))?;
        if published.kind != RemoteEntryKind::File || published.size != bytes {
            return Err(TransferError::Verification(
                "published remote file size or type did not match the uploaded stream".to_string(),
            ));
        }
        Ok::<(), TransferError>(())
    }
    .await;
    drop(remote_output);
    if let Err(error) = transfer {
        cleanup_remote_stage(
            backend,
            server_id,
            &stage_path,
            item_index,
            snapshot,
            &stage_artifact,
        )
        .await;
        return Err(error);
    }
    if choice == DestinationChoice::Create {
        match remove_published_remote_stage(backend, server_id, &stage_path).await {
            Ok(()) => snapshot.remove_staging(&stage_artifact).await?,
            Err(error) => {
                note_staging_residue(
                    snapshot,
                    item_index,
                    &stage_path,
                    TransferError::remote(format!(
                        "remote destination was published and verified, but its staging link could not be removed: {error}"
                    )),
                )
                .await;
            }
        }
    } else {
        snapshot.remove_staging(&stage_artifact).await?;
    }
    complete_item(snapshot, true).await;
    Ok(())
}

async fn download_file<B: RemoteTransferBackend>(
    context: &TransferExecutionContext<B>,
    item: DownloadItem<'_>,
    decisions: &mut mpsc::UnboundedReceiver<ConflictDecision>,
) -> Result<(), TransferError> {
    let backend = context.backend.as_ref();
    let server_id = context.server_id.as_str();
    let snapshot = context.snapshot.as_ref();
    let cancel = &context.cancel;
    let source = item.source;
    let initial_stat = item.initial_stat;
    let item_index = item.index;
    if initial_stat.kind != RemoteEntryKind::File {
        return Err(TransferError::InvalidInput(
            "only remote regular files can be downloaded".to_string(),
        ));
    }
    begin_item(snapshot, item.display_name, initial_stat.size).await;
    let (target, choice, expected_target) =
        resolve_local_destination(context, &item, decisions).await?;
    if choice == DestinationChoice::Skip {
        skip_item(snapshot).await;
        return Ok(());
    }
    local::ensure_download_directory(target.parent().ok_or_else(|| {
        TransferError::InvalidInput("download target has no parent directory".into())
    })?)
    .await?;
    let transfer_id = snapshot.read().await.transfer_id.0.clone();
    let (stage_path, mut local_output) =
        local::create_staging_file(&target, &transfer_id, item_index).await?;
    let stage_artifact = TransferStagingFile::Local {
        path: stage_path.clone(),
        item_index,
    };
    if let Err(error) = snapshot.add_staging(stage_artifact.clone()).await {
        drop(local_output);
        if local::remove_staging(&stage_path).await.is_ok() {
            let _ = snapshot.remove_staging(&stage_artifact).await;
        }
        return Err(error);
    }

    let transfer = async {
        // SFTP v3 has no open-no-follow flag. We lstat every path component before opening and
        // check it again after opening; a malicious server-side swap can still race those checks.
        ensure_remote_path_chain(backend, server_id, source, false).await?;
        let mut remote_input = backend.open_read(server_id, source).await?;
        let after_open = backend
            .lstat(server_id, source)
            .await?
            .ok_or_else(|| TransferError::Remote("remote source disappeared after open".into()))?;
        if after_open != initial_stat || after_open.kind != RemoteEntryKind::File {
            return Err(TransferError::SourceChanged(source.to_string()));
        }
        let (bytes, streamed_digest) = copy_stream(
            &mut remote_input,
            &mut local_output,
            StreamContext {
                item_index,
                snapshot,
                cancel,
                read_side: StreamSide::Remote,
                write_side: StreamSide::Local,
            },
        )
        .await?;
        if bytes != initial_stat.size {
            return Err(TransferError::SourceChanged(format!(
                "{source} changed size while being downloaded (expected {} bytes, read {bytes})",
                initial_stat.size
            )));
        }
        tokio::select! {
            _ = cancel.cancelled() => return Err(TransferError::Cancelled),
            result = local_output.flush() => result.map_err(TransferError::local)?,
        }
        tokio::select! {
            _ = cancel.cancelled() => return Err(TransferError::Cancelled),
            result = local_output.sync_all() => result.map_err(TransferError::local)?,
        }
        drop(local_output);
        let staged_digest = hash_local_file(&stage_path, cancel).await?;
        if staged_digest != streamed_digest {
            return Err(TransferError::Verification(
                "local staging file SHA-256 did not match the downloaded stream".to_string(),
            ));
        }
        // The remote endpoint does not provide an independent digest. Read it a second time and
        // compare the digest before publishing the local target.
        let second_digest = hash_remote_file(backend, server_id, source, cancel).await?;
        if second_digest != streamed_digest {
            return Err(TransferError::SourceChanged(format!(
                "{source} changed between download verification reads"
            )));
        }
        let final_remote = backend
            .lstat(server_id, source)
            .await?
            .ok_or_else(|| TransferError::SourceChanged(source.to_string()))?;
        if final_remote != initial_stat {
            return Err(TransferError::SourceChanged(source.to_string()));
        }
        match choice {
            DestinationChoice::Create => {
                if local::path_fingerprint_if_regular(&target).await?.is_some() {
                    return Err(TransferError::AlreadyExists);
                }
                local::publish_local_no_replace(&stage_path, &target).await?;
            }
            DestinationChoice::Replace => {
                local::publish_local_replace(
                    &stage_path,
                    &target,
                    expected_target.as_ref().ok_or_else(|| {
                        TransferError::InvalidInput("overwrite target guard is missing".into())
                    })?,
                )
                .await?;
            }
            DestinationChoice::Skip => unreachable!(),
        }
        Ok::<(), TransferError>(())
    }
    .await;
    if let Err(error) = transfer {
        if let Err(cleanup_error) = local::remove_staging(&stage_path).await {
            let residue = stage_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("local staging file")
                .to_string();
            note_staging_residue(snapshot, item_index, &residue, cleanup_error).await;
        } else {
            let _ = snapshot.remove_staging(&stage_artifact).await;
        }
        return Err(error);
    }
    snapshot.remove_staging(&stage_artifact).await?;
    complete_item(snapshot, true).await;
    Ok(())
}

async fn resolve_remote_destination<B: RemoteTransferBackend>(
    context: &TransferExecutionContext<B>,
    item: &UploadItem<'_>,
    incoming_size: u64,
    existing: Option<RemoteStat>,
    decisions: &mut mpsc::UnboundedReceiver<ConflictDecision>,
) -> Result<(String, DestinationChoice, Option<RemoteStat>), TransferError> {
    let backend = context.backend.as_ref();
    let server_id = context.server_id.as_str();
    let initial_target = item.target;
    let source_name = item.display_name;
    let item_index = item.index;
    let policy = &context.policy;
    let Some(existing) = existing else {
        return Ok((initial_target.to_string(), DestinationChoice::Create, None));
    };
    if existing.kind != RemoteEntryKind::File {
        return Err(TransferError::InvalidInput(
            "remote transfer target is occupied by a directory, symbolic link, or special file"
                .into(),
        ));
    }
    if matches!(policy, ConflictPolicy::Overwrite) && !backend.supports_atomic_replace() {
        return Err(TransferError::Unsupported(
            "this SFTP backend does not advertise an atomic overwrite rename".into(),
        ));
    }
    let mut allowed_actions = vec![ConflictActionKind::Skip, ConflictActionKind::Rename];
    if backend.supports_atomic_replace() {
        allowed_actions.push(ConflictActionKind::Overwrite);
    }
    let action = match policy {
        ConflictPolicy::Skip => ConflictAction::Skip,
        ConflictPolicy::Overwrite => ConflictAction::Overwrite,
        ConflictPolicy::Ask => {
            await_conflict(
                context.snapshot.as_ref(),
                &context.cancel,
                decisions,
                ConflictRequest {
                    item_index,
                    source_name: source_name.to_string(),
                    target_name: initial_target.to_string(),
                    existing_size: existing.size,
                    incoming_size,
                    existing_modified_epoch_seconds: existing.modified.map(u64::from),
                    allowed_actions,
                },
            )
            .await?
        }
    };
    match action {
        ConflictAction::Skip => Ok((
            initial_target.to_string(),
            DestinationChoice::Skip,
            Some(existing),
        )),
        ConflictAction::Overwrite if backend.supports_atomic_replace() => Ok((
            initial_target.to_string(),
            DestinationChoice::Replace,
            Some(existing),
        )),
        ConflictAction::Overwrite => Err(TransferError::Unsupported(
            "this SFTP backend does not advertise an atomic overwrite rename".into(),
        )),
        ConflictAction::Rename { name } => {
            let parent = initial_target
                .rsplit_once('/')
                .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
                .unwrap_or("/");
            let renamed = remote_child(parent, &name)?;
            if backend.lstat(server_id, &renamed).await?.is_some() {
                return Err(TransferError::InvalidInput(
                    "the renamed remote target already exists".to_string(),
                ));
            }
            Ok((renamed, DestinationChoice::Create, None))
        }
    }
}

async fn resolve_local_destination<B: RemoteTransferBackend>(
    context: &TransferExecutionContext<B>,
    item: &DownloadItem<'_>,
    decisions: &mut mpsc::UnboundedReceiver<ConflictDecision>,
) -> Result<(PathBuf, DestinationChoice, Option<local::LocalFingerprint>), TransferError> {
    let initial_target = item.initial_target;
    let source_name = item.display_name;
    let incoming = item.initial_stat;
    let item_index = item.index;
    let policy = &context.policy;
    let existing = local::path_fingerprint_if_regular(initial_target).await?;
    let Some(existing) = existing else {
        return Ok((
            initial_target.to_path_buf(),
            DestinationChoice::Create,
            None,
        ));
    };
    let action = match policy {
        ConflictPolicy::Skip => ConflictAction::Skip,
        ConflictPolicy::Overwrite => ConflictAction::Overwrite,
        ConflictPolicy::Ask => {
            await_conflict(
                context.snapshot.as_ref(),
                &context.cancel,
                decisions,
                ConflictRequest {
                    item_index,
                    source_name: source_name.to_string(),
                    target_name: initial_target
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default()
                        .to_string(),
                    existing_size: existing.size,
                    incoming_size: incoming.size,
                    existing_modified_epoch_seconds: existing.modified.and_then(|time| {
                        time.duration_since(UNIX_EPOCH)
                            .ok()
                            .map(|duration| duration.as_secs())
                    }),
                    allowed_actions: vec![
                        ConflictActionKind::Skip,
                        ConflictActionKind::Overwrite,
                        ConflictActionKind::Rename,
                    ],
                },
            )
            .await?
        }
    };
    match action {
        ConflictAction::Skip => Ok((
            initial_target.to_path_buf(),
            DestinationChoice::Skip,
            Some(existing),
        )),
        ConflictAction::Overwrite => Ok((
            initial_target.to_path_buf(),
            DestinationChoice::Replace,
            Some(existing),
        )),
        ConflictAction::Rename { name } => {
            safe_name(&name)?;
            let parent = initial_target.parent().ok_or_else(|| {
                TransferError::InvalidInput("download target has no parent directory".into())
            })?;
            let renamed = parent.join(name);
            if local::path_fingerprint_if_regular(&renamed)
                .await?
                .is_some()
            {
                return Err(TransferError::InvalidInput(
                    "the renamed local target already exists".to_string(),
                ));
            }
            Ok((renamed, DestinationChoice::Create, None))
        }
    }
}

async fn await_conflict(
    snapshot: &SnapshotCell,
    cancel: &CancellationToken,
    decisions: &mut mpsc::UnboundedReceiver<ConflictDecision>,
    conflict: ConflictRequest,
) -> Result<ConflictAction, TransferError> {
    let item_index = conflict.item_index;
    {
        let mut snapshot = snapshot.write().await;
        snapshot.status = TransferStatus::WaitingConflict;
        snapshot.active_conflict = Some(conflict);
        snapshot.touch();
    }
    let action = loop {
        tokio::select! {
            _ = cancel.cancelled() => return Err(TransferError::Cancelled),
            decision = decisions.recv() => match decision {
                Some(decision) if decision.item_index == item_index => break decision.action,
                Some(_) => continue,
                None => return Err(TransferError::Cancelled),
            },
        }
    };
    {
        let mut snapshot = snapshot.write().await;
        snapshot.status = TransferStatus::Running;
        snapshot.active_conflict = None;
        snapshot.touch();
    }
    Ok(action)
}

async fn create_remote_stage<B: RemoteTransferBackend>(
    backend: &B,
    server_id: &str,
    transfer_id: &TransferId,
    target: &str,
    item_index: u64,
) -> Result<(String, super::types::TransferWriter), TransferError> {
    let (parent, name) = target.rsplit_once('/').unwrap_or(("", target));
    let parent = if parent.is_empty() { "/" } else { parent };
    for attempt in 0..MAX_STAGING_ATTEMPTS {
        let stage_name = format!(
            ".{name}.yukinal-{}-{item_index}-{attempt}.part",
            transfer_id.0.replace('-', "")
        );
        let stage_path = remote_child(parent, &stage_name)?;
        if backend.lstat(server_id, &stage_path).await?.is_some() {
            continue;
        }
        match backend.create_exclusive(server_id, &stage_path).await {
            Ok(writer) => return Ok((stage_path, writer)),
            Err(TransferError::AlreadyExists) => continue,
            Err(error) => return Err(error),
        }
    }
    Err(TransferError::Unsupported(
        "could not allocate a unique remote staging file after 32 attempts".to_string(),
    ))
}

async fn cleanup_remote_stage<B: RemoteTransferBackend>(
    backend: &B,
    server_id: &str,
    stage_path: &str,
    item_index: u64,
    snapshot: &SnapshotCell,
    artifact: &TransferStagingFile,
) {
    match backend.lstat(server_id, stage_path).await {
        Ok(None) => {
            let _ = snapshot.remove_staging(artifact).await;
            return;
        }
        Ok(Some(stat)) if stat.kind == RemoteEntryKind::File => {}
        Ok(Some(_)) => {
            note_staging_residue(
                snapshot,
                item_index,
                stage_path,
                TransferError::InvalidInput(
                    "remote staging path no longer refers to a regular file".into(),
                ),
            )
            .await;
            return;
        }
        Err(error) => {
            note_staging_residue(snapshot, item_index, stage_path, error).await;
            return;
        }
    }
    match backend.remove_file(server_id, stage_path).await {
        Ok(()) => {
            let _ = snapshot.remove_staging(artifact).await;
        }
        Err(error) => note_staging_residue(snapshot, item_index, stage_path, error).await,
    }
}

async fn remove_published_remote_stage<B: RemoteTransferBackend>(
    backend: &B,
    server_id: &str,
    stage_path: &str,
) -> Result<(), TransferError> {
    match backend.lstat(server_id, stage_path).await? {
        None => Ok(()),
        Some(stat) if stat.kind == RemoteEntryKind::File => {
            backend.remove_file(server_id, stage_path).await
        }
        Some(_) => Err(TransferError::InvalidInput(
            "remote staging path no longer refers to a regular file".into(),
        )),
    }
}

#[derive(Debug, Clone, Copy)]
enum StreamSide {
    Local,
    Remote,
}

struct StreamContext<'a> {
    item_index: u64,
    snapshot: &'a SnapshotCell,
    cancel: &'a CancellationToken,
    read_side: StreamSide,
    write_side: StreamSide,
}

impl StreamSide {
    fn error(self, error: std::io::Error) -> TransferError {
        match self {
            Self::Local => TransferError::local(error),
            Self::Remote => TransferError::remote(error.to_string()),
        }
    }
}

async fn copy_stream<R, W>(
    reader: &mut R,
    writer: &mut W,
    context: StreamContext<'_>,
) -> Result<(u64, [u8; 32]), TransferError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let StreamContext {
        item_index,
        snapshot,
        cancel,
        read_side,
        write_side,
    } = context;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    loop {
        let count = tokio::select! {
            _ = cancel.cancelled() => return Err(TransferError::Cancelled),
            result = reader.read(&mut buffer) => result.map_err(|error| read_side.error(error))?,
        };
        if count == 0 {
            break;
        }
        tokio::select! {
            _ = cancel.cancelled() => return Err(TransferError::Cancelled),
            result = writer.write_all(&buffer[..count]) => result.map_err(|error| write_side.error(error))?,
        }
        hasher.update(&buffer[..count]);
        total = total.saturating_add(count as u64);
        let mut snapshot = snapshot.write().await;
        if snapshot
            .active_conflict
            .as_ref()
            .is_some_and(|conflict| conflict.item_index == item_index)
        {
            // Progress events are snapshots; the UI can recover the same values with `get`.
        }
        snapshot.current_item_bytes = snapshot.current_item_bytes.saturating_add(count as u64);
        snapshot.transferred_bytes = snapshot.transferred_bytes.saturating_add(count as u64);
        snapshot.touch();
    }
    let digest: [u8; 32] = hasher.finalize().into();
    Ok((total, digest))
}

async fn hash_remote_file<B: RemoteTransferBackend>(
    backend: &B,
    server_id: &str,
    path: &str,
    cancel: &CancellationToken,
) -> Result<[u8; 32], TransferError> {
    let mut reader = backend.open_read(server_id, path).await?;
    hash_reader(&mut reader, cancel, StreamSide::Remote).await
}

async fn hash_local_file(
    path: &Path,
    cancel: &CancellationToken,
) -> Result<[u8; 32], TransferError> {
    let (mut file, _) = local::open_source(path).await?;
    hash_reader(&mut file, cancel, StreamSide::Local).await
}

async fn hash_reader<R: AsyncRead + Unpin>(
    reader: &mut R,
    cancel: &CancellationToken,
    side: StreamSide,
) -> Result<[u8; 32], TransferError> {
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut hasher = Sha256::new();
    loop {
        let count = tokio::select! {
            _ = cancel.cancelled() => return Err(TransferError::Cancelled),
            result = reader.read(&mut buffer) => result.map_err(|error| side.error(error))?,
        };
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize().into())
}
