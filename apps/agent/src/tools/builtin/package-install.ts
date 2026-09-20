/** `package.install` — install one exact package spec after high-risk approval. */

import { PackageInstallInputSchema, PackageInstallResultSchema } from "@yukinal/shared";

import { hostBackedTool, type HostToolExecutor } from "./host-backed.js";

export function packageInstallTool(host: HostToolExecutor) {
  return hostBackedTool(host, {
    name: "package.install",
    description: "Install one bounded apt or dnf package without sudo or free-form flags; explicit approval is always required.",
    risk: "high",
    effectful: true,
    timeoutMs: 600_000,
    input: PackageInstallInputSchema,
    output: PackageInstallResultSchema,
  });
}
