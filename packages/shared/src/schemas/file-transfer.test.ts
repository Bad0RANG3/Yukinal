import assert from "node:assert/strict";
import test from "node:test";

import { LocalPathHandleSchema, TransferConflictActionSchema, TransferSnapshotSchema } from "./file.js";

test("native local file references expose an opaque handle rather than a path", () => {
  assert.equal(
    LocalPathHandleSchema.safeParse({ handleId: "local_handle_0123456789", name: "report.txt", kind: "file", size: 12 }).success,
    true,
  );
  assert.equal(
    LocalPathHandleSchema.safeParse({ handleId: "local_handle_0123456789", name: "report.txt", kind: "file", path: "C:\\private\\report.txt" }).success,
    false,
  );
});

test("transfer conflict actions are explicit and snapshots report verifiable progress", () => {
  assert.equal(TransferConflictActionSchema.safeParse({ action: "overwrite" }).success, true);
  assert.equal(TransferConflictActionSchema.safeParse({ action: "rename", name: "copy.txt" }).success, true);
  assert.equal(TransferConflictActionSchema.safeParse({ action: "rename", name: "../outside" }).success, false);

  const snapshot = {
    transferId: "xfer_0123",
    serverId: "srv_0123456789abcdef",
    direction: "upload",
    status: "waitingConflict",
    startedAtEpochMs: 1_800_000_000_000,
    updatedAtEpochMs: 1_800_000_000_100,
    totalFiles: 2,
    completedFiles: 0,
    skippedFiles: 0,
    totalBytes: 128,
    transferredBytes: 64,
    currentItem: "report.txt",
    currentItemBytes: 64,
    currentItemTotalBytes: 64,
    activeConflict: {
      itemIndex: 0,
      sourceName: "report.txt",
      targetName: "report.txt",
      existingSize: 10,
      incomingSize: 64,
      existingModifiedEpochSeconds: null,
      allowedActions: ["skip", "overwrite", "rename"],
    },
    verifiedFiles: 0,
    unverifiedFiles: 0,
    failures: [],
    stagingResidue: [],
  };
  assert.equal(TransferSnapshotSchema.safeParse(snapshot).success, true);
  assert.equal(TransferSnapshotSchema.safeParse({ ...snapshot, localPath: "C:\\private\\report.txt" }).success, false);
});
