/** `filesystem.write` — overwrite one bounded text file after permission evaluation. */

import { FilesystemWriteInputSchema, FilesystemWriteOutputSchema } from "@yukinal/shared";

import { hostBackedTool, type HostToolExecutor } from "./host-backed.js";

export function filesystemWriteTool(host: HostToolExecutor) {
  return hostBackedTool(host, {
    name: "filesystem.write",
    description:
      "Replace one whole bounded text file on the resolved remote server after permission approval. " +
      "This is a blind overwrite: what you send becomes the entire file, and anything changed since you " +
      "last read it is gone. It stays that way on purpose — use it to create a file, or when you really do " +
      "mean to replace all of it, and use filesystem.edit when you are changing part of an existing file " +
      "(it verifies the revision you read and replaces one exact match). Content is capped at 512 KiB.",
    risk: "medium",
    timeoutMs: 20_000,
    input: FilesystemWriteInputSchema,
    output: FilesystemWriteOutputSchema,
  });
}
