import test from "node:test";
import assert from "node:assert/strict";
import { join } from "node:path";

import { resolveTauriAgentCommand } from "./tauri-agent-command.mjs";

const root = "D:/yukinal";

test("Tauri development launches the Python Agent by default", () => {
  const resolved = resolveTauriAgentCommand(["dev"], {
    root,
    platform: "win32",
    exists: () => true,
    env: { PATH: "test-path" },
  });

  assert.equal(resolved.runtime, "python");
  assert.deepEqual(resolved.args, ["--filter", "@yukinal/desktop", "exec", "tauri", "dev"]);
  assert.equal(resolved.env.YUKINAL_AGENT_COMMAND, join(root, "agent", ".venv", "Scripts", "python.exe"));
  assert.equal(resolved.env.YUKINAL_AGENT_ARGS, "-m;yukinal_agent.sidecar");
});

test("the Python development shortcut defaults to Tauri dev", () => {
  const resolved = resolveTauriAgentCommand([], {
    root,
    platform: "linux",
    exists: () => true,
    env: {},
  });

  assert.equal(resolved.runtime, "python");
  assert.deepEqual(resolved.args, ["--filter", "@yukinal/desktop", "exec", "tauri", "dev"]);
  assert.equal(resolved.env.YUKINAL_AGENT_COMMAND, join(root, "agent", ".venv", "bin", "python"));
});

test("non-development Tauri commands do not require or select Python", () => {
  const resolved = resolveTauriAgentCommand(["build"], {
    root,
    env: { YUKINAL_AGENT_COMMAND: "caller-owned-command" },
    exists: () => false,
  });

  assert.equal(resolved.runtime, "unchanged");
  assert.deepEqual(resolved.args, ["--filter", "@yukinal/desktop", "exec", "tauri", "build"]);
  assert.equal(resolved.env.YUKINAL_AGENT_COMMAND, "caller-owned-command");
});

test("a missing Python development environment has an actionable error", () => {
  assert.throws(
    () => resolveTauriAgentCommand(["dev"], { root, env: {}, exists: () => false }),
    /Python Agent environment was not found/,
  );
});

test("the missing-runtime message uses the current platform's Python path", () => {
  assert.throws(
    () => resolveTauriAgentCommand(["dev"], {
      root,
      platform: "linux",
      env: {},
      exists: () => false,
    }),
    /agent\/\.venv\/bin\/python -m pip install/,
  );
});
