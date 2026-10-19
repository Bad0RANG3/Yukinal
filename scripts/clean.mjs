#!/usr/bin/env node
/**
 * Remove build output. Never touches databases or credential stores.
 *
 * `target/` and the frozen-Agent staging directory are included because both are
 * pure build output now: `target/` holds the packaging bundle, probe logs and
 * PyInstaller work directory, and `agent/runtime` is rewritten by every
 * `pnpm build:python-agent` (which also clears the onedir and the licence notice
 * itself). Leaving them behind used to mean a stale notice or stale payload could
 * survive a "clean" and be copied into the next package. The tracked
 * `agent/runtime/.gitkeep` is preserved so the directory stays in the repository.
 *
 * Paths are joined as filesystem paths rather than built with `new URL`: a `new URL`
 * treats `#`, `?` and `%` as fragment/query/escape syntax, so a directory whose name
 * contained one would resolve to a *different* path and delete the wrong thing. The
 * glob is also given an explicit `cwd`; without it the patterns were matched against
 * the process working directory and silently removed nothing when run from elsewhere.
 */
import { glob, rm } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const targets = [
  "packages/*/dist",
  "apps/*/dist",
  "target",
  "agent/runtime/THIRD_PARTY_LICENSES.txt",
  "agent/runtime/yukinal-agent",
];

for (const pattern of targets) {
  for await (const target of glob(pattern, { cwd: root })) {
    await rm(join(root, target), { recursive: true, force: true });
    console.log(`removed ${target}`);
  }
}
