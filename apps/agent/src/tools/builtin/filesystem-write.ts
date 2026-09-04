/** `filesystem.write` — overwrite one bounded text file after permission evaluation. */

import { FilesystemWriteInputSchema, FilesystemWriteOutputSchema } from "@yukinal/shared";

import { hostBackedTool, type HostToolExecutor } from "./host-backed.js";

export function filesystemWriteTool(host: HostToolExecutor) {
  return hostBackedTool(host, {
    name: "filesystem.write",
    description: "Overwrite one bounded text file on the resolved remote server after permission approval.",
    risk: "medium",
    timeoutMs: 20_000,
    input: FilesystemWriteInputSchema,
    output: FilesystemWriteOutputSchema,
  });
}
