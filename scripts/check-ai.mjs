#!/usr/bin/env node
/** Deterministic AI acceptance. Real-provider/native UI acceptance is a separate gate. */
import { existsSync } from "node:fs";
import { delimiter, join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnCommandSync } from "./lib/commands.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
const tests = join(root, "agent", "tests");
const localPython = join(root, "agent", ".venv", process.platform === "win32" ? "Scripts/python.exe" : "bin/python");
const python = process.env.YUKINAL_AGENT_PYTHON ?? process.env.YUKINAL_PYTHON
  ?? (existsSync(localPython) ? localPython : process.platform === "win32" ? "python" : "python3");
const modules = [
  "test_agent_streaming", "test_agent_model_loop", "test_agent_conversation_context", "test_coordinator_runtime",
  "test_agent_run_finalization", "test_provider_store", "test_agent_sidecar",
  "test_reasoning_effort", "test_agent_moe_routing", "test_agent_reasoning_loop",
  "test_agent_long_workflow", "test_agent_large_coordination",
];
for (const module of modules) {
  if (!existsSync(join(tests, `${module}.py`))) {
    console.error(`AI acceptance prerequisite missing: ${module}.py`);
    process.exit(2);
  }
}

const stages = [
  { name: "build current contracts", command: "pnpm", args: ["--filter", "@yukinal/shared", "build"] },
  { name: "desktop typecheck", command: "pnpm", args: ["--filter", "@yukinal/desktop", "typecheck"] },
  { name: "desktop UI, model routing and lifecycle regressions", command: "pnpm", args: ["--filter", "@yukinal/desktop", "test"] },
  { name: "AI context, tool loop, coordination and failure finalization", command: python,
    args: ["-m", "unittest", "-v", ...modules], cwd: tests,
    env: { ...process.env, PYTHONPATH: [join(root, "agent", "src"), process.env.PYTHONPATH].filter(Boolean).join(delimiter) } },
];

for (const stage of stages) {
  console.log(`\nAI acceptance: ${stage.name}`);
  const result = spawnCommandSync(stage.command, stage.args, {
    cwd: stage.cwd ?? root, env: stage.env ?? process.env, stdio: "inherit", windowsHide: true,
  });
  if (result.error || result.status !== 0) {
    console.error(`AI acceptance failed: ${stage.name} (${result.error?.message ?? result.status})`);
    process.exit(result.status || 1);
  }
}
console.log("\nAI deterministic acceptance passed. Native UI and real-provider evidence must still be reviewed; no target security/stress pass is implied.");
