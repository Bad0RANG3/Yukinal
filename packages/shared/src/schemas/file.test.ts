import assert from "node:assert/strict";
import test from "node:test";

import {
  FilesystemBackupInputSchema,
  FilesystemBackupCleanupInputSchema,
  FilesystemBackupCleanupOutputSchema,
  FilesystemBackupListInputSchema,
  FilesystemBackupListOutputSchema,
  FilesystemBackupOutputSchema,
  FilesystemBackupRetentionInputSchema,
  FilesystemBackupRetentionOutputSchema,
  FilesystemRestoreInputSchema,
  FilesystemRestoreOutputSchema,
} from "./file.js";

const REVISION = "a".repeat(64);
const BACKUP = "/etc/.yukinal-backup-0123456789abcdef-0123456789abcdef0123456789abcdef";

test("filesystem backup contracts keep the source and generated path bounded", () => {
  assert.equal(FilesystemBackupInputSchema.safeParse({ path: "/etc/yukinal.conf" }).success, true);
  assert.equal(FilesystemBackupInputSchema.safeParse({ path: "relative.conf" }).success, false);
  assert.equal(
    FilesystemBackupOutputSchema.safeParse({
      path: "/etc/yukinal.conf",
      backupPath: BACKUP,
      revision: REVISION,
      bytesBackedUp: 42,
    }).success,
    true,
  );
});

test("filesystem restore requires a revision guard and rejects drifted output", () => {
  assert.equal(
    FilesystemRestoreInputSchema.safeParse({
      path: "/etc/yukinal.conf",
      backupPath: BACKUP,
      expectedRevision: REVISION,
    }).success,
    true,
  );
  assert.equal(
    FilesystemRestoreInputSchema.safeParse({
      path: "/etc/yukinal.conf",
      backupPath: BACKUP,
      expectedRevision: "bad",
    }).success,
    false,
  );
  assert.equal(
    FilesystemRestoreOutputSchema.safeParse({
      path: "/etc/yukinal.conf",
      backupPath: BACKUP,
      revision: REVISION,
      bytesBefore: 14,
      bytesAfter: 13,
      extra: true,
    }).success,
    false,
  );
});

test("filesystem backup cleanup accepts one exact item or a bounded batch", () => {
  const item = { path: "/etc/yukinal.conf", backupPath: BACKUP, expectedRevision: REVISION };
  assert.equal(FilesystemBackupCleanupInputSchema.safeParse(item).success, true);
  assert.equal(
    FilesystemBackupCleanupInputSchema.safeParse({
      items: [item, { ...item, backupPath: `${BACKUP}b` }],
    }).success,
    true,
  );
  assert.equal(FilesystemBackupCleanupInputSchema.safeParse({ items: [] }).success, false);
  // The two forms are a strict union: neither may carry the other shape's fields.
  assert.equal(FilesystemBackupCleanupInputSchema.safeParse({ ...item, items: [item] }).success, false);
  assert.equal(
    FilesystemBackupCleanupInputSchema.safeParse({
      items: Array.from({ length: 33 }, (_, index) => ({ ...item, backupPath: `${BACKUP}${index}` })),
    }).success,
    false,
  );
  assert.equal(
    FilesystemBackupCleanupOutputSchema.safeParse({
      path: item.path,
      backupPath: item.backupPath,
      revision: REVISION,
      bytesDeleted: 12,
    }).success,
    true,
  );
});

test("filesystem backup retention requires a filter and bounds its output", () => {
  assert.equal(FilesystemBackupRetentionInputSchema.safeParse({}).success, false);
  assert.equal(FilesystemBackupRetentionInputSchema.safeParse({ keepLatest: 3 }).success, true);
  assert.equal(FilesystemBackupRetentionInputSchema.safeParse({ olderThanDays: 30 }).success, true);
  assert.equal(FilesystemBackupRetentionInputSchema.safeParse({ keepLatest: 65 }).success, false);
  assert.equal(FilesystemBackupRetentionInputSchema.safeParse({ olderThanDays: 0 }).success, false);
  const candidate = {
    path: "/etc/yukinal.conf",
    backupPath: BACKUP,
    revision: REVISION,
    createdAt: "2026-01-01T00:00:00Z",
    bytesBackedUp: 12,
  };
  assert.equal(
    FilesystemBackupRetentionOutputSchema.safeParse({
      candidates: [candidate],
      truncated: false,
      keptCount: 1,
      scannedCount: 2,
    }).success,
    true,
  );
  assert.equal(
    FilesystemBackupRetentionOutputSchema.safeParse({
      candidates: [],
      truncated: false,
      keptCount: 0,
      scannedCount: 0,
      extra: true,
    }).success,
    false,
  );
});

test("filesystem backup cleanup requires the source binding and revision", () => {
  assert.equal(
    FilesystemBackupCleanupInputSchema.safeParse({
      path: "/etc/yukinal.conf",
      backupPath: BACKUP,
      expectedRevision: REVISION,
    }).success,
    true,
  );
  assert.equal(
    FilesystemBackupCleanupInputSchema.safeParse({
      path: "/etc/yukinal.conf",
      backupPath: BACKUP,
      expectedRevision: "short",
    }).success,
    false,
  );
  assert.equal(
    FilesystemBackupCleanupOutputSchema.safeParse({
      path: "/etc/yukinal.conf",
      backupPath: BACKUP,
      revision: REVISION,
      bytesDeleted: 42,
      extra: true,
    }).success,
    false,
  );
});

test("filesystem backup listing is bounded and exposes ledger metadata only", () => {
  assert.equal(
    FilesystemBackupListInputSchema.safeParse({ status: "available", path: "/etc/yukinal.conf", limit: 8 }).success,
    true,
  );
  assert.equal(FilesystemBackupListInputSchema.safeParse({ status: "unknown" }).success, false);
  assert.equal(
    FilesystemBackupListOutputSchema.safeParse({
      backups: [{
        id: "backup_1",
        serverId: "srv_1",
        taskId: "task_1",
        path: "/etc/yukinal.conf",
        backupPath: BACKUP,
        revision: REVISION,
        bytesBackedUp: 42,
        status: "available",
        createdAt: "2026-09-20T00:00:00Z",
        updatedAt: "2026-09-20T00:00:00Z",
      }],
      truncated: false,
    }).success,
    true,
  );
});
