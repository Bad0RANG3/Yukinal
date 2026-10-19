import assert from "node:assert/strict";
import test from "node:test";

import { createCopyController, writeClipboardText } from "../src/lib/clipboard.js";

function deferred() {
  let resolve!: () => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<void>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

test("copy starts in the click stack, preserves all source bytes, and reports success only after resolution", async (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const pending = deferred();
  const original = "\tif (ready) {  \r\n\t\twork();\r\n}\r\n\r\n";
  const writes: string[] = [];
  const controller = createCopyController(original, (text) => { writes.push(text); return pending.promise; });
  context.after(() => controller.dispose());
  const states: string[] = [];
  controller.subscribe(() => states.push(controller.getSnapshot().status));
  const copy = controller.copy();
  assert.deepEqual(writes, [original], "the actual clipboard call starts before yielding user activation");
  assert.equal(controller.getSnapshot().status, "copying");
  assert.equal(await controller.copy(), "ignored", "double clicks cannot queue another write");
  pending.resolve();
  assert.equal(await copy, "copied");
  assert.deepEqual(states, ["copying", "copied"]);
  context.mock.timers.tick(1999);
  assert.equal(controller.getSnapshot().status, "copied");
  context.mock.timers.tick(1);
  assert.equal(controller.getSnapshot().status, "idle");
});

test("a denied write remains visibly failed with safe text and can be retried manually", async (context) => {
  let attempts = 0;
  const controller = createCopyController("answer\n", async () => {
    if (attempts++ === 0) throw new Error("private clipboard detail: secret");
  });
  context.after(() => controller.dispose());
  assert.equal(await controller.copy(), "error");
  assert.equal(controller.getSnapshot().status, "error");
  assert.match(controller.getSnapshot().error ?? "", /允许剪贴板访问后重试/);
  assert.doesNotMatch(controller.getSnapshot().error ?? "", /private|secret/);
  assert.equal(attempts, 1, "no automatic retry follows a failure");
  assert.equal(await controller.copy(), "copied");
  assert.equal(attempts, 2);
});

test("stream updates clear feedback, discard a pending old success and copy the latest exact text next", async (context) => {
  const pending = deferred();
  const writes: string[] = [];
  const controller = createCopyController("half", (text) => {
    writes.push(text);
    return writes.length === 1 ? pending.promise : Promise.resolve();
  });
  context.after(() => controller.dispose());
  const first = controller.copy();
  controller.setText("half\t finished\r\n");
  assert.equal(await controller.copy(), "ignored", "a changed stream cannot overlap a still-pending OS write");
  pending.resolve();
  assert.equal(await first, "stale");
  assert.equal(controller.getSnapshot().status, "idle");
  assert.equal(await controller.copy(), "copied");
  assert.deepEqual(writes, ["half", "half\t finished\r\n"]);
  controller.setText("next");
  assert.equal(controller.getSnapshot().status, "idle");
});

test("changing source cancels its old success timer rather than clearing newer feedback", async (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const controller = createCopyController("one", async () => {});
  context.after(() => controller.dispose());
  await controller.copy();
  context.mock.timers.tick(1500);
  controller.setText("two");
  await controller.copy();
  context.mock.timers.tick(500);
  assert.equal(controller.getSnapshot().status, "copied", "the first write's old timer was cleared");
  context.mock.timers.tick(1500);
  assert.equal(controller.getSnapshot().status, "idle");
});

test("disposal prevents late success/error notifications, clears timers and supports an effect reactivation", async (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  for (const reject of [false, true]) {
    const pending = deferred();
    const controller = createCopyController("old", () => pending.promise);
    let notifications = 0;
    const unsubscribe = controller.subscribe(() => { notifications++; });
    const copy = controller.copy();
    controller.dispose();
    const before = notifications;
    if (reject) pending.reject(new Error("permission denied")); else pending.resolve();
    assert.equal(await copy, "stale");
    context.mock.timers.tick(10000);
    assert.equal(notifications, before);
    assert.equal(controller.getSnapshot().status, "idle");
    assert.equal(await controller.copy(), "ignored");
    unsubscribe();
  }
  const controller = createCopyController("new", async () => {});
  let notifications = 0;
  controller.subscribe(() => { notifications++; });
  await controller.copy();
  controller.dispose();
  const before = notifications;
  context.mock.timers.tick(10000);
  assert.equal(notifications, before, "unmount cleared the success timer");
  controller.activate();
  assert.equal(await controller.copy(), "copied", "React strict effect setup can reactivate the same control");
  controller.dispose();
});

test("a pending failure from a previous text cannot overwrite the current control", async (context) => {
  const pending = deferred();
  const controller = createCopyController("old", () => pending.promise);
  context.after(() => controller.dispose());
  const copy = controller.copy();
  controller.setText("new");
  pending.reject(new Error("old permission error"));
  assert.equal(await copy, "stale");
  assert.deepEqual(controller.getSnapshot(), { status: "idle", error: null });
});

test("an empty copy is inert while whitespace-only source is copied verbatim", async (context) => {
  const writes: string[] = [];
  const controller = createCopyController("", async (text) => { writes.push(text); });
  context.after(() => controller.dispose());
  assert.equal(await controller.copy(), "ignored");
  assert.deepEqual(writes, []);
  controller.setText("\t \r\n");
  assert.equal(await controller.copy(), "copied");
  assert.deepEqual(writes, ["\t \r\n"]);
});

test("the product writer uses real navigator clipboard with its receiver and awaits permission", async (context) => {
  const descriptor = Object.getOwnPropertyDescriptor(globalThis, "navigator");
  context.after(() => {
    if (descriptor) Object.defineProperty(globalThis, "navigator", descriptor);
    else Reflect.deleteProperty(globalThis, "navigator");
  });
  const pending = deferred();
  const clipboard = {
    writeText(text: string) {
      assert.equal(this, clipboard);
      assert.equal(text, "\tcode\r\n");
      return pending.promise;
    },
  };
  Object.defineProperty(globalThis, "navigator", { configurable: true, value: { clipboard } });
  let complete = false;
  const copy = writeClipboardText("\tcode\r\n").then(() => { complete = true; });
  await Promise.resolve();
  assert.equal(complete, false);
  pending.resolve();
  await copy;
  assert.equal(complete, true);
  Object.defineProperty(globalThis, "navigator", { configurable: true, value: {} });
  await assert.rejects(writeClipboardText("text"), /当前环境无法访问剪贴板/);
  Object.defineProperty(globalThis, "navigator", { configurable: true, value: {
    clipboard: { writeText() { throw new Error("private browser error"); } },
  } });
  await assert.rejects(writeClipboardText("text"), (error: unknown) => {
    assert.ok(error instanceof Error);
    assert.match(error.message, /允许剪贴板访问后重试/);
    assert.doesNotMatch(error.message, /private/);
    return true;
  });
});
