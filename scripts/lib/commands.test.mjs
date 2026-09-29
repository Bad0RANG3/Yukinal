import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { resolveCommandInvocation } from "./commands.mjs";

test("Windows pnpm.cmd installations launch the package JS entrypoint without a shell", () => {
  const invocation = resolveCommandInvocation("pnpm", ["--filter", "@yukinal/agent", "build"], {
    platform: "win32",
    env: { Path: "C:\\tools\\npm;C:\\other" },
    nodeExecutable: "C:\\Program Files\\nodejs\\node.exe",
    exists: (candidate) => candidate === "C:\\tools\\npm\\node_modules\\pnpm\\bin\\pnpm.mjs",
  });

  assert.deepEqual(invocation, {
    command: "C:\\Program Files\\nodejs\\node.exe",
    args: [
      "C:\\tools\\npm\\node_modules\\pnpm\\bin\\pnpm.mjs",
      "--filter",
      "@yukinal/agent",
      "build",
    ],
  });
});

test("Windows pnpm setup in node_modules/.bin resolves the sibling package", () => {
  const entrypoint = path.win32.resolve(
    "D:\\runner\\setup\\node_modules\\.bin",
    "..",
    "pnpm",
    "bin",
    "pnpm.mjs",
  );
  const invocation = resolveCommandInvocation("pnpm", ["check"], {
    platform: "win32",
    env: { PATH: "D:\\runner\\setup\\node_modules\\.bin" },
    nodeExecutable: "D:\\node.exe",
    exists: (candidate) => candidate === entrypoint,
  });

  assert.deepEqual(invocation, { command: "D:\\node.exe", args: [entrypoint, "check"] });
});

test("Windows Corepack global installation resolves its pnpm JS shim", () => {
  const entrypoint = "C:\\tools\\node_modules\\corepack\\dist\\pnpm.js";
  const invocation = resolveCommandInvocation("pnpm", ["--version"], {
    platform: "win32",
    env: { PATH: "C:\\tools" },
    nodeExecutable: "C:\\Program Files\\nodejs\\node.exe",
    exists: (candidate) => candidate === entrypoint,
  });

  assert.deepEqual(invocation, {
    command: "C:\\Program Files\\nodejs\\node.exe",
    args: [entrypoint, "--version"],
  });
});

test("Windows Corepack setup in node_modules/.bin resolves the sibling shim", () => {
  const entrypoint = path.win32.resolve(
    "D:\\runner\\setup\\node_modules\\.bin",
    "..",
    "corepack",
    "dist",
    "pnpm.js",
  );
  const invocation = resolveCommandInvocation("pnpm", ["check"], {
    platform: "win32",
    env: { PATH: "D:\\runner\\setup\\node_modules\\.bin" },
    nodeExecutable: "D:\\node.exe",
    exists: (candidate) => candidate === entrypoint,
  });

  assert.deepEqual(invocation, { command: "D:\\node.exe", args: [entrypoint, "check"] });
});

test("native Windows pnpm.exe is invoked directly", () => {
  const invocation = resolveCommandInvocation("pnpm", ["--version"], {
    platform: "win32",
    env: { PATH: "D:\\pnpm" },
    exists: (candidate) => candidate === "D:\\pnpm\\pnpm.exe",
  });

  assert.deepEqual(invocation, { command: "D:\\pnpm\\pnpm.exe", args: ["--version"] });
});

test("non-Windows commands are passed through unchanged", () => {
  const args = ["--filter", "@yukinal/agent", "build"];
  const invocation = resolveCommandInvocation("pnpm", args, {
    platform: "linux",
    env: {},
  });

  assert.deepEqual(invocation, { command: "pnpm", args });
});

test("missing Windows pnpm entrypoint fails with setup guidance", () => {
  assert.throws(
    () => resolveCommandInvocation("pnpm", ["check"], {
      platform: "win32",
      env: { PATH: "C:\\tools" },
      exists: () => false,
    }),
    /Could not resolve pnpm's Windows executable/,
  );
});
