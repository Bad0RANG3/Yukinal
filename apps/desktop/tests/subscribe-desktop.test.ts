/**
 * `subscribeDesktop`: the leak that the unlisten race causes, pinned.
 *
 * `listenDesktop` resolves the unlisten function through a promise, but every caller
 * unsubscribes synchronously. Get that wrong and the Tauri-side listener outlives the
 * component that asked for it, still firing into a dead handler — visible only under
 * fast navigation, which is why the idiom was written out by hand four times with
 * three slightly different shapes.
 *
 * These tests drive the real `plugin:event|listen` / `plugin:event|unlisten` IPC
 * calls through `mockIPC`, so they exercise the actual `@tauri-apps/api/event` code
 * path rather than a stub of it.
 */

import assert from "node:assert/strict";
import { webcrypto } from "node:crypto";
import { afterEach, test } from "node:test";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { tauriEventName } from "@yukinal/shared";

import { subscribeDesktop } from "../src/lib/ipc.js";

/**
 * Tauri's `listen` goes through `transformCallback`, which mints a callback id with
 * `window.crypto.getRandomValues`. There is no DOM here, so the fake window has to
 * carry a real WebCrypto — a stub returning zeros would collide callback ids and
 * make these tests pass for the wrong reason.
 */
Object.defineProperty(globalThis, "window", {
  value: { __TAURI_INTERNALS__: {}, crypto: webcrypto },
  configurable: true,
});

afterEach(() => {
  clearMocks();
});

/** Records every listen/unlisten round trip and lets a test control their ordering. */
function recordingBridge() {
  const listens: string[] = [];
  const unlistens: string[] = [];
  let releaseListen: (() => void) | undefined;
  const gate = new Promise<void>((resolve) => { releaseListen = resolve; });
  let holdListens = false;

  mockIPC((command, args) => {
    if (command === "plugin:event|listen") {
      listens.push(String((args as { event: string }).event));
      if (holdListens) return gate.then(() => 1);
      return 1;
    }
    if (command === "plugin:event|unlisten") {
      unlistens.push(String((args as { event: string }).event));
      return undefined;
    }
    return undefined;
  });

  return {
    listens,
    unlistens,
    hold: () => { holdListens = true; },
    release: () => releaseListen?.(),
  };
}

/** Let the microtask queue drain so the listen promise settles. */
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

test("subscribing registers the channel and unsubscribing releases it", async () => {
  const bridge = recordingBridge();
  const { stop } = subscribeDesktop("activity.created", () => {});
  await settle();
  assert.deepEqual(bridge.listens, [tauriEventName("activity.created")], "the channel was never registered");
  // 事件名在 IPC 线上用的是冒号形式（`activity.created` → `activity:created`），
  // 由 shared 的 `tauriEventName` 唯一决定。断一次真正的线上名字，免得上面那句
  // 断言两边都用同一个函数而永远成立。
  assert.equal(bridge.listens[0], "activity:created");

  stop();
  await settle();
  assert.deepEqual(bridge.unlistens, ["activity:created"], "unsubscribe did not reach Tauri");
});

test("unsubscribing before the listen resolves still releases the channel", async () => {
  // This is the leak. The caller tears down while `listen` is still in flight; the
  // unlisten function then arrives with nobody left to receive it. Unsubscribing on
  // arrival is the only thing that prevents a permanently registered listener.
  const bridge = recordingBridge();
  bridge.hold();

  const { stop } = subscribeDesktop("terminal.data", () => {});
  stop(); // teardown happens first — exactly the fast-navigation case
  await settle();
  assert.deepEqual(bridge.unlistens, [], "nothing to unsubscribe yet, correctly");

  bridge.release();
  await settle();
  assert.deepEqual(bridge.listens, ["terminal:data"]);
  assert.deepEqual(
    bridge.unlistens,
    ["terminal:data"],
    "the late subscription was not unsubscribed: this is the listener leak",
  );
});

test("unsubscribing twice is harmless", async () => {
  // React 19 runs effects twice in development under StrictMode, so a cleanup can
  // legitimately run more than once against the same subscription.
  const bridge = recordingBridge();
  const { stop } = subscribeDesktop("activity.created", () => {});
  await settle();
  stop();
  stop();
  await settle();
  assert.deepEqual(bridge.listens, ["activity:created"], "a second teardown re-listened");
  assert.equal(bridge.unlistens.length, 2, "the second teardown is a no-op, not a crash");
});

test("a collapsed pair of subscribe/unsubscribe leaves nothing registered", async () => {
  // Two channels opened and closed back to back, both resolving late: the counts must
  // match, which is the invariant that actually matters (no dangling listener).
  const bridge = recordingBridge();
  bridge.hold();

  const { stop: stopA } = subscribeDesktop("terminal.data", () => {});
  const { stop: stopB } = subscribeDesktop("terminal.closed", () => {});
  stopA();
  stopB();
  bridge.release();
  await settle();

  assert.equal(bridge.listens.length, 2);
  assert.equal(
    bridge.unlistens.length,
    2,
    `registered ${bridge.listens.length} but released ${bridge.unlistens.length}: one listener leaked`,
  );
});
