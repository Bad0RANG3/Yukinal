import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";

function pathValue(env) {
  const key = Object.keys(env).find((name) => name.toUpperCase() === "PATH");
  return key ? env[key] : "";
}

function windowsPnpmInvocation(args, { env, exists, nodeExecutable }) {
  const directories = pathValue(env)
    .split(path.win32.delimiter)
    .map((directory) => directory.trim().replace(/^"|"$/g, ""))
    .filter(Boolean);

  for (const directory of directories) {
    const nativeExecutable = path.win32.join(directory, "pnpm.exe");
    if (exists(nativeExecutable)) {
      return { command: nativeExecutable, args };
    }

    const javascriptEntrypoints = [
      path.win32.join(directory, "node_modules", "pnpm", "bin", "pnpm.mjs"),
      path.win32.join(directory, "node_modules", "pnpm", "bin", "pnpm.cjs"),
      path.win32.resolve(directory, "..", "pnpm", "bin", "pnpm.mjs"),
      path.win32.resolve(directory, "..", "pnpm", "bin", "pnpm.cjs"),
      path.win32.join(directory, "node_modules", "corepack", "dist", "pnpm.js"),
      path.win32.resolve(directory, "..", "corepack", "dist", "pnpm.js"),
    ];
    const entrypoint = javascriptEntrypoints.find(exists);
    if (entrypoint) {
      return { command: nodeExecutable, args: [entrypoint, ...args] };
    }
  }

  throw new Error(
    "Could not resolve pnpm's Windows executable. Ensure pnpm.exe or its pnpm/Corepack JS entrypoint is on PATH.",
  );
}

export function resolveCommandInvocation(command, args, {
  platform = process.platform,
  env = process.env,
  exists = existsSync,
  nodeExecutable = process.execPath,
} = {}) {
  if (platform === "win32" && command.toLowerCase() === "pnpm") {
    return windowsPnpmInvocation(args, { env, exists, nodeExecutable });
  }

  return { command, args };
}

export function spawnCommandSync(command, args, options = {}) {
  const invocation = resolveCommandInvocation(command, args, {
    env: options.env ?? process.env,
  });
  return spawnSync(invocation.command, invocation.args, options);
}
