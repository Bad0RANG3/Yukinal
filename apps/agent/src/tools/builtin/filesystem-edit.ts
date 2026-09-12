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
      "with newString and writes the file back in the same operation. Returns the new revision (usable as " +
      "the expectedRevision of the next edit) plus the byte and line deltas. " +
      "A missing or repeated oldString, and a stale revision, come back as invalid_input: re-read the file " +
      "and retry with more context / the new revision. Files larger than 512 KiB and edits that would grow " +
      "the file past 512 KiB are refused — only a prefix could be read, so writing it back would truncate " +
      "the file; use filesystem.write deliberately for those. " +
      "The guard is check-then-write, not atomic: SFTP has no compare-and-swap, so a writer that changes " +
      "the file between the check and the write still gets overwritten — re-read when the file may be busy.",
    risk: "medium",
    // One host operation, but two SFTP round trips (read then write), so the budget is larger than a
    // single write's.
    timeoutMs: 30_000,
    input: FilesystemEditInputSchema,
    output: FilesystemEditResultSchema,
  });
}
