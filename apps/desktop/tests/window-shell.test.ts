import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import type { WindowShellContext, WindowShellAction, WindowShellSidebar, Server } from "@yukinal/shared";
import { isNativeDesktopShell, hasNativeWindowSidebar, updateNativeWindowContext, updateNativeWindowSidebar, onNativeWindowAction } from "../src/lib/ipc.js";
import { dispatchWindowShellAction } from "../src/lib/window-shell-actions.js";
import { installHost, removeHost } from "./host-mock.js";
import { ServerSchema } from "@yukinal/shared";

Object.defineProperty(globalThis, "window", { value: {}, configurable: true });
afterEach(removeHost);
const context: WindowShellContext = { section: "servers", serverName: null, serverStatus: null, agentBusy: false };

test("window context uses the optional presentation capability without a business command", async () => {
  const received: WindowShellContext[] = [];
  installHost({
    invoke: async () => { assert.fail("presentation must not invoke business commands"); },
    windowShell: { updateContext: async (value) => { received.push(value); } },
  });
  await updateNativeWindowContext(context);
  assert.equal(hasNativeWindowSidebar(), false);
  assert.deepEqual(received, [context]);
});

test("browser and older native hosts need no presentation capability", async () => {
  assert.equal(isNativeDesktopShell(), false);
  await updateNativeWindowContext(context);
  installHost();
  assert.equal(isNativeDesktopShell(), true);
  await updateNativeWindowContext(context);
});

test("malformed context is rejected before it reaches the native capability", async () => {
  let calls = 0;
  installHost({ windowShell: { updateContext: async () => { calls += 1; } } });
  await assert.rejects(updateNativeWindowContext({ ...context, approved: true } as WindowShellContext));
  assert.equal(calls, 0);
});

test("a native context failure is observable without falling back to a business command", async () => {
  installHost({
    invoke: async () => { assert.fail("there is no fallback business command"); },
    windowShell: { updateContext: async () => { throw new Error("shell unavailable"); } },
  });
  await assert.rejects(updateNativeWindowContext(context), /shell unavailable/);
});

const sidebar: WindowShellSidebar = { selectedServerId: null, servers: [], loading: false, failed: false, blocked: false };

test("native sidebar validates snapshots and action delivery and releases its subscription", async () => {
  let deliver: (value: unknown) => void = () => {};
  let released = false;
  const received: WindowShellAction[] = [];
  const snapshots: WindowShellSidebar[] = [];
  installHost({
    invoke: async () => { assert.fail("sidebar must use existing consumers, not arbitrary business commands"); },
    windowShell: {
      updateContext: async () => {}, updateSidebar: async (value) => { snapshots.push(value); },
      onAction: (handler) => { deliver = handler; return () => { released = true; }; },
    },
  });
  assert.equal(hasNativeWindowSidebar(), true);
  await updateNativeWindowSidebar(sidebar);
  await assert.rejects(updateNativeWindowSidebar({ ...sidebar, approved: true } as WindowShellSidebar));
  assert.deepEqual(snapshots, [sidebar]);
  const unsubscribe = onNativeWindowAction((value) => received.push(value));
  deliver({ type: "navigate", section: "projects" });
  deliver({ type: "connect-server", serverId: "bad-id" });
  deliver({ type: "navigate", section: "projects", approved: true });
  assert.deepEqual(received, [{ type: "navigate", section: "projects" }]);
  unsubscribe();
  assert.equal(released, true);
});

test("browser and partial native capabilities retain their React sidebar", async () => {
  assert.equal(hasNativeWindowSidebar(), false);
  await updateNativeWindowSidebar(sidebar);
  onNativeWindowAction(() => assert.fail("no native subscription"))();
  installHost({ windowShell: { updateContext: async () => {}, updateSidebar: async () => assert.fail("incomplete capability") } });
  assert.equal(hasNativeWindowSidebar(), false);
  await updateNativeWindowSidebar(sidebar);
});

test("sidebar actions revalidate current IDs, blocked state, lifecycle and delete confirmation", () => {
  const server: Server = ServerSchema.parse({ id: "srv_preview1", name: "Test", status: "disconnected",
    connection: { host: "test.invalid", port: 22, username: "test" }, capabilities: {}, metadata: { environment: "development" }, createdAt: "2026-10-16", updatedAt: "2026-10-16" });
  const effects: unknown[] = [];
  const services = {
    servers: [server], blocked: false, navigate: (section: string) => effects.push(section),
    select: (id: string) => effects.push(id), add: () => effects.push("add"), refresh: () => effects.push("refresh"),
    edit: (value: Server) => effects.push(value), confirmDelete: () => false,
    mutate: (value: unknown) => { effects.push(value); },
  };
  dispatchWindowShellAction({ type: "connect-server", serverId: "srv_preview2" }, services);
  dispatchWindowShellAction({ type: "connect-server", serverId: server.id }, { ...services, blocked: true });
  dispatchWindowShellAction({ type: "disconnect-server", serverId: server.id }, services);
  dispatchWindowShellAction({ type: "delete-server", serverId: server.id }, services);
  assert.deepEqual(effects, []);
  dispatchWindowShellAction({ type: "connect-server", serverId: server.id }, services);
  server.status = "connecting";
  dispatchWindowShellAction({ type: "connect-server", serverId: server.id }, services);
  server.status = "connected";
  dispatchWindowShellAction({ type: "connect-server", serverId: server.id }, services);
  dispatchWindowShellAction({ type: "disconnect-server", serverId: server.id }, services);
  dispatchWindowShellAction({ type: "delete-server", serverId: server.id }, { ...services, confirmDelete: () => true });
  dispatchWindowShellAction({ type: "navigate", section: "settings" }, services);
  assert.deepEqual(effects, [
    { type: "connect", serverId: server.id }, { type: "disconnect", serverId: server.id }, { type: "delete", serverId: server.id }, "settings",
  ]);
});
