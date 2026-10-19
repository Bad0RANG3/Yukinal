import assert from "node:assert/strict";
import test from "node:test";
import { IPC_COMMANDS, type AgentStatus } from "@yukinal/shared";
import { installHost, removeHost } from "./host-mock.js";

Object.defineProperty(globalThis, "window", { value: {}, configurable: true });
const { QueryClient, QueryObserver } = await import("@tanstack/react-query");
const { agentStatusQueryOptions, RUNTIME_QUERY_KEYS } = await import("../src/lib/runtime.js");
const settle = () => new Promise<void>((resolve) => setImmediate(resolve));
const cached: AgentStatus = { running: false, pid: null, protocolVersion: null,
  agentVersion: null, toolCount: null, entry: null, startedAt: null, lastExit: null };

test("hidden diagnostics retain cached status without fetching on invalidation", async () => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  let calls = 0;
  installHost({ invoke: async () => { calls++; return cached; } });
  client.setQueryData(RUNTIME_QUERY_KEYS.status, cached);
  const observer = new QueryObserver(client, agentStatusQueryOptions({ enabled: false }));
  const stop = observer.subscribe(() => {});
  try {
    await client.invalidateQueries({ queryKey: RUNTIME_QUERY_KEYS.status });
    await settle();
    assert.equal(calls, 0);
    assert.deepEqual(observer.getCurrentResult().data, cached);
    assert.equal(observer.getCurrentResult().isFetching, false);
    observer.setOptions(agentStatusQueryOptions({ enabled: true }));
    await settle();
    assert.equal(calls, 1, "opening diagnostics should fetch current state once");
  } finally { stop(); client.clear(); removeHost(); }
});

test("an active conversation keeps health monitoring when diagnostics are hidden", async () => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  let calls = 0;
  installHost({ invoke: async (command) => {
    assert.equal(command, IPC_COMMANDS.agentStatus);
    calls++;
    return cached;
  } });
  const hidden = new QueryObserver(client, agentStatusQueryOptions({ enabled: false }));
  const conversation = new QueryObserver(client, agentStatusQueryOptions({ poll: true }));
  const stops = [hidden.subscribe(() => {}), conversation.subscribe(() => {})];
  try {
    await settle();
    assert.equal(calls, 1);
    assert.deepEqual(hidden.getCurrentResult().data, cached);
    await client.invalidateQueries({ queryKey: RUNTIME_QUERY_KEYS.status });
    assert.equal(calls, 2, "only the active conversation owns status fetching");
  } finally { stops.forEach((stop) => stop()); client.clear(); removeHost(); }
});


test("a hidden model editor keeps cached choices without refreshing on focus, reconnect or provider invalidation", async () => {
  const { providerModelsQueryOptions, PROVIDERS_QUERY_KEY } = await import("../src/lib/providers.js");
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  let calls = 0;
  const models = [{ id: "test-model", label: "Test model" }];
  installHost({ invoke: async (command) => {
    assert.equal(command, IPC_COMMANDS.providerModels);
    calls++;
    return { models };
  } });
  const options = providerModelsQueryOptions("test-provider", { enabled: false });
  client.setQueryData(options.queryKey, models);
  const observer = new QueryObserver(client, options);
  const stop = observer.subscribe(() => {});
  try {
    await client.invalidateQueries({ queryKey: PROVIDERS_QUERY_KEY });
    client.getQueryCache().onFocus();
    client.getQueryCache().onOnline();
    await settle();
    assert.equal(calls, 0);
    assert.deepEqual(observer.getCurrentResult().data, models);
    observer.setOptions(providerModelsQueryOptions("test-provider"));
    await settle();
    assert.equal(calls, 1, "returning to the editor refreshes the directory once");
    observer.setOptions(providerModelsQueryOptions("test-provider", { enabled: false }));
    await client.invalidateQueries({ queryKey: PROVIDERS_QUERY_KEY });
    await settle();
    assert.equal(calls, 1, "hiding the editor disables further directory refreshes");
  } finally { stop(); client.clear(); removeHost(); }
});
