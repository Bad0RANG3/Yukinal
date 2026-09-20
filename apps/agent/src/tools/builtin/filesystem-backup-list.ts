/** `filesystem.backup.list` — read the host-owned recovery ledger for this task. */

import { FilesystemBackupListInputSchema, FilesystemBackupListOutputSchema } from "@yukinal/shared";

import { hostBackedTool, type HostToolExecutor } from "./host-backed.js";

export function filesystemBackupListTool(host: HostToolExecutor) {
  return hostBackedTool(host, {
    name: "filesystem.backup.list",
    description:
      "Read the host-owned remote backup ledger for the current investigation task. This returns " +
      "bounded metadata only; an available row is not proof that the remote sibling still exists. " +
      "Use the result to explain possible restore or cleanup choices. It never deletes, restores, " +
      "or probes a remote path.",
    risk: "read",
    timeoutMs: 10_000,
    input: FilesystemBackupListInputSchema,
    output: FilesystemBackupListOutputSchema,
  });
}
