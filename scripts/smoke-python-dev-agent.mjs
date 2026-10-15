#!/usr/bin/env node

/** Exercise the source Python Agent through the same NDJSON sidecar contract as Tauri. */

import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdtemp, rm } from "node:fs/promises";
import { delimiter, join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";

import { runSidecarSmoke } from "./lib/sidecar-smoke.mjs";

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
const env = {
  ...process.env,
  PYTHONPATH: [join(root, "agent", "src"), process.env.PYTHONPATH].filter(Boolean).join(delimiter),
  // Register SSH tool schemas for tools.list without connecting to a real service.
  YUKINAL_SSH_API_TOKEN: `smoke-${process.pid}-${Date.now()}`.padEnd(40, "x"),
  YUKINAL_SSH_API_URL: "http://127.0.0.1:9",
};

const version = spawnSync(python, ["--version"], {
  cwd: root,
  env,
  encoding: "utf8",
  windowsHide: true,
});
if (version.error || version.status !== 0) {
  console.error(`Python Agent smoke could not start ${python}: ${version.error?.message ?? version.stderr}`);
  process.exit(2);
}
const versionText = `${version.stdout}${version.stderr}`;
const match = versionText.match(/Python (\d+)\.(\d+)/);
if (!match || Number(match[1]) < 3 || (Number(match[1]) === 3 && Number(match[2]) < 11)) {
  console.error(`Python 3.11 or newer is required for the Agent smoke; found ${versionText.trim()}`);
  process.exit(2);
}

const imports = spawnSync(python, ["-c", [
  "import yukinal_agent.sidecar",
  "import jsonschema, langgraph, mcp",
  "import langgraph.checkpoint.sqlite.aio",
  "import langchain_openai, langchain_anthropic, langchain_google_genai",
].join("; ")], {
  cwd: root,
  env,
  encoding: "utf8",
  windowsHide: true,
});
if (imports.error || imports.status !== 0) {
  console.error("Python Agent dependencies are not installed; run: python -m pip install -e \"agent[agent,models]\"");
  if (imports.stderr) console.error(imports.stderr.trim());
  process.exit(2);
}

const dataDir = await mkdtemp(join(tmpdir(), "yukinal-python-agent-smoke-"));
try {
  await runSidecarSmoke(python, {
    agentKind: "python",
    args: ["-m", "yukinal_agent.sidecar"],
    env,
    label: "-m yukinal_agent.sidecar",
    program: python,
    dataDir,
  });
} finally {
  await rm(dataDir, { recursive: true, force: true });
}
