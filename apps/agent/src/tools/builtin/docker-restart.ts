/** `docker.restart` — restart one remote container after high-risk approval. */

import { DockerRestartInputSchema, DockerRestartResultSchema } from "@yukinal/shared";

import { hostBackedTool, type HostToolExecutor } from "./host-backed.js";

export function dockerRestartTool(host: HostToolExecutor) {
  return hostBackedTool(host, {
    name: "docker.restart",
    description: "Restart one Docker container on the resolved remote server after explicit approval.",
    risk: "high",
    timeoutMs: 30_000,
    input: DockerRestartInputSchema,
    output: DockerRestartResultSchema,
  });
}
