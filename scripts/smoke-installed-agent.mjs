#!/usr/bin/env node
/**
 * Exercise the Agent using only a package's installed resource directory and its bundled
 * Node.js runtime. The host Node.js runs this harness; the Agent itself is started by the
 * runtime shipped inside the app.
 */

import { execFileSync } from "node:child_process";
import { mkdtemp, rm, stat, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { runSidecarSmoke } from "./lib/sidecar-smoke.mjs";

const [resourceRootArg, ...extraArgs] = process.argv.slice(2);
if (!resourceRootArg || extraArgs.length > 0) {
  console.error("usage: node scripts/smoke-installed-agent.mjs <installed-resource-directory>");
  process.exit(2);
}

const resourceRoot = resolve(resourceRootArg);
const runtime = join(resourceRoot, "runtime", process.platform === "win32" ? "node.exe" : "node");
const entry = join(resourceRoot, "agent", "index.js");
const requiredResources = [
  runtime,
  entry,
  join(resourceRoot, "agent", "package.json"),
  join(resourceRoot, "runtime", "LICENSE"),
  join(resourceRoot, "NOTICE"),
];
const runtimeManifestPath = fileURLToPath(new URL("./node-runtime.json", import.meta.url));
const runtimeManifest = JSON.parse(await readFile(runtimeManifestPath, "utf8"));

for (const requiredPath of requiredResources) {
  const info = await stat(requiredPath).catch(() => null);
  if (!info?.isFile()) throw new Error(`installed package is missing a required file: ${requiredPath}`);
}

const runtimeVersion = execFileSync(runtime, ["--version"], { encoding: "utf8" }).trim();
if (runtimeVersion !== `v${runtimeManifest.version}`) {
  throw new Error(`installed Node.js runtime is ${runtimeVersion}; expected v${runtimeManifest.version}`);
}
console.log(`✓ installed Node.js runtime ${runtimeVersion}`);

const dataDir = await mkdtemp(join(tmpdir(), "yukinal-installed-agent-data-"));
try {
  await runSidecarSmoke(entry, {
    program: runtime,
    label: `${entry} (installed package resources)`,
    dataDir,
  });
} finally {
  await rm(dataDir, { recursive: true, force: true });
}
