/** `systemd.restart` — restart one systemd service after high-risk approval. */

import { SystemdRestartInputSchema, SystemdRestartResultSchema } from "@yukinal/shared";

import { hostBackedTool, type HostToolExecutor } from "./host-backed.js";

export function systemdRestartTool(host: HostToolExecutor) {
  return hostBackedTool(host, {
    name: "systemd.restart",
    description: "Restart one bounded systemd .service unit on the resolved remote server after explicit approval.",
    risk: "high",
    effectful: true,
    timeoutMs: 30_000,
    input: SystemdRestartInputSchema,
    output: SystemdRestartResultSchema,
  });
}
