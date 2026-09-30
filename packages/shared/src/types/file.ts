export const REMOTE_FILE_TYPES = ["file", "directory", "symlink", "other"] as const;
export type RemoteFileType = (typeof REMOTE_FILE_TYPES)[number];

export interface RemoteFileEntry {
  name: string;
  path: string;
  type: RemoteFileType;
  size: number;
}

export interface RemoteFileListResponse { path: string; entries: RemoteFileEntry[]; }
export interface RemoteFileReadResponse { path: string; content: string; truncated: boolean; }

export type FilePreviewKind = "text" | "image" | "binary";

/** Preview bytes are separately capped; this is never used for full file transfer. */
export interface FilePreviewResponse {
  name: string;
  size: number;
  kind: FilePreviewKind;
  truncated: boolean;
  text: string | null;
  dataUrl: string | null;
}

/** Opaque host-owned staging ticket for starting an OS-native remote file drag. */
export interface PreparedRemoteDrag {
  dragId: string;
  name: string;
  size: number;
}

/** Opaque, short-lived reference to a path selected or dropped in the native shell. */
export interface LocalPathHandle {
  handleId: string;
  name: string;
  kind: "file" | "directory";
  size?: number;
}

export interface LocalFileDropEvent {
  handles: LocalPathHandle[];
  rejectedCount: number;
}

export const TRANSFER_DIRECTIONS = ["upload", "download"] as const;
export type TransferDirection = (typeof TRANSFER_DIRECTIONS)[number];

export const TRANSFER_STATUSES = [
  "queued", "running", "waitingConflict", "completed", "partial", "failed", "cancelled", "interrupted",
] as const;
export type TransferStatus = (typeof TRANSFER_STATUSES)[number];

export const TRANSFER_CONFLICT_ACTIONS = ["skip", "overwrite", "rename"] as const;
export type TransferConflictActionName = (typeof TRANSFER_CONFLICT_ACTIONS)[number];

export interface TransferConflictRequest {
  itemIndex: number;
  sourceName: string;
  targetName: string;
  existingSize: number;
  incomingSize: number;
  existingModifiedEpochSeconds: number | null;
  allowedActions: TransferConflictActionName[];
}

export type TransferConflictAction =
  | { action: "skip" }
  | { action: "overwrite" }
  | { action: "rename"; name: string };

export interface TransferItemFailure {
  itemIndex?: number;
  item: string;
  kind: "invalidInput" | "localIo" | "remoteIo" | "verification" | "unsupported";
  message: string;
  stagingResidue?: string;
}

/** Progress contains metadata only; file bytes and absolute local paths stay in Rust. */
export interface TransferSnapshot {
  transferId: string;
  serverId: string;
  direction: TransferDirection;
  status: TransferStatus;
  startedAtEpochMs: number;
  updatedAtEpochMs: number;
  totalFiles: number | null;
  completedFiles: number;
  skippedFiles: number;
  totalBytes: number | null;
  transferredBytes: number;
  currentItem: string | null;
  currentItemBytes: number;
  currentItemTotalBytes: number | null;
  activeConflict: TransferConflictRequest | null;
  verifiedFiles: number;
  unverifiedFiles: number;
  failures: TransferItemFailure[];
  stagingResidue: string[];
}

/** Agent-facing remote file tools. The host enforces the same bounds again. */
export interface FilesystemReadInput {
  path: string;
  maxBytes?: number;
}

export interface FilesystemReadOutput {
  path: string;
  content: string;
  truncated: boolean;
  /**
   * 这次读到的**字节**的内容摘要：SHA-256 的 64 位小写十六进制。
   *
   * 它是 `filesystem.edit` 的 `expectedRevision`，也就是「我看到的还是不是我看到的那份」
   * 的凭据。两条必须一起读的性质：
   *
   * - `truncated` 为 `true` 时它描述的是**前缀**，而不是整个文件。宿主不会让这样的摘要
   *   授权一次编辑，所以截断过的读不能直接拿去改。
   * - 它是**内容**摘要，不是版本号：两个不同的写入如果落成同样的字节，摘要相同。宿主
   *   事后可以再校验一次「文件现在是不是还是这个摘要」，但那只是把竞争窗口收窄
   *   （检查与写入之间仍有间隙），不是原子操作。
   */
  revision: string;
}

export interface FilesystemWriteInput {
  path: string;
  content: string;
}

export interface FilesystemWriteOutput {
  path: string;
  bytesWritten: number;
}

/** Create a host-owned sibling backup of one complete regular file. */
export interface FilesystemBackupInput {
  path: string;
}

export interface FilesystemBackupOutput {
  path: string;
  backupPath: string;
  revision: string;
  bytesBackedUp: number;
}

/**
 * Agent 侧的读-改-写：拿刚读到的摘要去换一次精确替换。
 *
 * `expectedRevision` 是**唯一**防止覆盖别人改动的机制，所以它不是可选项。宿主在写入前
 * 会比较它与当前文件的摘要；不一致就拒绝，并把两个摘要一起回给模型（`expectedRevision`
 * 与 `actualRevision`），让它重读后再试，而不是替它猜一个折中。
 */
export interface FilesystemEditInput {
  path: string;
  expectedRevision: string;
  /** 必须**恰好一次**出现在文件里；出现零次或多次都拒绝，绝不「取第一个」。 */
  oldString: string;
  newString: string;
}

export interface FilesystemEditOutput {
  path: string;
  /** 替换之后的摘要。 */
  revision: string;
  bytesBefore: number;
  bytesAfter: number;
  /** 行数变化，可正可负。 */
  lineDelta: number;
}

/** Restore a host-owned backup only when the target still has the expected revision. */
export interface FilesystemRestoreInput {
  path: string;
  backupPath: string;
  expectedRevision: string;
}

export interface FilesystemRestoreOutput {
  path: string;
  backupPath: string;
  revision: string;
  bytesBefore: number;
  bytesAfter: number;
}

/** Delete a host-owned backup only when its bytes still match the recorded revision. */
export interface FilesystemBackupCleanupItem {
  path: string;
  backupPath: string;
  expectedRevision: string;
}

/**
 * Delete one exact backup, or a bounded batch produced by an approved `backup_rotation`
 * step. The single-item object is the shape every existing caller already sends, so it is
 * kept as one branch of the union rather than a breaking change.
 */
export type FilesystemBackupCleanupInput =
  | FilesystemBackupCleanupItem
  | { items: FilesystemBackupCleanupItem[] };

export interface FilesystemBackupCleanupOutput {
  path: string;
  backupPath: string;
  revision: string;
  bytesDeleted: number;
}

/** One item's fate in a batch cleanup. Never reports `removed` without a verified delete. */
export interface FilesystemBackupCleanupBatchItem {
  path: string;
  backupPath: string;
  outcome: "removed" | "skipped" | "failed";
  revision?: string;
  bytesDeleted?: number;
  reason?: string;
}

/**
 * A batch cleanup's result. `partial` is true whenever any requested item was not removed,
 * so a partially successful batch can never be read as a full success.
 */
export interface FilesystemBackupCleanupBatchOutput {
  items: FilesystemBackupCleanupBatchItem[];
  partial: boolean;
}

export const FILESYSTEM_BACKUP_STATUSES = ["available", "restored", "deleted"] as const;
export type FilesystemBackupStatus = (typeof FILESYSTEM_BACKUP_STATUSES)[number];

/** Read the host-owned backup ledger for the current investigation task. */
export interface FilesystemBackupListInput {
  status?: FilesystemBackupStatus;
  path?: string;
  limit?: number;
}

export interface FilesystemBackupLedgerItem {
  id: string;
  serverId: string;
  taskId: string;
  path: string;
  backupPath: string;
  revision: string;
  bytesBackedUp: number;
  status: FilesystemBackupStatus;
  createdAt: string;
  updatedAt: string;
  restoredAt?: string;
  deletedAt?: string;
}

export interface FilesystemBackupListOutput {
  backups: FilesystemBackupLedgerItem[];
  truncated: boolean;
}

/**
 * Plan a cross-task backup rotation without touching the remote target. Both filters are
 * optional, but at least one must be given; `keepLatest` counts newest-first per original
 * path, `olderThanDays` is an absolute age cutoff, and when both are given a record must
 * satisfy **both** (intersection) to become a candidate.
 */
export interface FilesystemBackupRetentionInput {
  pathPrefix?: string;
  keepLatest?: number;
  olderThanDays?: number;
}

/** One backup the host would delete if a `backup_rotation` step were approved. */
export interface FilesystemBackupRetentionCandidate {
  path: string;
  backupPath: string;
  revision: string;
  taskId?: string;
  createdAt: string;
  bytesBackedUp: number;
}

export interface FilesystemBackupRetentionOutput {
  candidates: FilesystemBackupRetentionCandidate[];
  /** True when the scan hit its row bound or the candidate list was capped. */
  truncated: boolean;
  /** Available records that are not candidates. */
  keptCount: number;
  /** Available records that matched the request (before grouping/candidate selection). */
  scannedCount: number;
}
