import assert from "node:assert/strict";
import { test } from "node:test";
import { installHost, removeHost } from "./host-mock.js";

Object.defineProperty(globalThis, "window", { value: {}, configurable: true });
const { QueryClient, QueryObserver } = await import("@tanstack/react-query");
const { serverLogsQueryOptions, serverServicesQueryOptions } = await import("../src/lib/remote-feeds.js");
const settle = () => new Promise<void>((resolve) => setImmediate(resolve));

test("hidden log and service pages issue no IPC or polling", async () => {
  const client = new QueryClient();
  let calls = 0;
  installHost({ invoke: async () => { calls++; return {}; } });
  const logOptions = serverLogsQueryOptions("srv_feed", false);
  const serviceOptions = serverServicesQueryOptions("srv_feed", false);
  const logs = new QueryObserver(client, logOptions);
  const services = new QueryObserver(client, serviceOptions);
  const stops = [logs.subscribe(() => {}), services.subscribe(() => {})];
  try {
    await settle();
    assert.equal(calls, 0);
    for (const options of [logOptions, serviceOptions]) {
      assert.equal(options.refetchInterval, false);
      assert.equal(options.retry, false);
      assert.equal(options.refetchOnWindowFocus, false);
      assert.equal(options.refetchOnReconnect, false);
    }
  } finally { stops.forEach((stop) => stop()); client.clear(); removeHost(); }
});

test("visible copies and repeated refresh share one pending native request", async () => {
  const client = new QueryClient();
  let calls = 0;
  let complete: (value: unknown) => void = () => {};
  installHost({ invoke: async () => {
    calls++;
    return new Promise((resolve) => { complete = resolve; });
  } });
  const first = new QueryObserver(client, serverLogsQueryOptions("srv_feed"));
  const second = new QueryObserver(client, serverLogsQueryOptions("srv_feed"));
  const stops = [first.subscribe(() => {}), second.subscribe(() => {})];
  try {
    await settle();
    const refreshes = Array.from({ length: 8 }, (_, index) => (index % 2 ? first : second).refetch({ cancelRefetch: false }));
    assert.equal(calls, 1);
    complete({ source: "journalctl", lines: [{ text: "ready", level: "info" }] });
    await Promise.all(refreshes);
    assert.equal(calls, 1);
    assert.equal(second.getCurrentResult().data?.lines[0]?.text, "ready");
  } finally { stops.forEach((stop) => stop()); client.clear(); removeHost(); }
});

test("reopening a fresh feed uses cache and another server remains isolated", async () => {
  const client = new QueryClient();
  let calls = 0;
  installHost({ invoke: async () => { calls++; return { source: "systemd", services: [] }; } });
  client.setQueryData(["serverServices", "srv_cached"], { source: "systemd", services: [] });
  const cached = new QueryObserver(client, serverServicesQueryOptions("srv_cached"));
  const other = new QueryObserver(client, serverServicesQueryOptions("srv_other"));
  const stops = [cached.subscribe(() => {}), other.subscribe(() => {})];
  try {
    await settle();
    assert.equal(calls, 1);
    assert.equal(cached.getCurrentResult().isFetching, false);
    assert.equal(other.getCurrentResult().data?.source, "systemd");
  } finally { stops.forEach((stop) => stop()); client.clear(); removeHost(); }
});

test("failed remote reads stop after one call and keep cached results", async () => {
  const client = new QueryClient();
  let calls = 0;
  installHost({ invoke: async () => { calls++; throw new Error("SSH timed out"); } });
  client.setQueryData(["serverLogs", "srv_feed"], { source: "journalctl", lines: [{ text: "cached", level: "info" }] });
  const logs = new QueryObserver(client, serverLogsQueryOptions("srv_feed"));
  const stop = logs.subscribe(() => {});
  try {
    await logs.refetch({ cancelRefetch: false });
    assert.equal(calls, 1);
    assert.equal(logs.getCurrentResult().isError, true);
    assert.equal(logs.getCurrentResult().data?.lines[0]?.text, "cached");
  } finally { stop(); client.clear(); removeHost(); }
});
