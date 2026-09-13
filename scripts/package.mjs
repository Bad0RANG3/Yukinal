#!/usr/bin/env node
/**
 * Produce the installer for whatever platform this runs on (`pnpm run package`).
 *
 * What it adds over calling the bundler by hand: it fails *before* the expensive part.
 * `tauri build` compiles the whole Rust workspace in release mode before the bundler looks
 * at a single resource, so a config that stages nothing, or an agent bundle that still
 * imports `zod` from `node_modules`, would surface ten minutes in -- or, worse, only for
 * the user who installs the result. Steps 1-3 cost a few seconds and close that gap.
 *
 * The installer itself is unsigned: there is no certificate in this repository, and
 * `bundle.signingIdentity` is deliberately not configured (the repository `docs/packaging.md`,
 * 「打包与分发」). That does not make a local build impossible -- the bundler simply
 * produces unsigned artifacts, and macOS/Windows warn on first launch.
 *
 * Extra arguments are forwarded to `tauri build`, e.g.
 *   pnpm run package -- --no-bundle     compile only, produce no installer
 */

import { spawnSync } from "node:child_process";
import { existsSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));

if (!existsSync(join(root, "pnpm-workspace.yaml"))) {
  console.error("run this from the repository root");
  process.exit(2);
}

const steps = [
  { name: "contract libs", command: "pnpm", args: ["build:libs"] },
  // Rebuilt here even though `beforeBuildCommand` builds it too: this is what step 3 checks,
  // and both must work for someone who runs `tauri build` directly.
  { name: "agent bundle (esbuild)", command: "pnpm", args: ["--filter", "@yukinal/agent", "build"] },
  { name: "packaging contract", command: process.execPath, args: ["scripts/check-packaging.mjs"] },
  {
    name: "tauri build (bundle)",
    command: "pnpm",
    args: ["--filter", "@yukinal/desktop", "exec", "tauri", "build", ...process.argv.slice(2)],
  },
];

for (const step of steps) {
  console.log(`\n── ${step.name}`);
  const result = spawnSync(step.command, step.args, {
    cwd: root,
    stdio: "inherit",
    // pnpm resolves through a .cmd shim on Windows, so it needs a shell; node is a real
    // executable and a shell would mangle paths that contain spaces.
    shell: step.command === "pnpm" && process.platform === "win32",
  });
  if (result.status !== 0) {
    console.error(`\n✗ ${step.name} failed (exit ${result.status})`);
    process.exit(result.status ?? 1);
  }
}

// The cargo target directory is the workspace root's, so the bundles land there; the
// per-crate path is checked too because that is where they go if the crate is ever built
// outside the workspace.
const candidates = [
  join(root, "target", "release", "bundle"),
  join(root, "apps", "desktop", "src-tauri", "target", "release", "bundle"),
];
const bundleDir = candidates.find((path) => existsSync(path));

console.log("\n── artifacts");
if (!bundleDir) {
  console.log("  the bundler reported no output directory; nothing was produced");
} else {
  const walk = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) walk(path);
      else console.log(`  ${path.replace(root, "").replace(/\\/g, "/")}  (${(statSync(path).size / 1_048_576).toFixed(1)} MiB)`);
    }
  };
  walk(bundleDir);
}

console.log("\nThese installers are UNSIGNED: no certificate or signing identity is configured.");
console.log("They contain no Node.js runtime — the app needs the user's own `node` on PATH.");
console.log("See the repository docs/packaging.md, 「打包与分发」.");
