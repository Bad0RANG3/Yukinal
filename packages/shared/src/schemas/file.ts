import { z } from "zod";

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
