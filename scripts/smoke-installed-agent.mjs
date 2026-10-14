#!/usr/bin/env node
/** Exercise the frozen Python Agent using only a package's installed resource directory. */

import { execFileSync } from "node:child_process";
import { mkdtemp, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

import { runSidecarSmoke } from "./lib/sidecar-smoke.mjs";

const [resourceRootArg, ...extraArgs] = process.argv.slice(2);
if (!resourceRootArg || extraArgs.length > 0) {
  console.error("usage: node scripts/smoke-installed-agent.mjs <installed-resource-directory>");
  process.exit(2);
}

const resourceRoot = resolve(resourceRootArg);
const entry = join(
  resourceRoot,
  "python-agent",
  "yukinal-agent",
  process.platform === "win32" ? "yukinal-agent.exe" : "yukinal-agent",
);
const requiredResources = [
  entry,
  join(resourceRoot, "python-agent", "THIRD_PARTY_LICENSES.txt"),
  join(resourceRoot, "NOTICE"),
];

for (const requiredPath of requiredResources) {
  const info = await stat(requiredPath).catch(() => null);
  if (!info?.isFile()) throw new Error(`installed package is missing a required file: ${requiredPath}`);
}

const version = execFileSync(entry, ["--version"], { encoding: "utf8" }).trim();
if (version !== "0.1.0") {
  throw new Error(`installed Python Agent is ${version}; expected 0.1.0`);
}
const imports = execFileSync(entry, ["--check-imports"], { encoding: "utf8" }).trim();
if (imports !== "packaged imports ok") throw new Error(`installed Python Agent imports failed: ${imports}`);
console.log(`✓ installed Python Agent ${version} and provider/MCP imports`);

const dataDir = await mkdtemp(join(tmpdir(), "yukinal-installed-agent-data-"));
const restrictedPath = process.platform === "win32"
  ? `${process.env.SystemRoot ?? "C:\\Windows"}\\System32;${process.env.SystemRoot ?? "C:\\Windows"}`
  : "/usr/bin:/bin";
try {
  await runSidecarSmoke(entry, {
    agentKind: "python",
    args: [],
    env: { PATH: restrictedPath },
    program: entry,
    label: `${entry} (installed Python Agent resources)`,
    dataDir,
  });
} finally {
  await rm(dataDir, { recursive: true, force: true });
}
