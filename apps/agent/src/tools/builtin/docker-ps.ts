/** `docker.ps` — read the container list from the resolved remote server. */

import { ContainerInfoSchema } from "@yukinal/shared";
import { z } from "zod";

import { hostBackedTool, type HostToolExecutor } from "./host-backed.js";

const input = z.strictObject({ all: z.boolean().optional() });
const output = z.strictObject({
  available: z.boolean(),
  containers: z.array(ContainerInfoSchema),
});

export function dockerPsTool(host: HostToolExecutor) {
  return hostBackedTool(host, {
    name: "docker.ps",
    description: "List Docker containers on the resolved remote server without changing state.",
    timeoutMs: 10_000,
    input,
    output,
  });
}
