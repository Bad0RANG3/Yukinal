/** `systemd.inspect` — read one normalized systemd service state. */

import { SystemdInspectInputSchema, SystemdInspectResultSchema } from "@yukinal/shared";

import { hostBackedTool, type HostToolExecutor } from "./host-backed.js";

export function systemdInspectTool(host: HostToolExecutor) {
  return hostBackedTool(host, {
    name: "systemd.inspect",
    description: "Read the normalized load, active and sub-state of one bounded .service unit.",
    timeoutMs: 10_000,
    input: SystemdInspectInputSchema,
    output: SystemdInspectResultSchema,
  });
}
