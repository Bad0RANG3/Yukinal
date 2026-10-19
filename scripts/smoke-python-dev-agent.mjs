#!/usr/bin/env node

/** Exercise the source Python Agent through the same NDJSON sidecar contract the desktop host uses. */

import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdtemp, rm } from "node:fs/promises";
import { delimiter, join } from "node:path";
import { tmpdir } from "node:os";
import { DatabaseSync } from "node:sqlite";
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
// The desktop host migrates `yukinal.db` before the sidecar starts. Reproduce the one
// table the protocol smoke exercises so run.start resolves a loopback provider.
const seed = new DatabaseSync(join(dataDir, "yukinal.db"));
seed.exec(`
  CREATE TABLE provider_configs (
    id TEXT PRIMARY KEY,
    family TEXT NOT NULL,
    kind TEXT NOT NULL,
    label TEXT NOT NULL,
    base_url TEXT,
    model TEXT,
    api_key_credential_ref TEXT,
    credential_ref TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    custom_headers TEXT,
    max_input_tokens INTEGER,
    settings TEXT,
    wire_api TEXT NOT NULL DEFAULT 'chat',
    api_version TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
  );
  INSERT INTO provider_configs (
    id, family, kind, label, base_url, model, api_key_credential_ref, credential_ref,
    enabled, custom_headers, max_input_tokens, settings, wire_api, api_version, created_at, updated_at
  ) VALUES (
    'smoke-provider', 'ai', 'openai-compatible', 'smoke', 'http://127.0.0.1:1', 'm',
    NULL, NULL, 1, NULL, NULL, NULL, 'chat', NULL,
    '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z'
  );
`);
seed.close();
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
  // Windows can keep the checkpoint SQLite file locked for a moment after the
  // child exits, so removal is retried instead of failing the smoke on cleanup.
  for (let attempt = 0; attempt < 10; attempt += 1) {
    try {
      await rm(dataDir, { recursive: true, force: true });
      break;
    } catch (error) {
      if (error?.code !== "EBUSY" && error?.code !== "EPERM") throw error;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
  }
}
