#!/usr/bin/env node

import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { resolveCommandInvocation } from "./lib/commands.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
const python = process.env.YUKINAL_PYTHON ?? join(
  root,
  "agent",
  ".venv",
  process.platform === "win32" ? "Scripts" : "bin",
  process.platform === "win32" ? "python.exe" : "python",
);

if (!existsSync(python)) {
  console.error(`Python Agent environment was not found: ${python}`);
  console.error("Create it with: python -m venv agent/.venv");
  console.error('Then install it with: agent/.venv/Scripts/python.exe -m pip install -e "agent[agent,models]"');
  process.exit(2);
}

const env = {
  ...process.env,
  YUKINAL_AGENT_COMMAND: python,
  YUKINAL_AGENT_ARGS: "-m;yukinal_agent.sidecar",
};
const invocation = resolveCommandInvocation(
  "pnpm",
  ["--filter", "@yukinal/desktop", "exec", "tauri", "dev"],
  { env },
);
const child = spawn(invocation.command, invocation.args, {
  cwd: root,
  env,
  stdio: "inherit",
});

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => child.kill(signal));
}

const result = await new Promise((resolve) => {
  child.once("error", (error) => resolve({ error }));
  child.once("exit", (code, signal) => resolve({ code, signal }));
});
if (result.error) {
  console.error(`Could not start the Python Agent desktop: ${result.error.message}`);
  process.exitCode = 1;
} else if (Number.isInteger(result.code)) {
  process.exitCode = result.code;
} else if (!process.exitCode) {
  process.exitCode = 1;
}
