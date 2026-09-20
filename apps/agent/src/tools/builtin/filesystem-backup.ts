/** `filesystem.backup` — create one host-owned sibling recovery copy. */

import { FilesystemBackupInputSchema, FilesystemBackupOutputSchema } from "@yukinal/shared";

import { hostBackedTool, type HostToolExecutor } from "./host-backed.js";

export function filesystemBackupTool(host: HostToolExecutor) {
  return hostBackedTool(host, {
    name: "filesystem.backup",
    description:
      "Create a complete, bounded backup of one existing regular remote file before a change. " +
      "The host chooses a unique sibling backup path; the model cannot choose an arbitrary destination " +
      "or overwrite an older backup. The host also records its server, target, task and revision in a " +
      "durable ledger. Returns that path and the source revision for a later, separately approved " +
      "filesystem.restore.",
    risk: "medium",
    effectful: true,
    timeoutMs: 30_000,
    input: FilesystemBackupInputSchema,
    output: FilesystemBackupOutputSchema,
  });
}
