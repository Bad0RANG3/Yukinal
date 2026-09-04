/** `filesystem.read` — bounded read of one absolute path on the resolved server. */

import { FilesystemReadInputSchema, FilesystemReadOutputSchema } from "@yukinal/shared";

import { hostBackedTool, type HostToolExecutor } from "./host-backed.js";

export function filesystemReadTool(host: HostToolExecutor) {
  return hostBackedTool(host, {
    name: "filesystem.read",
    description: "Read a bounded text file from the resolved remote server without changing state.",
    timeoutMs: 15_000,
    input: FilesystemReadInputSchema,
    output: FilesystemReadOutputSchema,
  });
}
