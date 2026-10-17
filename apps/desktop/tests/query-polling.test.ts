import assert from "node:assert/strict";
import { test } from "node:test";

import { installHost, removeHost } from "./host-mock.js";

// Query Core determines browser timer support at import time.
Object.defineProperty(globalThis, "window", { value: {}, configurable: true });
const { QueryClient, QueryObserver, timeoutManager } = await import("@tanstack/react-query");
const { serverListQueryOptions } = await import("../src/lib/servers.js");
const { agentStatusQueryOptions } = await import("../src/lib/runtime.js");

const intervals = new Map<number, { callback: () => void; delay: number }>();
let timerId = 0;
timeoutManager.setTimeoutProvider({
  setTimeout: () => ++timerId,
  clearTimeout: () => {},
  setInterval: (callback, delay) => {
    const id = ++timerId;
    intervals.set(id, { callback, delay });
    return id;
  },
  clearInterval: (id) => { if (id !== undefined) intervals.delete(id); },
});
const settle = () => new Promise<void>((resolve) => setImmediate(resolve));

test("many server readers share one polling owner; unmount releases its timer", async () => {
  const client = new QueryClient();
  let calls = 0;
  installHost({ invoke: async () => { calls++; return { servers: [] }; } });
  const owner = new QueryObserver(client, serverListQueryOptions({ poll: true }));
  const readers = Array.from({ length: 8 }, () => new QueryObserver(client, serverListQueryOptions()));
  const stopOwner = owner.subscribe(() => {});
  const stops = readers.map((reader) => reader.subscribe(() => {}));
  try {
    await settle();
    assert.equal(calls, 1);
    assert.deepEqual([...intervals.values()].map((timer) => timer.delay), [60_000]);
    for (const timer of [...intervals.values()]) timer.callback();
    await settle();
    assert.equal(calls, 2, "a tick must issue one IPC request, regardless of reader count");
    assert.ok(readers.every((reader) => reader.getCurrentResult().data?.length === 0));
    stopOwner();
    assert.equal(intervals.size, 0);
    await client.invalidateQueries({ queryKey: ["servers"] });
    assert.equal(calls, 3, "readers must still refresh after mutations without owning a timer");
  } finally {
    stopOwner();
    stops.forEach((stop) => stop());
    client.clear();
    removeHost();
  }
});

test("Agent readers share adaptive polling and disabled queries schedule nothing", async () => {
  const client = new QueryClient();
  let running = false;
  let calls = 0;
  installHost({ invoke: async () => {
    calls++;
    return { running, pid: null, protocolVersion: null, agentVersion: null,
      toolCount: null, entry: null, startedAt: null, lastExit: null };
  } });
  const owner = new QueryObserver(client, agentStatusQueryOptions({ poll: true }));
  const reader = new QueryObserver(client, agentStatusQueryOptions());
  const stopOwner = owner.subscribe(() => {});
  const stopReader = reader.subscribe(() => {});
  const disabled = new QueryObserver(client, serverListQueryOptions({ enabled: false, poll: true }));
  const stopDisabled = disabled.subscribe(() => {});
  try {
    await settle();
    assert.equal(calls, 1);
    assert.deepEqual([...intervals.values()].map((timer) => timer.delay), [5_000]);
    running = true;
    for (const timer of [...intervals.values()]) timer.callback();
    await settle();
    assert.equal(calls, 2);
    assert.equal(reader.getCurrentResult().data?.running, true);
    assert.deepEqual([...intervals.values()].map((timer) => timer.delay), [1_500]);
    stopOwner();
    assert.equal(intervals.size, 0);
  } finally {
    stopOwner();
    stopReader();
    stopDisabled();
    client.clear();
    removeHost();
  }
});
