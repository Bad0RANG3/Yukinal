/** `server.info` — read a fresh, host-collected snapshot for the resolved server. */

import { ServerSnapshotSchema } from "@yukinal/shared";
import { z } from "zod";

import { hostBackedTool, type HostToolExecutor } from "./host-backed.js";

const input = z.strictObject({});

export function serverInfoTool(host: HostToolExecutor) {
  return hostBackedTool(host, {
    name: "server.info",
    description: "Collect a fresh read-only health snapshot for the resolved remote server.",
    timeoutMs: 20_000,
    input,
    output: ServerSnapshotSchema,
  });
}
