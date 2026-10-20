import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { renderToStaticMarkup } from "react-dom/server";
import { QueryClient, QueryObserver } from "@tanstack/react-query";
import { ServerSnapshotSchema } from "@yukinal/shared";
import { ServerToolkitSections } from "../src/features/overview/ServerToolkitSections.js";
import { prepareServerSnapshot, serverSnapshotQueryOptions } from "../src/lib/server-snapshot.js";
import { installHost, removeHost } from "./host-mock.js";

const fixture = ServerSnapshotSchema.parse(JSON.parse(readFileSync(new URL("../../../packages/shared/fixtures/server_toolkit.json", import.meta.url), "utf8")));
const settle = () => new Promise<void>((resolve) => setImmediate(resolve));
Object.defineProperty(globalThis, "window", { value: {}, configurable: true });

test("GPU and security stay compact and expose observed values on expansion", () => {
  const html = renderToStaticMarkup(<ServerToolkitSections snapshot={fixture} />);
  assert.match(html, /73%/);
  assert.match(html, /NVIDIA T4/);
  assert.match(html, /当前以 root 连接/);
  assert.match(html, /检测工具不可用/);
  assert.match(html, /监听端口不代表公网可达/);
  assert.equal((html.match(/<details/g) ?? []).length, 2);
  assert.doesNotMatch(html, /<details[^>]* open/);
});

test("missing, failed and unsupported GPU readings never become zero usage", () => {
  for (const gpu of [undefined, { status: "unavailable", message: "检测工具不可用", devices: [] }, { status: "error", message: "检测超时", devices: [] }] as const) {
    const html = renderToStaticMarkup(<ServerToolkitSections snapshot={{ ...fixture, gpu: gpu as typeof fixture.gpu, security: undefined }} />);
    assert.doesNotMatch(html, /0%/);
    assert.doesNotMatch(html, /已安全|安全通过/);
  }
});

test("failed extension collectors suppress retained GPU and security readings", () => {
  const html = renderToStaticMarkup(<ServerToolkitSections snapshot={{ ...fixture, collectors: [
    { collectorId: "gpu", ok: false, collectedAt: fixture.collectedAt, error: "GPU_FIXTURE_FAILED" },
    { collectorId: "security", ok: false, collectedAt: fixture.collectedAt, error: "SECURITY_FIXTURE_FAILED" },
  ] }} />);
  assert.doesNotMatch(html, /73%|NVIDIA T4|当前以 root 连接/);
  assert.match(html, /GPU_FIXTURE_FAILED/);
  assert.match(html, /SECURITY_FIXTURE_FAILED/);
});

test("connection warm-up and mounted inspector share one request; reconnect forces a new sample", async () => {
  const client = new QueryClient({ defaultOptions: { queries: { gcTime: Infinity } } });
  let calls = 0;
  let finish: (() => void) | undefined;
  installHost({ invoke: async () => {
    calls++;
    await new Promise<void>((resolve) => { finish = resolve; });
    return { snapshot: fixture };
  } });
  const preparing = prepareServerSnapshot(client, fixture.serverId);
  await settle();
  const observer = new QueryObserver(client, serverSnapshotQueryOptions(fixture.serverId));
  const stop = observer.subscribe(() => {});
  try {
    assert.equal(calls, 1);
    finish?.();
    await preparing;
    assert.equal(observer.getCurrentResult().data?.gpu?.devices[0]?.usagePercent, 73);
    const reconnecting = prepareServerSnapshot(client, fixture.serverId);
    await settle();
    assert.equal(calls, 2, "a recent pre-reconnect sample must not suppress a fresh check");
    finish?.();
    await reconnecting;
  } finally { stop(); client.clear(); removeHost(); }
});

test("late result from a different server is rejected before entering that server's cache", async () => {
  const client = new QueryClient({ defaultOptions: { queries: { gcTime: Infinity } } });
  installHost({ invoke: async () => ({ snapshot: fixture }) });
  try {
    await assert.rejects(client.fetchQuery(serverSnapshotQueryOptions("srv_other")), /不匹配/);
    assert.equal(client.getQueryData(["serverSnapshot", "srv_other"]), undefined);
    assert.equal(client.getQueryData(["serverSnapshot", fixture.serverId]), undefined);
  } finally { client.clear(); removeHost(); }
});
