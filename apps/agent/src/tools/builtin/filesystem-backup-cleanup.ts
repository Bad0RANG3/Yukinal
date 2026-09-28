/**
 * `filesystem.backup.cleanup` — remove verified host-owned recovery copies.
 *
 * Output is either the single-item result or the batch result; a batch always reports the
 * fate of every requested item and sets `partial` when anything was not removed.
 */

import {
  FilesystemBackupCleanupBatchOutputSchema,
  FilesystemBackupCleanupInputSchema,
  FilesystemBackupCleanupOutputSchema,
} from "@yukinal/shared";

import { z } from "zod";

import { hostBackedTool, type HostToolExecutor } from "./host-backed.js";

export function filesystemBackupCleanupTool(host: HostToolExecutor) {
  return hostBackedTool(host, {
    name: "filesystem.backup.cleanup",
    description:
      "Delete host-owned sibling backups after explicit approval. Either one exact backup " +
      "(path, backupPath, expectedRevision) or a bounded items array from an approved " +
      "rotation step. Each backup must still be in the host ledger for this server, its bytes " +
      "must match expectedRevision, and a successful cleanup consumes the ledger entry. Batch " +
      "results report every item as removed/skipped/failed and set partial when any item was " +
      "not removed; it cannot delete an arbitrary remote path.",
    risk: "medium",
    effectful: true,
    timeoutMs: 60_000,
    input: FilesystemBackupCleanupInputSchema,
    output: z.union([FilesystemBackupCleanupOutputSchema, FilesystemBackupCleanupBatchOutputSchema]),
  });
}
