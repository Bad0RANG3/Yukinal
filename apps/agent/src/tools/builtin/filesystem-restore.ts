/** `filesystem.restore` — guarded high-risk restore from a host-owned backup. */

import { FilesystemRestoreInputSchema, FilesystemRestoreOutputSchema } from "@yukinal/shared";

import { hostBackedTool, type HostToolExecutor } from "./host-backed.js";

export function filesystemRestoreTool(host: HostToolExecutor) {
  return hostBackedTool(host, {
    name: "filesystem.restore",
    description:
      "Restore one remote file from a host-generated sibling backup after explicit approval. " +
      "The target must still have expectedRevision, the backupPath must come from the host's durable " +
      "filesystem.backup ledger for the same server, target and task, and the host publishes the bytes " +
      "through its guarded atomic replacement. A successful restore consumes the ledger entry; it never " +
      "accepts an arbitrary destination or silently overwrites later edits.",
    risk: "high",
    effectful: true,
    timeoutMs: 30_000,
    input: FilesystemRestoreInputSchema,
    output: FilesystemRestoreOutputSchema,
  });
}
