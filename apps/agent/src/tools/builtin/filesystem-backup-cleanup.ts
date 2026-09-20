/** `filesystem.backup.cleanup` — remove one verified host-owned recovery copy. */

import {
  FilesystemBackupCleanupInputSchema,
  FilesystemBackupCleanupOutputSchema,
} from "@yukinal/shared";

import { hostBackedTool, type HostToolExecutor } from "./host-backed.js";

export function filesystemBackupCleanupTool(host: HostToolExecutor) {
  return hostBackedTool(host, {
    name: "filesystem.backup.cleanup",
    description:
      "Delete one host-owned sibling backup after explicit approval. The backup must still be in the " +
      "host ledger for this server and task, its bytes must match expectedRevision, and a successful " +
      "cleanup consumes the ledger entry. It cannot delete an arbitrary remote path.",
    risk: "medium",
    effectful: true,
    timeoutMs: 30_000,
    input: FilesystemBackupCleanupInputSchema,
    output: FilesystemBackupCleanupOutputSchema,
  });
}
