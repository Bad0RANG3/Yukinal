#!/usr/bin/env node

import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { setTimeout as delay } from "node:timers/promises";
import { delimiter, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { resolveCommandInvocation } from "./lib/commands.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const desktop = join(root, "apps", "desktop");
const native = join(root, "native");
const runningChildren = new Set();
let shutdownSignal;
const buildDirectory = resolve(
  root,
  process.env.YUKINAL_DESKTOP_HOST_BUILD_DIR ?? join("native", "build-cpp-host"),
);
// A preview-only host deliberately has no SSH transport. Real desktop development
// builds both the in-process core and the PTY worker unless explicitly opted out.
const previewOnlyValue = process.env.YUKINAL_DESKTOP_PREVIEW_ONLY ?? "0";
if (previewOnlyValue !== "0" && previewOnlyValue !== "1") {
  throw new Error("YUKINAL_DESKTOP_PREVIEW_ONLY must be 0 or 1.");
}
const previewOnly = previewOnlyValue === "1";
const opensslRoot = process.env.YUKINAL_OPENSSL_ROOT ?? process.env.OPENSSL_ROOT_DIR;
const port = Number(process.env.YUKINAL_DESKTOP_DEV_PORT ?? "1420");
if (!Number.isInteger(port) || port < 1 || port > 65535) {
  console.error("YUKINAL_DESKTOP_DEV_PORT must be an integer from 1 to 65535.");
  process.exit(2);
}

function run(command, args, options = {}) {
  return new Promise((resolveResult, reject) => {
    const child = spawn(command, args, {
      cwd: options.cwd ?? root,
      env: options.env ?? process.env,
      stdio: "inherit",
      windowsHide: true,
    });
    runningChildren.add(child);
    child.once("error", (error) => {
      runningChildren.delete(child);
      reject(error);
    });
    child.once("exit", (code, signal) => {
      runningChildren.delete(child);
      if (code === 0) resolveResult();
      else reject(new Error(`${command} exited with ${signal ?? code ?? "unknown status"}`));
    });
  });
}

function spawnLongRunning(command, args, options = {}) {
  const child = spawn(command, args, {
    cwd: options.cwd ?? root,
    env: options.env ?? process.env,
    stdio: "inherit",
    windowsHide: options.windowsHide ?? true,
  });
  runningChildren.add(child);
  const exit = new Promise((resolveResult) => {
    child.once("error", (error) => {
      runningChildren.delete(child);
      resolveResult({ error });
    });
    child.once("exit", (code, signal) => {
      runningChildren.delete(child);
      resolveResult({ code, signal });
    });
  });
  return { child, exit };
}

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => {
    shutdownSignal = signal;
    for (const child of runningChildren) child.kill(signal);
  });
}

async function waitForVite(child, url) {
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    if (child.exitCode !== null || child.signalCode !== null) {
      throw new Error("Vite exited before the development page became available.");
    }
    try {
      const response = await fetch(url, { signal: AbortSignal.timeout(1_000) });
      if (response.ok) return;
    } catch {
      // Vite has not bound its port yet.
    }
    await delay(250);
  }
  throw new Error(`Vite did not become ready at ${url} within 30 seconds.`);
}

async function stop(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  child.kill("SIGTERM");
  await Promise.race([
    new Promise((resolveResult) => child.once("exit", resolveResult)),
    delay(5_000),
  ]);
  if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
}

function hostExecutable() {
  if (process.env.YUKINAL_DESKTOP_HOST_BINARY) {
    const configured = resolve(root, process.env.YUKINAL_DESKTOP_HOST_BINARY);
    if (!existsSync(configured)) throw new Error(`C++ desktop host not found: ${configured}`);
    return configured;
  }
  const suffix = process.platform === "win32" ? ".exe" : "";
  const candidates = [
    join(buildDirectory, `yukinal-desktop-host${suffix}`),
    join(buildDirectory, "Debug", `yukinal-desktop-host${suffix}`),
    join(buildDirectory, "Release", `yukinal-desktop-host${suffix}`),
  ];
  const executable = candidates.find(existsSync);
  if (!executable) {
    throw new Error(`CMake built the host but its executable was not found under ${buildDirectory}.`);
  }
  return executable;
}

function sshServiceExecutable() {
  const suffix = process.platform === "win32" ? ".exe" : "";
  const candidates = [
    join(buildDirectory, `yukinal-ssh-service${suffix}`),
    join(buildDirectory, "Debug", `yukinal-ssh-service${suffix}`),
    join(buildDirectory, "Release", `yukinal-ssh-service${suffix}`),
  ];
  const executable = candidates.find(existsSync);
  if (!executable) {
    throw new Error(`CMake built the desktop but the SSH worker is missing under ${buildDirectory}.`);
  }
  return executable;
}

async function main() {
  const sharedBuild = resolveCommandInvocation("pnpm", ["--filter", "@yukinal/shared", "build"]);
  await run(sharedBuild.command, sharedBuild.args);

  const configureArgs = [
    "-S", native,
    "-B", buildDirectory,
    `-DYUKINAL_BUILD_SSH_SERVICE=${previewOnly ? "OFF" : "ON"}`,
    "-DYUKINAL_BUILD_DESKTOP_HOST=ON",
    "-DYUKINAL_BUILD_TESTS=OFF",
    "-DYUKINAL_FETCH_DEPS=ON",
    "-DYUKINAL_BUNDLE_WINDOWS_OPENSSL=OFF",
  ];
  if (!previewOnly && opensslRoot) {
    configureArgs.push(`-DOPENSSL_ROOT_DIR=${resolve(opensslRoot)}`);
  }
  await run("cmake", configureArgs);
  await run("cmake", [
    "--build", buildDirectory,
    "--target", "yukinal-desktop-host",
    ...(previewOnly ? [] : ["yukinal-ssh-service"]),
    "--config", "Debug",
  ]);
  const viteEntry = join(desktop, "node_modules", "vite", "bin", "vite.js");
  if (!existsSync(viteEntry)) {
    throw new Error("Vite is not installed. Run pnpm install --frozen-lockfile first.");
  }
  // Check before starting Vite so an incomplete native build cannot open a
  // seemingly functional desktop whose terminal worker is absent.
  const serviceBinary = sshServiceExecutable();
  const url = `http://127.0.0.1:${port}`;
  const vite = spawnLongRunning(process.execPath, [
    viteEntry, "--host", "127.0.0.1", "--port", String(port), "--strictPort",
  ], { cwd: desktop });
  let host;
  let result;
  try {
    await waitForVite(vite.child, url);
    const venvPython = join(
      root,
      "agent",
      ".venv",
      process.platform === "win32" ? "Scripts" : "bin",
      process.platform === "win32" ? "python.exe" : "python",
    );
    const hostEnvironment = { ...process.env };
    if (serviceBinary) {
      hostEnvironment.YUKINAL_SSH_SERVICE_BINARY = serviceBinary;
      hostEnvironment.YUKINAL_SSH_SERVICE_AUTOCONFIG = "1";
      const runtimeDirectories = [dirname(serviceBinary)];
      if (opensslRoot) runtimeDirectories.push(join(resolve(opensslRoot), "bin"));
      hostEnvironment.PATH = [...runtimeDirectories, hostEnvironment.PATH ?? ""].join(delimiter);
    } else {
      // An inherited autoconfig flag must not silently reactivate SSH in a
      // session whose owner explicitly requested a visual preview only.
      delete hostEnvironment.YUKINAL_SSH_SERVICE_AUTOCONFIG;
    }
    if (!hostEnvironment.YUKINAL_AGENT_PYTHON && existsSync(venvPython)) {
      hostEnvironment.YUKINAL_AGENT_PYTHON = venvPython;
    }
    if (!hostEnvironment.YUKINAL_AGENT_SOURCE) {
      hostEnvironment.YUKINAL_AGENT_SOURCE = join(root, "agent", "src");
    }
    // SW_HIDE in STARTUPINFO overrides the WebView's first ShowWindow call.
    host = spawnLongRunning(hostExecutable(), ["--dev-url", url], { env: hostEnvironment, windowsHide: false });
    console.log(previewOnly
      ? "Desktop preview-only mode is running; SSH is disabled by explicit request."
      : "Desktop development is running with the SSH core and terminal worker; saved UI changes update this window automatically.");
    result = await Promise.race([
      vite.exit.then((value) => ({ role: "Vite", value })),
      host.exit.then((value) => ({ role: "C++ host", value })),
    ]);
  } finally {
    await Promise.all([stop(vite.child), stop(host?.child)]);
  }

  if (result.value.error) throw result.value.error;
  if (shutdownSignal) {
    process.exitCode = shutdownSignal === "SIGINT" ? 130 : 143;
    return;
  }
  if (result.role === "Vite" && result.value.code === 0) return;
  if (Number.isInteger(result.value.code)) process.exitCode = result.value.code;
  else process.exitCode = 1;
}

main().catch((error) => {
  if (shutdownSignal) {
    process.exitCode = shutdownSignal === "SIGINT" ? 130 : 143;
    return;
  }
  console.error(error instanceof Error ? error.message : "Could not start the C++ desktop host");
  process.exitCode = 1;
});
