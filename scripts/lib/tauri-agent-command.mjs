import { existsSync } from "node:fs";
import { join } from "node:path";

export function resolveTauriAgentCommand(args, {
  env = process.env,
  root,
  platform = process.platform,
  exists = existsSync,
} = {}) {
  const tauriArgs = args.length > 0 ? [...args] : ["dev"];
  const development = tauriArgs[0] === "dev";
  const commandArgs = ["--filter", "@yukinal/desktop", "exec", "tauri", ...tauriArgs];
  const commandEnv = { ...env };

  if (!development) return { args: commandArgs, env: commandEnv, runtime: "unchanged" };

  const python = env.YUKINAL_PYTHON ?? join(
    root,
    "agent",
    ".venv",
    platform === "win32" ? "Scripts" : "bin",
    platform === "win32" ? "python.exe" : "python",
  );
  if (!exists(python)) {
    const pip = platform === "win32"
      ? "agent/.venv/Scripts/python.exe -m pip install -e \"agent[agent,models]\""
      : "agent/.venv/bin/python -m pip install -e 'agent[agent,models]'";
    throw new Error(
      `Python Agent environment was not found: ${python}\n`
      + "Create it with: python -m venv agent/.venv\n"
      + `Then install it with: ${pip}`,
    );
  }

  commandEnv.YUKINAL_AGENT_COMMAND = python;
  commandEnv.YUKINAL_AGENT_ARGS = "-m;yukinal_agent.sidecar";
  return { args: commandArgs, env: commandEnv, runtime: "python" };
}
