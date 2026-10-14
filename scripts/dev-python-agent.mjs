#!/usr/bin/env node

import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

import { resolveCommandInvocation } from "./lib/commands.mjs";
import { resolveTauriAgentCommand } from "./lib/tauri-agent-command.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
let launch;
try {
  launch = resolveTauriAgentCommand(process.argv.slice(2), { root });
} catch (error) {
  console.error(error instanceof Error ? error.message : "Could not configure the Python Agent runtime");
  process.exit(2);
}

const invocation = resolveCommandInvocation(
  "pnpm",
  launch.args,
  { env: launch.env },
);
const child = spawn(invocation.command, invocation.args, {
  cwd: root,
  env: launch.env,
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
