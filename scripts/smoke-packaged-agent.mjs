#!/usr/bin/env node
/**
 * Smoke the Python Agent the way an *installed* app launches it.
 *
 * `smoke-sidecar.mjs` covers the legacy Node development route. This script stages the
 * Tauri resource map and runs the frozen Python Agent from that isolated installed shape.
 *
 * The Python executable must start without Python on PATH, load its provider/MCP imports,
 * and complete the sidecar protocol smoke from the staged resource directory.
 *
 * The map is the single source of truth for the installed shape; deriving the staging from
 * it means the smoke and installer cannot silently disagree.
 */

import { copyFile, cp, mkdir, mkdtemp, rm, stat } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repo = fileURLToPath(new URL("..", import.meta.url));
const tauriDir = join(repo, "apps", "desktop", "src-tauri");
const confPath = join(tauriDir, "tauri.conf.json");

let resources;
try {
  resources = JSON.parse(readFileSync(confPath, "utf8"))?.bundle?.resources;
} catch (error) {
  console.error(`✗ cannot read bundle.resources from ${confPath}: ${error.message}`);
  throw error;
}
if (resources === null || typeof resources !== "object" || Array.isArray(resources)) {
  throw new Error(`${confPath} has no bundle.resources map to stage`);
}

const staged = await mkdtemp(join(tmpdir(), "yukinal-packaged-"));

try {
  for (const [source, destination] of Object.entries(resources)) {
    // Sources are relative to src-tauri, which is where the bundler runs.
    const from = resolve(tauriDir, source);
    const to = join(staged, String(destination).replace(/\\/g, "/"));
    if (!existsSync(from)) {
      throw new Error(`bundle.resources maps "${source}", which does not exist (${from}); build the Python Agent and C++ SSH service first`);
    }
    await mkdir(dirname(to), { recursive: true });
    if ((await stat(from)).isDirectory()) await cp(from, to, { recursive: true });
    else await copyFile(from, to);
  }

  const smoke = spawnSync(process.execPath, [join(repo, "scripts", "smoke-installed-agent.mjs"), staged], {
    cwd: repo,
    stdio: "inherit",
    windowsHide: true,
  });
  if (smoke.error) throw smoke.error;
  if (smoke.status !== 0) throw new Error(`installed Python Agent smoke failed (exit ${smoke.status ?? 1})`);
} finally {
  await rm(staged, { recursive: true, force: true });
}
