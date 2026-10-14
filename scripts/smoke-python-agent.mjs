#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const option = process.argv.indexOf("--executable");
const executable = option >= 0
  ? resolve(process.argv[option + 1] ?? "")
  : join(root, "agent", "runtime", "yukinal-agent", process.platform === "win32" ? "yukinal-agent.exe" : "yukinal-agent");

if (!existsSync(executable)) {
  console.error(`Packaged Python Agent was not found: ${executable}`);
  process.exit(2);
}

function invoke(args, input) {
  const result = spawnSync(executable, args, {
    cwd: root,
    input,
    encoding: "utf8",
    timeout: 30_000,
    windowsHide: true,
    maxBuffer: 1024 * 1024,
  });
  if (result.error || result.status !== 0) {
    throw new Error(result.error?.message ?? result.stderr ?? `Agent exited with code ${result.status}`);
  }
  return result.stdout;
}

const version = invoke(["--version"]).trim();
if (version !== "0.1.0") throw new Error(`unexpected Agent version: ${version}`);

const imports = invoke(["--check-imports"]).trim();
if (imports !== "packaged imports ok") throw new Error(`provider/MCP imports failed: ${imports}`);

const input = [
  JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "1.0", clientVersion: "smoke", dataDir: "" } }),
  JSON.stringify({ jsonrpc: "2.0", id: 2, method: "system.ping", params: { echo: "frozen-runtime" } }),
  "",
].join("\n");
const frames = invoke([], input)
  .split(/\r?\n/)
  .filter(Boolean)
  .map((line) => JSON.parse(line));
const initialized = frames.find((frame) => frame.id === 1 && frame.result);
const pinged = frames.find((frame) => frame.id === 2 && frame.result);
if (initialized?.result?.protocolVersion !== "1.0") {
  throw new Error(`frozen Agent failed the JSON-RPC handshake: ${JSON.stringify(frames)}`);
}
if (pinged?.result?.pong !== "frozen-runtime") {
  throw new Error(`frozen Agent failed the JSON-RPC ping: ${JSON.stringify(frames)}`);
}

console.log(`PASS packaged Python Agent ${version}, provider/MCP imports, and JSON-RPC stdio`);
