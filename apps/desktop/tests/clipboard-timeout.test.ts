import assert from "node:assert/strict";
import test from "node:test";

import { createCopyController } from "../src/lib/clipboard.js";

function deferred() {
  let resolve!: () => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<void>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

test("a timed-out browser write stays visibly locked until settlement and cannot report late success", async (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  for (const lateFailure of [false, true]) {
    const pending = deferred();
    const writes: string[] = [];
    const controller = createCopyController("original\n", (text) => {
      writes.push(text);
      return writes.length === 1 ? pending.promise : Promise.resolve();
    }, { timeoutMs: 10 });
    context.after(() => controller.dispose());
    const states: string[] = [];
    controller.subscribe(() => states.push(controller.getSnapshot().status));
    const copy = controller.copy();
    context.mock.timers.tick(9);
    assert.equal(controller.getSnapshot().status, "copying");
    context.mock.timers.tick(1);
    assert.equal(await copy, "error");
    assert.equal(controller.getSnapshot().status, "error");
    assert.equal(controller.getSnapshot().writePending, true);
    assert.match(controller.getSnapshot().error ?? "", /仍未响应.*手动复制.*请求结束后才能重试/);
    assert.equal(await controller.copy(), "ignored");
    assert.deepEqual(writes, ["original\n"], "timeout cannot overlap an uncancellable browser write");
    if (lateFailure) pending.reject(new Error("private platform failure")); else pending.resolve();
    await pending.promise.catch(() => {});
    assert.equal(controller.getSnapshot().status, "error");
    assert.notEqual(controller.getSnapshot().writePending, true);
    assert.match(controller.getSnapshot().error ?? "", /已超时.*重试/);
    assert.doesNotMatch(controller.getSnapshot().error ?? "", /private|platform/);
    assert.equal(states.includes("copied"), false, "neither late resolve nor reject fabricates success");
    controller.setText("latest\tanswer\r\n");
    assert.equal(await controller.copy(), "copied");
    assert.deepEqual(writes, ["original\n", "latest\tanswer\r\n"]);
    controller.dispose();
  }
});

test("streamed source changes keep a hanging OS barrier visible and retry only the latest source", async (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const pending = deferred();
  const writes: string[] = [];
  const controller = createCopyController("half", (text) => {
    writes.push(text);
    return writes.length === 1 ? pending.promise : Promise.resolve();
  }, { timeoutMs: 10 });
  context.after(() => controller.dispose());
  const old = controller.copy();
  controller.setText("half finished\n");
  context.mock.timers.tick(10);
  assert.equal(await old, "stale");
  assert.equal(controller.getSnapshot().writePending, true);
  controller.setText("complete\tanswer\r\n");
  assert.equal(controller.getSnapshot().writePending, true);
  assert.equal(await controller.copy(), "ignored");
  pending.resolve(); await pending.promise;
  assert.deepEqual(controller.getSnapshot(), { status: "idle", error: null });
  assert.equal(await controller.copy(), "copied");
  assert.deepEqual(writes, ["half", "complete\tanswer\r\n"]);
});

test("disposal resolves the local wait, clears its deadline and preserves the OS lock across reactivation", async (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const pending = deferred();
  let writes = 0;
  const controller = createCopyController("text", () => ++writes === 1 ? pending.promise : Promise.resolve(), { timeoutMs: 10 });
  context.after(() => controller.dispose());
  let notifications = 0;
  controller.subscribe(() => { notifications++; });
  const copy = controller.copy();
  controller.dispose();
  assert.equal(await copy, "stale", "unmounted callers do not wait forever for the browser");
  controller.activate();
  assert.equal(controller.getSnapshot().status, "copying");
  assert.equal(await controller.copy(), "ignored");
  context.mock.timers.tick(10);
  assert.equal(controller.getSnapshot().writePending, true, "effect reactivation arms a fresh bounded wait");
  controller.dispose();
  const before = notifications;
  pending.reject(new Error("late OS error")); await pending.promise.catch(() => {});
  context.mock.timers.tick(100000);
  assert.equal(notifications, before);
  assert.deepEqual(controller.getSnapshot(), { status: "idle", error: null });
  controller.activate();
  assert.equal(await controller.copy(), "copied");
  assert.equal(writes, 2);
});

test("missing clipboard and throwing accessors stay safe and allow explicit retry when access recovers", async (context) => {
  const descriptor = Object.getOwnPropertyDescriptor(globalThis, "navigator");
  context.after(() => {
    if (descriptor) Object.defineProperty(globalThis, "navigator", descriptor);
    else Reflect.deleteProperty(globalThis, "navigator");
  });
  const controller = createCopyController("exact\ttext\r\n");
  context.after(() => controller.dispose());
  Object.defineProperty(globalThis, "navigator", { configurable: true, value: {} });
  assert.equal(await controller.copy(), "error");
  assert.match(controller.getSnapshot().error ?? "", /当前环境无法访问剪贴板/);
  const restricted = Object.defineProperty({}, "clipboard", { get() { throw new Error("private accessor detail"); } });
  Object.defineProperty(globalThis, "navigator", { configurable: true, value: restricted });
  assert.equal(await controller.copy(), "error");
  assert.match(controller.getSnapshot().error ?? "", /允许剪贴板访问后重试/);
  assert.doesNotMatch(controller.getSnapshot().error ?? "", /private|accessor/);
  const writes: string[] = [];
  Object.defineProperty(globalThis, "navigator", { configurable: true, value: { clipboard: { writeText: async (text: string) => { writes.push(text); } } } });
  assert.equal(await controller.copy(), "copied");
  assert.deepEqual(writes, ["exact\ttext\r\n"]);
});
