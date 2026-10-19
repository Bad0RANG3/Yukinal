import assert from "node:assert/strict";
import test from "node:test";
import { IPC_COMMANDS, type AgentStatus, type AiProviderConfig } from "@yukinal/shared";

import { installHost, removeHost } from "./host-mock.js";

Object.defineProperty(globalThis, "window", { value: {}, configurable: true });
const { QueryClient, QueryObserver } = await import("@tanstack/react-query");
const { agentStatusQueryOptions, RUNTIME_QUERY_KEYS } = await import("../src/lib/runtime.js");
const { PROVIDERS_QUERY_KEY } = await import("../src/lib/providers.js");
const { callDesktop } = await import("../src/lib/ipc.js");

const provider: AiProviderConfig = {
  id: "offline-test",
  kind: "openai-compatible",
  label: "Offline model",
  baseUrl: "http://127.0.0.1:1/v1",
  model: "test-model",
  enabled: true,
  createdAt: "2026-01-01T00:00:00Z",
  updatedAt: "2026-01-01T00:00:00Z",
};

function status(running: boolean): AgentStatus {
  return { running, pid: null, protocolVersion: null, agentVersion: null,
    toolCount: null, entry: null, startedAt: null, lastExit: null };
}

async function settleUntil(condition: () => boolean): Promise<void> {
  for (let turn = 0; turn < 20 && !condition(); turn++) {
    await new Promise<void>((resolve) => setImmediate(resolve));
  }
  assert.ok(condition(), "query did not reach the expected state");
}

function setup(cachedRunning?: boolean) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: Infinity, staleTime: Infinity } },
  });
  let running = false;
  const calls = { providers: 0, status: 0 };
  installHost({ invoke: async (command) => {
    if (command === IPC_COMMANDS.agentStatus) {
      calls.status++;
      return status(running);
    }
    if (command === IPC_COMMANDS.providerList) {
      calls.providers++;
      if (!running) throw new Error("AGENT_UNAVAILABLE");
      return { providers: [provider] };
    }
    throw new Error(`Unexpected offline command: ${command}`);
  } });
  if (cachedRunning !== undefined) {
    client.setQueryData(RUNTIME_QUERY_KEYS.status, status(cachedRunning));
  }
  const providers = new QueryObserver(client, {
    queryKey: PROVIDERS_QUERY_KEY,
    queryFn: async () => (await callDesktop(IPC_COMMANDS.providerList, {})).providers,
  });
  const stop = providers.subscribe(() => {});
  return {
    calls,
    providers,
    setRunning: (value: boolean) => { running = value; },
    readStatus: () => client.fetchQuery({ ...agentStatusQueryOptions(), staleTime: 0 }),
    close: () => { stop(); client.clear(); removeHost(); },
  };
}

test("the first running observation recovers a failed provider read even with cached runtime status", async () => {
  const app = setup(true);
  try {
    await settleUntil(() => app.providers.getCurrentResult().isError);
    assert.equal(app.calls.providers, 1);
    app.setRunning(true);

    await app.readStatus();

    await settleUntil(() => app.providers.getCurrentResult().isSuccess);
    assert.deepEqual(app.providers.getCurrentResult().data, [provider]);
    assert.equal(app.calls.providers, 2);
  } finally { app.close(); }
});

test("automatic startup from stopped to running refreshes providers without an explicit spawn", async () => {
  const app = setup();
  try {
    await settleUntil(() => app.providers.getCurrentResult().isError);
    assert.equal((await app.readStatus()).running, false);
    assert.equal(app.calls.providers, 1);
    app.setRunning(true);

    assert.equal((await app.readStatus()).running, true);

    await settleUntil(() => app.providers.getCurrentResult().isSuccess);
    assert.equal(app.calls.providers, 2);
  } finally { app.close(); }
});

test("stable running polls and shared status readers do not repeatedly reload providers", async () => {
  const app = setup();
  try {
    await settleUntil(() => app.providers.getCurrentResult().isError);
    app.setRunning(true);
    await app.readStatus();
    await settleUntil(() => app.providers.getCurrentResult().isSuccess);
    const providerCalls = app.calls.providers;

    await Promise.all([app.readStatus(), app.readStatus(), app.readStatus()]);
    await app.readStatus();

    assert.equal(app.calls.providers, providerCalls);
    assert.equal(app.calls.status, 3, "concurrent readers must share one status request");
  } finally { app.close(); }
});
