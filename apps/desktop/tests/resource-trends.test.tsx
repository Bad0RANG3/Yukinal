import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";
import type { ServerSnapshot } from "@yukinal/shared";
import { ResourceTrends } from "../src/features/overview/ResourceTrends.js";
import {
  appendResourceSample,
  resourceTrendSegments,
  resourceValues,
  ResourceTrendStore,
  RESOURCE_TREND_SAMPLE_LIMIT,
  RESOURCE_TREND_SERVER_LIMIT,
  RESOURCE_TREND_WINDOW_MS,
  type ResourceSample,
} from "../src/features/overview/resource-trends.js";

const start = Date.parse("2026-10-18T00:00:00Z");
function snapshot(seconds = 0, overrides: Partial<ServerSnapshot> = {}): ServerSnapshot {
  return {
    id: `snapshot-${seconds}`,
    serverId: "server-a",
    collectedAt: new Date(start + seconds * 1_000).toISOString(),
    health: "healthy",
    capabilities: {},
    cpu: { model: "Test CPU", cores: 2, usagePercent: 12.5, loadAverage: [0, 0, 0] },
    memory: { totalBytes: 100, usedBytes: 40, availableBytes: 60, usagePercent: 40 },
    disks: [
      { device: "vda", mountPoint: "/", totalBytes: 100, usedBytes: 30, usagePercent: 30 },
      { device: "vdb", mountPoint: "/data", totalBytes: 100, usedBytes: 65, usagePercent: 65 },
    ],
    ...overrides,
  };
}

test("only distinct real timestamps add samples, in chronological order", () => {
  const first = appendResourceSample([], snapshot());
  assert.equal(appendResourceSample(first, snapshot()), first);
  assert.equal(appendResourceSample(first, snapshot(0, { collectedAt: "2026-10-18T08:00:00+08:00" })), first);
  assert.equal(appendResourceSample(first, snapshot(15, { collectedAt: "not a timestamp" })), first);
  const outOfOrder = appendResourceSample(appendResourceSample(first, snapshot(30)), snapshot(15));
  assert.deepEqual(outOfOrder.map((sample) => sample.timestamp - start), [0, 15_000, 30_000]);
  assert.deepEqual(outOfOrder.map((sample) => sample.cpu), [12.5, 12.5, 12.5]);
});

test("each server has its own bounded sample window and old servers are evicted", () => {
  const store = new ResourceTrendStore();
  for (let index = 0; index < RESOURCE_TREND_SAMPLE_LIMIT + 5; index++) store.record(snapshot(index));
  assert.equal(store.read("server-a").length, RESOURCE_TREND_SAMPLE_LIMIT);
  assert.equal(store.read("server-a")[0]?.timestamp, start + 5_000);

  store.record(snapshot(2_000));
  assert.equal(store.read("server-a").length, 1);
  const previous = store.read("server-a");
  store.record(snapshot(2_015, { serverId: "server-b" }));
  assert.equal(store.read("server-a"), previous);
  assert.ok(store.read("server-b").every((sample) => sample.serverId === "server-b"));
  assert.ok(appendResourceSample(previous, snapshot(2_015, { serverId: "server-b" })).every((sample) => sample.serverId === "server-b"));

  for (let index = 0; index < RESOURCE_TREND_SERVER_LIMIT; index++) store.record(snapshot(0, { serverId: `other-${index}` }));
  assert.equal(store.read("server-a").length, 0);
  assert.equal(store.read("server-b").length, 0);
  assert.equal(store.read(`other-${RESOURCE_TREND_SERVER_LIMIT - 1}`).length, 1);
});

test("time retention removes obsolete observations even before the count limit", () => {
  let samples: readonly ResourceSample[] = [];
  for (const seconds of [0, 300, 600, 900, 1_200]) samples = appendResourceSample(samples, snapshot(seconds));
  assert.equal(samples.length, 4);
  assert.ok(samples.every((sample) => sample.timestamp >= start + 1_200_000 - RESOURCE_TREND_WINDOW_MS));
});

test("failed, missing and invalid readings stay unavailable while actual zero remains valid", () => {
  const base = snapshot();
  assert.deepEqual(resourceValues(base), { cpu: 12.5, memory: 40, disk: 65 });
  const failed = resourceValues(snapshot(15, {
    collectors: ["collector.cpu", "memory", "collector.disk"].map((collectorId) => ({ collectorId, ok: false, collectedAt: base.collectedAt })),
  }));
  assert.deepEqual(failed, { cpu: undefined, memory: undefined, disk: undefined });
  assert.deepEqual(resourceValues(snapshot(15, { cpu: undefined, memory: undefined, disks: [] })), failed);
  assert.deepEqual(resourceValues(snapshot(15, {
    cpu: { ...base.cpu!, usagePercent: Number.NaN },
    memory: { ...base.memory!, usagePercent: -1 },
    disks: [{ ...base.disks![0]!, usagePercent: 101 }],
  })), failed);
  assert.deepEqual(resourceValues(snapshot(15, {
    cpu: { ...base.cpu!, usagePercent: 0 },
    memory: { ...base.memory!, usagePercent: 0 },
    disks: [{ ...base.disks![0]!, usagePercent: 0 }],
  })), { cpu: 0, memory: 0, disk: 0 });
});

test("chart geometry uses elapsed time and a fixed percent scale with gaps for missing data and outages", () => {
  let samples: readonly ResourceSample[] = [];
  const base = snapshot();
  for (const [seconds, usage] of [[0, 0], [15, 50], [30, undefined], [45, 100], [120, 25]] as const) {
    samples = appendResourceSample(samples, snapshot(seconds, { cpu: usage === undefined ? undefined : { ...base.cpu!, usagePercent: usage } }));
  }
  const segments = resourceTrendSegments(samples, "cpu");
  assert.deepEqual(segments.map((segment) => segment.map((point) => point.value)), [[0, 50], [100], [25]]);
  assert.equal(segments[0]?.[0]?.x, 4);
  assert.equal(segments[0]?.[1]?.x, 33);
  assert.equal(segments[2]?.[0]?.x, 236);
  assert.equal(segments[0]?.[0]?.y, 50);
  assert.equal(segments[1]?.[0]?.y, 4);
});

test("first-sample charts show three current readings and actual dots without invented history", () => {
  const current = snapshot();
  const html = renderToStaticMarkup(<ResourceTrends snapshot={current} samples={appendResourceSample([], current)} />);
  assert.equal((html.match(/<figure /g) ?? []).length, 3);
  assert.equal((html.match(/role="img"/g) ?? []).length, 3);
  assert.equal((html.match(/class="overview-trend-point"/g) ?? []).length, 3);
  assert.ok(!html.includes('class="overview-trend-line"'));
  for (const value of ["12.5%", "40%", "65%", "首个采样", "等待积累趋势", "最高占用", "纵轴 0 到 100%", "aria-labelledby="]) assert.ok(html.includes(value), value);
});

test("failed latest collectors cannot paint cached values as current or extend the trend", () => {
  const first = snapshot();
  const failed = snapshot(15, { collectors: ["cpu", "memory", "disk"].map((collectorId) => ({ collectorId, ok: false, collectedAt: snapshot(15).collectedAt })) });
  const samples = appendResourceSample(appendResourceSample([], first), failed);
  const html = renderToStaticMarkup(<ResourceTrends snapshot={failed} samples={samples} />);
  assert.equal((html.match(/<strong aria-label="当前[^\"]+读数不可用">—<\/strong>/g) ?? []).length, 3);
  assert.ok(!html.includes('class="overview-trend-line"'));
  assert.equal((html.match(/class="overview-trend-point"/g) ?? []).length, 3);
  assert.equal(resourceTrendSegments(samples, "cpu").flat().length, 1);
  assert.ok(resourceTrendSegments(samples, "cpu")[0]![0]!.x < 236);
});
