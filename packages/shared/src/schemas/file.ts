import { z } from "zod";
import { TRANSFER_CONFLICT_ACTIONS, TRANSFER_DIRECTIONS, TRANSFER_STATUSES } from "../types/file.js";

const BoundedByteCountSchema = z.number().int().min(0).max(Number.MAX_SAFE_INTEGER);
const TransferRenameNameSchema = z.string().trim().min(1).max(255).refine(
  (name) => name !== "." && name !== ".." && !/[\\/\u0000-\u001f\u007f]/.test(name),
  "rename must be a single safe file name",
);

export const LocalPathHandleSchema = z.strictObject({
  handleId: z.string().trim().min(16).max(128),
  name: z.string().min(1).max(512),
  kind: z.enum(["file", "directory"]),
  size: BoundedByteCountSchema.optional(),
});

export const LocalFileDropEventSchema = z.strictObject({
  handles: z.array(LocalPathHandleSchema).max(256),
  rejectedCount: z.number().int().nonnegative().max(256),
});

export const FilePreviewResponseSchema = z.strictObject({
  name: z.string().min(1).max(1_024),
  size: BoundedByteCountSchema,
  kind: z.enum(["text", "image", "binary"]),
  truncated: z.boolean(),
  text: z.string().max(1024 * 1024).nullable(),
  dataUrl: z.string().max(2 * 1024 * 1024).nullable(),
}).superRefine((preview, context) => {
  if (preview.kind === "text" && preview.text === null) {
    context.addIssue({ code: z.ZodIssueCode.custom, path: ["text"], message: "text previews require text" });
  }
  if (preview.kind === "image" && preview.dataUrl === null) {
    context.addIssue({ code: z.ZodIssueCode.custom, path: ["dataUrl"], message: "image previews require a data URL" });
  }
  if (preview.kind === "binary" && (preview.text !== null || preview.dataUrl !== null)) {
    context.addIssue({ code: z.ZodIssueCode.custom, message: "binary previews carry metadata only" });
  }
});

export const PreparedRemoteDragSchema = z.strictObject({
  dragId: z.string().regex(/^remote_drag_[a-f0-9]{64}$/),
  name: z.string().min(1).max(1_024),
  size: BoundedByteCountSchema,
});

export const TransferConflictRequestSchema = z.strictObject({
  itemIndex: z.number().int().nonnegative(),
  sourceName: z.string().min(1).max(1_024),
  targetName: z.string().min(1).max(1_024),
  existingSize: BoundedByteCountSchema,
  incomingSize: BoundedByteCountSchema,
  existingModifiedEpochSeconds: z.number().int().nonnegative().nullable(),
  allowedActions: z.array(z.enum(TRANSFER_CONFLICT_ACTIONS)).min(1).max(3),
});

export const TransferConflictActionSchema = z.discriminatedUnion("action", [
  z.strictObject({ action: z.literal("skip") }),
  z.strictObject({ action: z.literal("overwrite") }),
  z.strictObject({ action: z.literal("rename"), name: TransferRenameNameSchema }),
]);

export const TransferItemFailureSchema = z.strictObject({
  itemIndex: z.number().int().nonnegative().optional(),
  item: z.string().min(1).max(1_024),
  kind: z.enum(["invalidInput", "localIo", "remoteIo", "verification", "unsupported"]),
  message: z.string().min(1).max(2_048),
  stagingResidue: z.string().max(4_096).optional(),
});

export const TransferSnapshotSchema = z.strictObject({
  transferId: z.string().trim().min(1).max(128),
  serverId: z.string().trim().min(1).max(256),
  direction: z.enum(TRANSFER_DIRECTIONS),
  status: z.enum(TRANSFER_STATUSES),
  startedAtEpochMs: BoundedByteCountSchema,
  updatedAtEpochMs: BoundedByteCountSchema,
  totalFiles: BoundedByteCountSchema.nullable(),
  completedFiles: BoundedByteCountSchema,
  skippedFiles: BoundedByteCountSchema,
  totalBytes: BoundedByteCountSchema.nullable(),
  transferredBytes: BoundedByteCountSchema,
  currentItem: z.string().max(1_024).nullable(),
  currentItemBytes: BoundedByteCountSchema,
  currentItemTotalBytes: BoundedByteCountSchema.nullable(),
  activeConflict: TransferConflictRequestSchema.nullable(),
  verifiedFiles: BoundedByteCountSchema,
  unverifiedFiles: BoundedByteCountSchema,
  failures: z.array(TransferItemFailureSchema).max(1_024),
  stagingResidue: z.array(z.string().min(1).max(4_096)).max(1_024),
});

const RemoteAbsolutePathSchema = z
  .string()
  .min(1)
  .max(4096)
  .regex(/^\//, "remote path must be absolute");

/**
 * 内容摘要：SHA-256 的小写十六进制。
 *
 * 大小写不宽松是有意的：`ContentRevisionSchema`（Agent 侧）说了理由 —— 上层的比较是
 * 字节比较，若这里接受大写，同一份内容就会出现两种合法写法，而「大写是否等于没变」这种
 * 问题不该由每个调用点回答。
 */
const ContentRevisionSchema = z
  .string()
  .regex(/^[0-9a-f]{64}$/, "content revision must be the 64-character lowercase hex SHA-256");

export const FilesystemReadInputSchema = z.strictObject({
  path: RemoteAbsolutePathSchema,
  maxBytes: z.number().int().min(1).max(1024 * 1024).optional(),
});

export const FilesystemReadOutputSchema = z.strictObject({
  path: RemoteAbsolutePathSchema,
  content: z.string(),
  truncated: z.boolean(),
  /** 这次返回字节的内容摘要，供 `filesystem.edit` 的 `expectedRevision` 使用。 */
  revision: ContentRevisionSchema,
});

export const FilesystemEditInputSchema = z.strictObject({
  path: RemoteAbsolutePathSchema,
  /**
   * `filesystem.read` 给出的摘要。宿主拿它和**当前**文件比，不一致就拒绝这次编辑，
   * 而不是拿旧内容去覆盖新内容。
   */
  expectedRevision: ContentRevisionSchema,
  oldString: z.string().min(1).max(512 * 1024),
  newString: z.string().max(512 * 1024),
});

export const FilesystemEditOutputSchema = z.strictObject({
  path: RemoteAbsolutePathSchema,
  /** 替换后的内容摘要，让模型可以连续改同一个文件而不必重新读一遍。 */
  revision: ContentRevisionSchema,
  bytesBefore: z.number().int().nonnegative(),
  bytesAfter: z.number().int().nonnegative(),
  /**
   * 行数变化，可正可负。它存在是因为「改了 3 个字节」和「删了一整段」对下一步判断的
   * 意义完全不同，而工具返回的小结是模型唯一的依据。
   */
  lineDelta: z.number().int(),
});

export const FilesystemWriteInputSchema = z.strictObject({
  path: RemoteAbsolutePathSchema,
  content: z.string().max(512 * 1024),
});

export const FilesystemWriteOutputSchema = z.strictObject({
  path: RemoteAbsolutePathSchema,
  bytesWritten: z.number().int().nonnegative(),
});

export const FilesystemBackupInputSchema = z.strictObject({
  path: RemoteAbsolutePathSchema,
});

export const FilesystemBackupOutputSchema = z.strictObject({
  path: RemoteAbsolutePathSchema,
  backupPath: RemoteAbsolutePathSchema,
  revision: ContentRevisionSchema,
  bytesBackedUp: z.number().int().nonnegative(),
});

export const FilesystemRestoreInputSchema = z.strictObject({
  path: RemoteAbsolutePathSchema,
  backupPath: RemoteAbsolutePathSchema,
  expectedRevision: ContentRevisionSchema,
});

export const FilesystemRestoreOutputSchema = z.strictObject({
  path: RemoteAbsolutePathSchema,
  backupPath: RemoteAbsolutePathSchema,
  revision: ContentRevisionSchema,
  bytesBefore: z.number().int().nonnegative(),
  bytesAfter: z.number().int().nonnegative(),
});

export const FilesystemBackupCleanupItemSchema = z.strictObject({
  path: RemoteAbsolutePathSchema,
  backupPath: RemoteAbsolutePathSchema,
  expectedRevision: ContentRevisionSchema,
});

/**
 * One exact cleanup, or a bounded batch from an approved rotation step.
 *
 * A union rather than a widened object keeps the single-item callers unchanged and makes
 * "both a single item and an items array" unrepresentable.
 */
export const FilesystemBackupCleanupInputSchema = z.union([
  FilesystemBackupCleanupItemSchema,
  z.strictObject({
    items: z.array(FilesystemBackupCleanupItemSchema).min(1).max(32),
  }),
]);

export const FilesystemBackupCleanupOutputSchema = z.strictObject({
  path: RemoteAbsolutePathSchema,
  backupPath: RemoteAbsolutePathSchema,
  revision: ContentRevisionSchema,
  bytesDeleted: z.number().int().nonnegative(),
});

export const FilesystemBackupCleanupBatchItemSchema = z.strictObject({
  path: RemoteAbsolutePathSchema,
  backupPath: RemoteAbsolutePathSchema,
  outcome: z.enum(["removed", "skipped", "failed"]),
  revision: ContentRevisionSchema.optional(),
  bytesDeleted: z.number().int().nonnegative().optional(),
  reason: z.string().min(1).max(1_024).optional(),
});

export const FilesystemBackupCleanupBatchOutputSchema = z.strictObject({
  items: z.array(FilesystemBackupCleanupBatchItemSchema).max(32),
  /** True whenever any requested item was not removed, so partial success is visible. */
  partial: z.boolean(),
});

const FilesystemBackupStatusSchema = z.enum(["available", "restored", "deleted"]);

export const FilesystemBackupListInputSchema = z.strictObject({
  status: FilesystemBackupStatusSchema.optional(),
  path: RemoteAbsolutePathSchema.optional(),
  limit: z.number().int().min(1).max(128).optional(),
});

export const FilesystemBackupLedgerItemSchema = z.strictObject({
  id: z.string().trim().min(1).max(256),
  serverId: z.string().trim().min(1).max(256),
  taskId: z.string().trim().min(1).max(256),
  path: RemoteAbsolutePathSchema,
  backupPath: RemoteAbsolutePathSchema,
  revision: ContentRevisionSchema,
  bytesBackedUp: z.number().int().nonnegative().max(1024 * 1024),
  status: FilesystemBackupStatusSchema,
  createdAt: z.string().trim().min(1).max(80),
  updatedAt: z.string().trim().min(1).max(80),
  restoredAt: z.string().trim().min(1).max(80).optional(),
  deletedAt: z.string().trim().min(1).max(80).optional(),
});

export const FilesystemBackupListOutputSchema = z.strictObject({
  backups: z.array(FilesystemBackupLedgerItemSchema).max(128),
  truncated: z.boolean(),
});

export const FilesystemBackupRetentionCandidateSchema = z.strictObject({
  path: RemoteAbsolutePathSchema,
  backupPath: RemoteAbsolutePathSchema,
  revision: ContentRevisionSchema,
  taskId: z.string().trim().min(1).max(256).optional(),
  createdAt: z.string().trim().min(1).max(80),
  bytesBackedUp: z.number().int().nonnegative().max(1024 * 1024),
});

export const FilesystemBackupRetentionInputSchema = z
  .strictObject({
    pathPrefix: RemoteAbsolutePathSchema.optional(),
    keepLatest: z.number().int().min(1).max(64).optional(),
    olderThanDays: z.number().int().min(1).max(3650).optional(),
  })
  .superRefine((value, context) => {
    if (value.keepLatest === undefined && value.olderThanDays === undefined) {
      context.addIssue({
        code: z.ZodIssueCode.custom,
        path: ["keepLatest"],
        message: "keepLatest or olderThanDays is required",
      });
    }
  });

export const FilesystemBackupRetentionOutputSchema = z.strictObject({
  candidates: z.array(FilesystemBackupRetentionCandidateSchema).max(32),
  truncated: z.boolean(),
  keptCount: z.number().int().nonnegative(),
  scannedCount: z.number().int().nonnegative(),
});
