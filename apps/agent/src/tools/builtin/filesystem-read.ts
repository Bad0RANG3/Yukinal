/** `filesystem.read` — bounded read of one absolute path on the resolved server. */

import { FilesystemReadInputSchema } from "@yukinal/shared";

import { FilesystemReadResultSchema } from "./filesystem-schemas.js";
import { hostBackedTool, type HostToolExecutor } from "./host-backed.js";

export function filesystemReadTool(host: HostToolExecutor) {
  return hostBackedTool(host, {
    name: "filesystem.read",
    description:
      "Read a bounded text file from the resolved remote server without changing state. " +
      "Returns the content plus a revision: the SHA-256 of the exact bytes read, which filesystem.edit " +
      "requires as its precondition. The read is bounded (128 KiB by default, at most 1 MiB through " +
      "maxBytes) and sets truncated: true when the file is longer; a truncated read's revision describes " +
      "only the prefix that was returned, so it can never authorize an edit — read the whole file (within " +
      "the cap) before editing. Only files up to 512 KiB can be edited at all. " +
      "Use filesystem.edit to change part of an existing file, filesystem.write to create one or to " +
      "replace one deliberately.",
    timeoutMs: 15_000,
    input: FilesystemReadInputSchema,
    output: FilesystemReadResultSchema,
  });
}
