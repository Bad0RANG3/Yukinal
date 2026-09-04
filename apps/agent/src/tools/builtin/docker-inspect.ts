/** `docker.inspect` — read normalized metadata for one remote container. */

import { DockerInspectInputSchema, DockerInspectResultSchema } from "@yukinal/shared";

import { hostBackedTool, type HostToolExecutor } from "./host-backed.js";

export function dockerInspectTool(host: HostToolExecutor) {
  return hostBackedTool(host, {
    name: "docker.inspect",
    description: "Read normalized state, image, restart count, and health for one Docker container.",
    timeoutMs: 10_000,
    input: DockerInspectInputSchema,
    output: DockerInspectResultSchema,
  });
}
