#!/usr/bin/env node

import { chmodSync, copyFileSync, existsSync, mkdirSync, statSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnCommandSync } from "./lib/commands.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
const buildDirectory = resolve(
  root,
  process.env.YUKINAL_SSH_CMAKE_BUILD_DIR || join("native", "build-desktop"),
);
const resourceDirectory = join(root, "apps", "desktop", "src-tauri", "resources", "ssh-service");
const executableName = `yukinal-ssh-service${process.platform === "win32" ? ".exe" : ""}`;
const sourceCandidates = process.platform === "win32"
  ? [join(buildDirectory, "Release", executableName), join(buildDirectory, executableName)]
  : [join(buildDirectory, executableName), join(buildDirectory, "Release", executableName)];

function run(command, args) {
  const result = spawnCommandSync(command, args, { cwd: root, stdio: "inherit" });
  if (result.error) {
    console.error(`Could not run ${command}: ${result.error.message}`);
    process.exit(1);
  }
  if (result.status !== 0) process.exit(result.status ?? 1);
}

if (!existsSync(join(root, "native", "CMakeLists.txt"))) {
  console.error("native/CMakeLists.txt was not found; run this script from the repository root");
  process.exit(2);
}

console.log(`Configuring C++ SSH service in ${buildDirectory}`);
run("cmake", [
  "-S", join(root, "native"),
  "-B", buildDirectory,
  "-DYUKINAL_BUILD_TESTS=OFF",
  "-DYUKINAL_FETCH_DEPS=ON",
  "-DCMAKE_BUILD_TYPE=Release",
]);
run("cmake", [
  "--build", buildDirectory,
  "--config", "Release",
  "--target", "yukinal-ssh-service",
  "--parallel",
]);

const built = sourceCandidates.find((path) => existsSync(path) && statSync(path).isFile());
if (!built) {
  console.error(`CMake completed without producing ${executableName} in ${buildDirectory}`);
  process.exit(1);
}

mkdirSync(resourceDirectory, { recursive: true });
const staged = join(resourceDirectory, executableName);
copyFileSync(built, staged);
if (process.platform !== "win32") chmodSync(staged, 0o755);
console.log(`Staged C++ SSH service at ${staged}`);
