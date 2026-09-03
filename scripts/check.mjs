#!/usr/bin/env node
/**
 * The single local compile gate (every stage compiles, tests and is
 * checked for cross-platform problems before the next stage starts).
 *
 * Order is deliberate:
 *   1. build the contract libs   (consumers import their dist/*.d.ts)
 *   2. typecheck every workspace
 *   3. run every workspace's unit tests
 *   4. rust fmt + clippy + check, when a Rust toolchain is installed
 *
 * Exits non-zero on the first failure.
 */

import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

const hasCargo = spawnSync("cargo", ["--version"], { stdio: "ignore" }).status === 0;

const steps = [
  { name: "publication hygiene", command: process.execPath, args: ["scripts/check-publication.mjs"], required: true },
  {
    name: "build contract libs",
    command: "pnpm",
    args: ["--filter", "@yukinal/shared", "--filter", "@yukinal/provider-sdk", "--filter", "@yukinal/agent-sdk", "build"],
    required: true,
  },
  { name: "typecheck", command: "pnpm", args: ["-r", "--if-present", "typecheck"], required: true },
  { name: "agent bundle (tsc)", command: "pnpm", args: ["--filter", "@yukinal/agent", "build"], required: true },
  { name: "unit tests", command: "pnpm", args: ["-r", "--if-present", "test"], required: true },
  { name: "desktop bundle (vite build)", command: "pnpm", args: ["--filter", "@yukinal/desktop", "build"], required: true },
  { name: "sidecar stdio smoke", command: process.execPath, args: ["scripts/smoke-sidecar.mjs"], required: true },
  { name: "rustfmt check", command: "cargo", args: ["fmt", "--all", "--check"], required: false, skip: !hasCargo },
  {
    name: "clippy",
    command: "cargo",
    args: ["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
    required: false,
    skip: !hasCargo,
  },
  { name: "cargo check", command: "cargo", args: ["check", "--workspace", "--all-targets"], required: false, skip: !hasCargo },
  {
    name: "rust -> sidecar integration",
    command: "cargo",
    args: ["test", "--workspace", "--", "--test-threads=1"],
    required: false,
    skip: !hasCargo,
    // Opt the cross-language test in: the real supervisor launches the real bundle.
    env: {
      YUKINAL_TEST_NODE: process.execPath,
      YUKINAL_TEST_ENTRY: fileURLToPath(new URL("../apps/agent/dist/index.js", import.meta.url)),
      // If the bundle path stops resolving, CI must go red instead of quietly skipping.
      YUKINAL_TEST_REQUIRED: "1",
    },
  },
];

if (!existsSync("pnpm-workspace.yaml")) {
  console.error("run this from the repository root");
  process.exit(2);
}

let failures = 0;
for (const step of steps) {
  if (step.skip) {
    console.log(`\n── ${step.name}: skipped (no cargo on PATH)`);
    continue;
  }
  console.log(`\n── ${step.name}`);
  // pnpm resolves through a .cmd shim on Windows, so it needs a shell; node/cargo are
  // real executables and a shell would mangle paths that contain spaces.
  const result = spawnSync(step.command, step.args, {
    stdio: "inherit",
    shell: step.command === "pnpm" && process.platform === "win32",
    env: step.env ? { ...process.env, ...step.env } : process.env,
  });
  if (result.status !== 0) {
    failures += 1;
    console.error(`✗ ${step.name} failed (exit ${result.status})`);
    if (step.required) break;
  } else {
    console.log(`✓ ${step.name}`);
  }
}

console.log(failures === 0 ? "\ncheck: green" : `\ncheck: ${failures} step(s) failed`);
process.exit(failures === 0 ? 0 : 1);
