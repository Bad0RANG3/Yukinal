/** `package.inspect` — read one normalized package-manager state. */

import { PackageInspectInputSchema, PackageInspectResultSchema } from "@yukinal/shared";

import { hostBackedTool, type HostToolExecutor } from "./host-backed.js";

export function packageInspectTool(host: HostToolExecutor) {
  return hostBackedTool(host, {
    name: "package.inspect",
    description: "Read whether one bounded apt or dnf package is installed and return its normalized version.",
    timeoutMs: 10_000,
    input: PackageInspectInputSchema,
    output: PackageInspectResultSchema,
  });
}
