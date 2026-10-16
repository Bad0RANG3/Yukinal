import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import type { WindowShellContext } from "@yukinal/shared";
import { isNativeDesktopShell, updateNativeWindowContext, supportsNativeCaptionSurface, supportsNativeTitlebarControls, updateNativeCaptionSurface } from "../src/lib/ipc.js";
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
  assert.equal(supportsNativeCaptionSurface(), false);
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


test("caption presentation is optional and validates before crossing the host boundary", async () => {
  const received: unknown[] = [];
  const surface = { viewportWidth: 1440, left: 280, right: 1160, inspectorLeft: 1160, radius: 14, background: "#282828", border: "#303030" };
  installHost({ windowShell: {
    updateContext: async () => {}, titlebarOverlay: true,
    updateSurface: async (value) => { received.push(value); },
  } });
  assert.equal(supportsNativeCaptionSurface(), true);
  assert.equal(supportsNativeTitlebarControls(), true);
  await updateNativeCaptionSurface(surface);
  await updateNativeCaptionSurface(null);
  await assert.rejects(updateNativeCaptionSurface({ ...surface, radius: 65 }));
  assert.deepEqual(received, [surface, null]);
});

test("older native hosts retain a complete renderer surface", async () => {
  installHost({ windowShell: { updateContext: async () => {} } });
  assert.equal(supportsNativeCaptionSurface(), false);
  assert.equal(supportsNativeTitlebarControls(), false);
  await updateNativeCaptionSurface(null);
});
