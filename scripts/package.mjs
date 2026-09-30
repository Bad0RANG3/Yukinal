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
 * `bundle.signingIdentity` is deliberately not configured (see `docs/release.md`).
 * That does not make a local build impossible -- the bundler simply
 * produces unsigned artifacts, and macOS/Windows warn on first launch.
 *
 * Extra arguments are forwarded to `tauri build`, e.g.
 *   pnpm run package -- --no-bundle     compile only, produce no installer
 */

import { existsSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnCommandSync } from "./lib/commands.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));

if (!existsSync(join(root, "pnpm-workspace.yaml"))) {
  console.error("run this from the repository root");
  process.exit(2);
}

const steps = [
  { name: "verify Node.js runtime checksum pins", command: process.execPath, args: ["scripts/check-node-runtime-pins.mjs"] },
  { name: "prepare bundled Node.js runtime", command: process.execPath, args: ["scripts/prepare-node-runtime.mjs"] },
  { name: "contract libs", command: "pnpm", args: ["build:libs"] },
  // Rebuilt here even though `beforeBuildCommand` builds it too: this is what step 3 checks,
  // and both must work for someone who runs `tauri build` directly.
  { name: "agent bundle (esbuild)", command: "pnpm", args: ["--filter", "@yukinal/agent", "build"] },
  {
    name: "installed-layout smoke with bundled Node.js",
    command: process.execPath,
    args: ["scripts/smoke-packaged-agent.mjs"],
    env: { YUKINAL_REQUIRE_PACKAGED_NODE: "1" },
  },
  { name: "packaging contract", command: process.execPath, args: ["scripts/check-packaging.mjs"] },
  {
    name: "tauri build (bundle)",
    command: "pnpm",
    args: ["--filter", "@yukinal/desktop", "exec", "tauri", "build", ...process.argv.slice(2)],
  },
];

for (const step of steps) {
  console.log(`\n── ${step.name}`);
  const result = spawnCommandSync(step.command, step.args, {
    cwd: root,
    stdio: "inherit",
    env: step.env ? { ...process.env, ...step.env } : process.env,
  });
  if (result.status !== 0) {
    console.error(`\n✗ ${step.name} failed (exit ${result.status})`);
    process.exit(result.status ?? 1);
  }
}

// Keep the checksum/provenance files in lockstep with the just-built installer. A compile-
// only invocation may leave older bundles in target/release, so it must not refresh them.
if (!process.argv.slice(2).includes("--no-bundle")) {
  if (process.platform === "win32") {
    console.log("\n── generated NSIS uninstall policy");
    const nsisCheck = spawnCommandSync(process.execPath, ["scripts/check-nsis-uninstall.mjs"], {
      cwd: root,
      stdio: "inherit",
    });
    if (nsisCheck.status !== 0) {
      console.error(`\n✗ generated NSIS uninstall policy failed (exit ${nsisCheck.status})`);
      process.exit(nsisCheck.status ?? 1);
    }
  }

  console.log("\n── release artifact manifest");
  const result = spawnCommandSync(process.execPath, ["scripts/write-release-manifest.mjs"], {
    cwd: root,
    stdio: "inherit",
  });
  if (result.status !== 0) {
    console.error(`\n✗ release artifact manifest failed (exit ${result.status})`);
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
      if (entry.isFile() && ["SHA256SUMS.txt", "release-manifest.json"].includes(entry.name)) continue;
      const path = join(dir, entry.name);
      if (entry.isDirectory()) walk(path);
      else console.log(`  ${path.replace(root, "").replace(/\\/g, "/")}  (${(statSync(path).size / 1_048_576).toFixed(1)} MiB)`);
    }
  };
  walk(bundleDir);
}

console.log("\nThese installers are UNSIGNED: no certificate or signing identity is configured.");
console.log("The packaged app includes the pinned Node.js runtime; the project NOTICE and bundled LICENSE identify its terms.");
console.log("See the repository docs/release.md, 「交付与发布」.");
