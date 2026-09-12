#!/usr/bin/env node
/**
 * Smoke the agent the way an *installed* app launches it.
 *
 * `smoke-sidecar.mjs` proves the bundle speaks the protocol while it still sits in the
 * repository, where `apps/agent/node_modules` and the workspace checkout are one `..`
 * away. That is not what the user gets: the installer ships a small `<resource_dir>/agent/`
 * directory, staged by `bundle.resources` in
 * `apps/desktop/src-tauri/tauri.conf.json`, with no `node_modules` anywhere above it.
 *
 * So this stages **exactly what that map says** into a throwaway directory, then runs the
 * same assertions against the staged entry. A leftover bare specifier (`import "zod"`), a
 * relative import that esbuild left unresolved, or a path resolved against the source tree
 * fails here and nowhere else.
 *
 * Reading the map instead of hardcoding one file is not tidiness: this script previously
 * copied `index.js` alone with a comment saying "not even a package.json", and when the
 * map gained a second entry (`agent/package.json`, which declares the staged bundle's
 * module system) the smoke kept testing a layout the installer no longer produces. The
 * map is the single source of truth for the installed shape; deriving the staging from it
 * means the two cannot disagree again.
 */

import { copyFile, mkdir, mkdtemp, rm } from "node:fs/promises";
import { existsSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { runSidecarSmoke } from "./lib/sidecar-smoke.mjs";

const repo = fileURLToPath(new URL("..", import.meta.url));
const tauriDir = join(repo, "apps", "desktop", "src-tauri");
const confPath = join(tauriDir, "tauri.conf.json");

let resources;
try {
  resources = JSON.parse(readFileSync(confPath, "utf8"))?.bundle?.resources;
} catch (error) {
  console.error(`✗ cannot read bundle.resources from ${confPath}: ${error.message}`);
  process.exit(2);
}
if (resources === null || typeof resources !== "object" || Array.isArray(resources)) {
  console.error(`✗ ${confPath} has no bundle.resources map to stage`);
  process.exit(2);
}

const staged = await mkdtemp(join(tmpdir(), "yukinal-packaged-"));
const entry = join(staged, "agent", "index.js");

try {
  for (const [source, destination] of Object.entries(resources)) {
    // Sources are relative to src-tauri, which is where the bundler runs.
    const from = resolve(tauriDir, source);
    const to = join(staged, String(destination).replace(/\\/g, "/"));
    if (!existsSync(from)) {
      console.error(`✗ bundle.resources maps "${source}", which does not exist (${from})`);
      console.error("  build it first: pnpm --filter @yukinal/agent build");
      process.exit(2);
    }
    await mkdir(dirname(to), { recursive: true });
    await copyFile(from, to);
  }

  await runSidecarSmoke(entry, { label: `${entry} (resource layout, no node_modules)` });
} finally {
  await rm(staged, { recursive: true, force: true });
}
