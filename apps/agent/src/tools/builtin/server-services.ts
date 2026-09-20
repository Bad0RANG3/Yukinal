/** `server.services` — discover bounded systemd or Docker service state. */

import { ServerServicesInputSchema, ServerServicesResponseSchema } from "@yukinal/shared";

import { hostBackedTool, type HostToolExecutor } from "./host-backed.js";

export function serverServicesTool(host: HostToolExecutor) {
  return hostBackedTool(host, {
    name: "server.services",
    description: "Discover bounded read-only systemd or Docker service status on the resolved server. Optionally keep one exact service/container name or normalized state.",
    timeoutMs: 20_000,
    input: ServerServicesInputSchema,
    output: ServerServicesResponseSchema,
  });
}
