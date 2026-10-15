#!/usr/bin/env node
/**
 * The single local compile gate (every stage compiles, tests and is
 * checked for cross-platform problems before the next stage starts).
 *
 * Order is deliberate:
 *   1. check repository hygiene and docs
 *   2. run the configured C++ SSH and Python Agent tests before unrelated JS gates
 *   3. build the contract libs (consumers import their dist/*.d.ts)
 *   4. typecheck and test JavaScript sources (including retained legacy Agent tests)
 *   5. smoke the Python Agent
 *
 * Exits non-zero on the first failure.
 */

import { existsSync } from "node:fs";
import { join, resolve } from "node:path";
import { spawnCommandSync } from "./lib/commands.mjs";

const nativeBuildCandidates = process.env.YUKINAL_SSH_CMAKE_BUILD_DIR
  ? [resolve(process.env.YUKINAL_SSH_CMAKE_BUILD_DIR)]
  : ["native/build-check", "native/build-desktop", "native/build", "native/build-nmake"];
const nativeBuildDirectory = nativeBuildCandidates.find((directory) =>
  existsSync(join(directory, "CTestTestfile.cmake")));

const steps = [
  { name: "publication hygiene", command: process.execPath, args: ["scripts/check-publication.mjs"], required: true },
  { name: "secret scan", command: process.execPath, args: ["scripts/check-secrets.mjs"], required: true },
  { name: "cross-platform command runner tests", command: process.execPath, args: ["--test", "scripts/lib/commands.test.mjs"], required: true },
  // Documentation links are load-bearing now that the rules live in a tree: a
  // section that moves takes its anchors with it, and only the renderer notices.
  { name: "docs links", command: process.execPath, args: ["scripts/check-docs-links.mjs"], required: true },
  {
    name: "C++ SSH service and Python Agent tests",
    command: "ctest",
    args: ["--test-dir", nativeBuildDirectory ?? "native/build", "-C", "Release", "--output-on-failure"],
    required: true,
    skip: !nativeBuildDirectory,
    skipReason: "no configured CMake test build (see README.md)",
  },
  {
    name: "build contract libs",
    command: "pnpm",
    args: ["--filter", "@yukinal/shared", "--filter", "@yukinal/provider-sdk", "--filter", "@yukinal/agent-sdk", "build"],
    required: true,
  },
  { name: "typecheck", command: "pnpm", args: ["-r", "--if-present", "typecheck"], required: true },
  { name: "unit tests", command: "pnpm", args: ["-r", "--if-present", "test"], required: true },
  { name: "desktop bundle (vite build)", command: "pnpm", args: ["--filter", "@yukinal/desktop", "build"], required: true },
  { name: "Python Agent source sidecar smoke", command: process.execPath, args: ["scripts/smoke-python-dev-agent.mjs"], required: true },
];

if (!existsSync("pnpm-workspace.yaml")) {
  console.error("run this from the repository root");
  process.exit(2);
}

let failures = 0;
for (const step of steps) {
  if (step.skip) {
    console.log(`\n── ${step.name}: skipped (${step.skipReason ?? "prerequisite unavailable"})`);
    continue;
  }
  console.log(`\n── ${step.name}`);
  const result = spawnCommandSync(step.command, step.args, {
    stdio: "inherit",
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
