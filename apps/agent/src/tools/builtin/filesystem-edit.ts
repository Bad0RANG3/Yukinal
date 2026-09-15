/** `filesystem.edit` — guarded exact-match replacement (read-then-modify) after permission evaluation. */

import { FilesystemEditInputSchema, FilesystemEditResultSchema } from "./filesystem-schemas.js";
import { hostBackedTool, type HostToolExecutor } from "./host-backed.js";

export function filesystemEditTool(host: HostToolExecutor) {
  return hostBackedTool(host, {
    name: "filesystem.edit",
    // The description is the only place the model learns the precondition, the limits and what the
    // guard does *not* promise, so all three are stated here rather than assumed.
    description:
      "Change part of an existing remote text file, guarded against clobbering someone else's write. " +
      "The host reads the file, refuses unless its content still hashes to the expectedRevision returned " +
      "by filesystem.read, refuses unless oldString occurs in it exactly once, replaces that one occurrence " +
      "with newString, and publishes it through a same-directory staging file and a rename in the same " +
      "operation. Returns the new revision (usable as the expectedRevision of the next edit) plus the byte " +
      "and line deltas. " +
      "A missing or repeated oldString, and a stale revision, come back as invalid_input: re-read the file " +
      "and retry with more context / the new revision. Files larger than 512 KiB and edits that would grow " +
      "the file past 512 KiB are refused — only a prefix could be read, so writing it back would truncate " +
      "the file; use filesystem.write deliberately for those. " +
      "The host keeps the file's mode, mtime, owner and group, and the replacement is only published when " +
      "the file still matches what was read: a symlink, a file with other hard links, a server whose link " +
      "count cannot be determined, a server that refuses the rename, metadata that cannot be carried over, " +
      "and a file that changes while the edit is staged all come back as unsupported (nothing to retry) or " +
      "invalid_input (re-read and retry) with nothing written. The guard is still check-then-replace rather " +
      "than compare-and-swap: a change within the same second and the same size cannot be told apart, so " +
      "re-read when the file may be busy. filesystem.write is the deliberate in-place overwrite for the " +
      "cases this tool refuses.",
    risk: "medium",
    // One host operation, but the edit reads, stages, syncs and renames over SFTP, so its budget is
    // larger than a single write's.
    timeoutMs: 30_000,
    input: FilesystemEditInputSchema,
    output: FilesystemEditResultSchema,
  });
}
