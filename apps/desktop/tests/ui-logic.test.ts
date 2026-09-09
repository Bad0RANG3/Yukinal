import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { IPC_COMMANDS } from "@yukinal/shared";
import { callDesktop } from "../src/lib/ipc.js";
import { useWorkspaceStore } from "../src/stores/workspace-store.js";
import { buildServerInput, type ServerFormValues } from "../src/features/servers/server-form.js";
import { RunLifecycle } from "../src/features/agent/run-lifecycle.js";

Object.defineProperty(globalThis, "window", { value: {}, configurable: true });
afterEach(() => {
  clearMocks();
  useWorkspaceStore.setState(useWorkspaceStore.getInitialState(), true);
});

test("server selection opens its workspace without resetting the current server's tab", () => {
  const workspace = useWorkspaceStore.getState();
  workspace.selectServer("srv_first");
  workspace.setServerPage("files");
  workspace.selectServer("srv_first");
  assert.equal(useWorkspaceStore.getState().serverPage, "files");
  workspace.setPrimary("settings");
  workspace.selectServer("srv_second");
  assert.equal(useWorkspaceStore.getState().primary, "servers");
  assert.equal(useWorkspaceStore.getState().serverPage, "overview");
});

test("server form commands match the Rust input argument; other commands stay flat", async () => {
  const calls: unknown[] = [];
  const server = {
    id: "srv_test",
    name: "Test",
    connection: { host: "example.test", port: 22, username: "deploy" },
    capabilities: {},
    status: "disconnected",
    metadata: { environment: "staging" },
    createdAt: "2026-01-01T00:00:00Z",
    updatedAt: "2026-01-01T00:00:00Z",
  };
  mockIPC((command, args) => {
    calls.push({ command, args });
    if (command === "server_add" || command === "server_update") return { server };
    if (command === "server_connect") return { status: "connected" };
    return {};
  });
  const input = {
    name: "Test", host: "example.test", port: 22, username: "deploy",
    environment: "staging" as const,
    authentication: { method: "password" as const, password: "test-only" },
  };
  await callDesktop(IPC_COMMANDS.serverAdd, input);
  await callDesktop(IPC_COMMANDS.serverUpdate, { ...input, serverId: "srv_test" });
  await callDesktop(IPC_COMMANDS.serverConnect, { serverId: "srv_test" });
  assert.deepEqual(calls, [
    { command: "server_add", args: { input } },
    { command: "server_update", args: { input: { ...input, serverId: "srv_test" } } },
    { command: "server_connect", args: { serverId: "srv_test" } },
  ]);
});

test("native string errors become visible Error messages", async () => {
  mockIPC(() => Promise.reject("SSH authentication failed"));
  await assert.rejects(callDesktop(IPC_COMMANDS.serverConnect, { serverId: "srv_test" }), {
    name: "Error", message: "SSH authentication failed",
  });
});

const formValues: ServerFormValues = {
  name: " staging ", host: "example.test", port: "2222", username: "deploy", environment: "staging",
  authMethod: "password", password: "secret", privateKeyPem: "",
};

test("server form normalizes values and validates port and first-time credentials", () => {
  assert.deepEqual(buildServerInput(formValues), {
    name: "staging", host: "example.test", port: 2222, username: "deploy", environment: "staging",
    authentication: { method: "password", password: "secret" },
  });
  assert.deepEqual(buildServerInput({ ...formValues, password: "" }, "srv_existing"), {
    name: "staging", host: "example.test", port: 2222, username: "deploy", environment: "staging", serverId: "srv_existing",
  });
  assert.throws(() => buildServerInput({ ...formValues, port: "0" }), /端口/);
  assert.throws(() => buildServerInput({ ...formValues, password: "" }), /SSH 密码/);
});

test("run lifecycle accepts one start and ignores duplicate or stale terminal events", () => {
  const lifecycle = new RunLifecycle();
  assert.equal(lifecycle.begin("run_a"), true);
  assert.equal(lifecycle.begin(), false);
  assert.equal(lifecycle.started("run_other"), false);
  assert.equal(lifecycle.started("run_a"), true);
  assert.equal(lifecycle.started("run_b"), false);
  assert.equal(lifecycle.isActive("run_b"), false);
  assert.equal(lifecycle.finish("run_b"), false);
  assert.equal(lifecycle.finish("run_a"), true);
  assert.equal(lifecycle.finish("run_a"), false);
});
