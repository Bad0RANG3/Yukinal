#!/usr/bin/env node
/**
 * End-to-end smoke test of the sidecar over its real transport (ADR 0001 / ADR 0006).
 *
 * It runs the **built bundle** -- `apps/agent/dist/index.js`, the exact file the installer
 * ships as `<resource_dir>/agent/index.js` -- the way Rust launches it (`node <entry>`,
 * nothing else on the command line). Running the `tsx src/index.ts` dev path here would
 * test a process the user never gets: a multi-file `tsc` build resolves `zod`,
 * `@yukinal/shared` and `@yukinal/provider-sdk` through `node_modules`, which a packaged
 * app does not have. If the bundle regresses, this step has to be the one that goes red.
 *
 * Build it first (`pnpm --filter @yukinal/agent build`; `pnpm check` does that in order).
 * Usage: node scripts/smoke-sidecar.mjs [entry.js]
 */

import { existsSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { runSidecarSmoke } from "./lib/sidecar-smoke.mjs";

const entry = process.argv[2]
  ? resolve(process.argv[2])
  : fileURLToPath(new URL("../apps/agent/dist/index.js", import.meta.url));

if (!existsSync(entry)) {
  console.error(`✗ no sidecar bundle at ${entry}`);
  console.error("  build it first: pnpm --filter @yukinal/agent build");
  process.exit(2);
}

await runSidecarSmoke(entry);
