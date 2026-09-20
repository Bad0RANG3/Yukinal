/** `server.logs` — read the bounded system log source selected by the host. */

import { ServerLogsInputSchema, ServerLogsResponseSchema } from "@yukinal/shared";

import { hostBackedTool, type HostToolExecutor } from "./host-backed.js";

export function serverLogsTool(host: HostToolExecutor) {
  return hostBackedTool(host, {
    name: "server.logs",
    description: "Read a bounded, read-only tail from the resolved server's journal or system log. Optionally filter journalctl to the last 24 hours and one exact .service unit.",
    timeoutMs: 20_000,
    input: ServerLogsInputSchema,
    output: ServerLogsResponseSchema,
  });
}
