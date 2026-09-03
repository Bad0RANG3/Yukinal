/**
 * Sidecar entry point (ADR 0001). Nothing but wiring: the runtime lives in
 * `runtime/create-runtime.ts`, the protocol lives in `rpc/` and `transport/`.
 */

import { AGENT_VERSION, createLogger, readConfig } from "./config.js";
import { BUILTIN_POLICY_IDS, createRuntime } from "./runtime/create-runtime.js";
import { startStdioRpc, type StdioServer } from "./transport/stdio.js";

let server: StdioServer | undefined;

export function main(): void {
  const config = readConfig();
  const log = createLogger({ level: config.logLevel, scope: "agent" });
  const runtime = createRuntime({ log });

  server = startStdioRpc({
    router: runtime.router,
    log: log.child("stdio"),
    // No parent stdin means the desktop is gone: exit instead of orphaning.
    onParentGone: () => shutdown("stdin-closed"),
  });

  log.info("ready", {
    version: AGENT_VERSION,
    tools: runtime.declarations.map((declaration) => declaration.name),
    policies: BUILTIN_POLICY_IDS,
    dataDir: config.dataDir === "" ? "(unset)" : config.dataDir,
    maxRunMs: config.maxRunMs,
  });
}

export function shutdown(reason: string): void {
  server?.close();
  server = undefined;
  process.stderr.write(`agent sidecar shutting down (${reason})\n`);
  // In-flight cancellation handling lands with the agent loop.
  setTimeout(() => {
    process.exit(0);
  }, 50).unref();
}

for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.on(signal, () => {
    shutdown(signal);
  });
}

main();
