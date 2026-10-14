#!/usr/bin/env node

import { existsSync, statSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const venvPython = join(
  root,
  "agent",
  ".venv",
  process.platform === "win32" ? "Scripts" : "bin",
  process.platform === "win32" ? "python.exe" : "python",
);
const python = process.env.YUKINAL_PYTHON ?? (existsSync(venvPython)
  ? venvPython
  : (process.platform === "win32" ? "python" : "python3"));
const entrypoint = join(root, "agent", "packaging", "entrypoint.py");
const output = join(root, "agent", "runtime");
const work = join(root, "target", "pyinstaller");

if (!existsSync(python)) {
  console.error(`Python environment was not found: ${python}`);
  console.error("Create it with: python -m venv agent/.venv");
  console.error('Then install it with: agent/.venv/Scripts/python.exe -m pip install -e "agent[agent,models,package]"');
  process.exit(2);
}

if (!existsSync(entrypoint)) {
  console.error(`Python Agent entrypoint was not found: ${entrypoint}`);
  process.exit(2);
}

const args = [
  "-m", "PyInstaller",
  "--clean", "--noconfirm", "--onedir",
  "--name", "yukinal-agent",
  "--distpath", output,
  "--workpath", work,
  "--specpath", join(work, "spec"),
  "--paths", join(root, "agent", "src"),
  "--collect-all", "jsonschema",
  "--collect-all", "langchain_core",
  "--collect-all", "langchain_openai",
  "--collect-all", "langchain_anthropic",
  "--collect-all", "langchain_google_genai",
  "--collect-all", "langgraph",
  "--collect-submodules", "mcp.client",
  "--hidden-import", "yukinal_ssh",
  "--hidden-import", "yukinal_agent.sidecar",
  entrypoint,
];

const result = spawnSync(python, args, { cwd: root, stdio: "inherit", windowsHide: true });
if (result.error) {
  console.error(`Could not launch PyInstaller: ${result.error.message}`);
  process.exit(1);
}
if (result.status !== 0) process.exit(result.status ?? 1);

const executable = join(output, "yukinal-agent", process.platform === "win32" ? "yukinal-agent.exe" : "yukinal-agent");
if (!existsSync(executable) || !statSync(executable).isFile()) {
  console.error(`PyInstaller did not produce the Agent executable: ${executable}`);
  process.exit(1);
}

const licenses = spawnSync(python, [
  join(root, "scripts", "collect-python-licenses.py"),
  "--destination",
  join(output, "THIRD_PARTY_LICENSES.txt"),
], { cwd: root, stdio: "inherit", windowsHide: true });
if (licenses.error || licenses.status !== 0) {
  console.error(`Could not collect frozen Agent license notices: ${licenses.error?.message ?? "see diagnostics above"}`);
  process.exit(licenses.status && licenses.status !== 0 ? licenses.status : 1);
}

const smoke = spawnSync(process.execPath, [join(root, "scripts", "smoke-python-agent.mjs"), "--executable", executable], {
  cwd: root,
  stdio: "inherit",
  windowsHide: true,
});
if (smoke.error || smoke.status !== 0) {
  console.error(`Packaged Agent smoke failed: ${smoke.error?.message ?? "see diagnostics above"}`);
  process.exit(smoke.status && smoke.status !== 0 ? smoke.status : 1);
}

console.log(`Packaged Python Agent: ${executable}`);
