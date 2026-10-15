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
 *   6. rust fmt + clippy + check, when a Rust toolchain is installed
 *
 * Exits non-zero on the first failure.
 */

import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { join, resolve } from "node:path";
import { spawnCommandSync } from "./lib/commands.mjs";
import { fileURLToPath } from "node:url";

const hasCargo = spawnSync("cargo", ["--version"], { stdio: "ignore" }).status === 0;
const venvPython = join(
  "agent",
  ".venv",
  process.platform === "win32" ? "Scripts" : "bin",
  process.platform === "win32" ? "python.exe" : "python",
);
const testPython = process.env.YUKINAL_PYTHON
  ?? (existsSync(venvPython) ? resolve(venvPython) : (process.platform === "win32" ? "python" : "python3"));
const nativeBuildCandidates = process.env.YUKINAL_SSH_CMAKE_BUILD_DIR
  ? [resolve(process.env.YUKINAL_SSH_CMAKE_BUILD_DIR)]
  : ["native/build-check", "native/build-desktop", "native/build", "native/build-vs", "native/build-nmake"];
const nativeBuildDirectory = nativeBuildCandidates.find((directory) =>
  existsSync(join(directory, "CTestTestfile.cmake")));

const steps = [
  { name: "publication hygiene", command: process.execPath, args: ["scripts/check-publication.mjs"], required: true },
  { name: "secret scan", command: process.execPath, args: ["scripts/check-secrets.mjs"], required: true },
  { name: "cross-platform command runner tests", command: process.execPath, args: ["--test", "scripts/lib/commands.test.mjs"], required: true },
  { name: "NSIS uninstall data-policy tests", command: process.execPath, args: ["--test", "scripts/nsis-uninstall-policy.test.mjs"], required: true },
  { name: "release artifact manifest tests", command: process.execPath, args: ["--test", "scripts/release-manifest.test.mjs"], required: true },
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
  { name: "packaging contract", command: process.execPath, args: ["scripts/check-packaging.mjs"], required: true },
  { name: "unit tests", command: "pnpm", args: ["-r", "--if-present", "test"], required: true },
  { name: "desktop bundle (vite build)", command: "pnpm", args: ["--filter", "@yukinal/desktop", "build"], required: true },
  { name: "Python Agent source sidecar smoke", command: process.execPath, args: ["scripts/smoke-python-dev-agent.mjs"], required: true },
  { name: "rustfmt check", command: "cargo", args: ["fmt", "--all", "--check"], required: false, skip: !hasCargo, skipReason: "no cargo on PATH" },
  {
    name: "clippy",
    command: "cargo",
    args: ["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
    required: false,
    skip: !hasCargo,
    skipReason: "no cargo on PATH",
  },
  { name: "cargo check", command: "cargo", args: ["check", "--workspace", "--all-targets"], required: false, skip: !hasCargo, skipReason: "no cargo on PATH" },
  {
    name: "rust -> sidecar integration",
    command: "cargo",
    args: ["test", "--workspace", "--", "--test-threads=1"],
    required: false,
    skip: !hasCargo,
    skipReason: "no cargo on PATH",
    // Opt the cross-language test in: the real supervisor launches the Python Agent.
    env: {
      YUKINAL_TEST_PYTHON: testPython,
      YUKINAL_TEST_AGENT_SOURCE: fileURLToPath(new URL("../agent/src", import.meta.url)),
      // The supervisor also has protocol-only process fixtures written in JavaScript.
      YUKINAL_TEST_NODE: process.execPath,
      // CI must go red instead of quietly skipping the real Python-sidecar integration.
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
