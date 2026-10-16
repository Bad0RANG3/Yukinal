import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import type { WindowShellContext } from "@yukinal/shared";
import { isNativeDesktopShell, updateNativeWindowContext } from "../src/lib/ipc.js";
import { installHost, removeHost } from "./host-mock.js";

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
