/** `docker.logs` — read a bounded tail of one remote container's logs. */

import { DockerLogsInputSchema, DockerLogsResultSchema } from "@yukinal/shared";

import { hostBackedTool, type HostToolExecutor } from "./host-backed.js";

export function dockerLogsTool(host: HostToolExecutor) {
  return hostBackedTool(host, {
    name: "docker.logs",
    description: "Read a bounded, timestamped tail of logs for one resolved Docker container.",
    timeoutMs: 15_000,
    input: DockerLogsInputSchema,
    output: DockerLogsResultSchema,
  });
}
