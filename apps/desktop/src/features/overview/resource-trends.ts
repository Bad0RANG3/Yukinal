import type { ServerSnapshot } from "@yukinal/shared";

export const RESOURCE_TREND_SAMPLE_LIMIT = 60;
export const RESOURCE_TREND_SERVER_LIMIT = 12;
export const RESOURCE_TREND_WINDOW_MS = 15 * 60_000;
export const RESOURCE_TREND_GAP_MS = 45_000;

export type ResourceMetric = "cpu" | "memory" | "disk";
export interface ResourceValues { cpu?: number; memory?: number; disk?: number }
export interface ResourceSample extends ResourceValues {
  serverId: string;
  collectedAt: string;
  timestamp: number;
}

const EMPTY_SAMPLES: readonly ResourceSample[] = [];

function validPercent(value: number | undefined): number | undefined {
  return value !== undefined && Number.isFinite(value) && value >= 0 && value <= 100 ? value : undefined;
}

/** Failed collectors may leave old values in a snapshot; those are not readings. */
export function resourceValues(snapshot: ServerSnapshot): ResourceValues {
  const available = (id: string) => !snapshot.collectors?.some((sample) =>
    sample.collectorId.replace(/^collector\./, "") === id && !sample.ok,
  );
  const disks = available("disk") ? snapshot.disks : undefined;
  const diskValues = disks?.map((disk) => validPercent(disk.usagePercent));
  return {
    cpu: available("cpu") ? validPercent(snapshot.cpu?.usagePercent) : undefined,
    memory: available("memory") ? validPercent(snapshot.memory?.usagePercent) : undefined,
    disk: diskValues?.length && diskValues.every((value) => value !== undefined)
      ? Math.max(...diskValues) : undefined,
  };
}

/** Only actual snapshot timestamps enter the window; rerenders never add points. */
export function appendResourceSample(previous: readonly ResourceSample[], snapshot: ServerSnapshot): readonly ResourceSample[] {
  const samples = previous.every((sample) => sample.serverId === snapshot.serverId)
    ? previous : previous.filter((sample) => sample.serverId === snapshot.serverId);
  const timestamp = Date.parse(snapshot.collectedAt);
  if (!Number.isFinite(timestamp) || samples.some((sample) => sample.timestamp === timestamp)) return samples;
  const next = [...samples, {
    serverId: snapshot.serverId, collectedAt: snapshot.collectedAt, timestamp, ...resourceValues(snapshot),
  }].sort((left, right) => left.timestamp - right.timestamp);
  const cutoff = next[next.length - 1]!.timestamp - RESOURCE_TREND_WINDOW_MS;
  return next.filter((sample) => sample.timestamp >= cutoff).slice(-RESOURCE_TREND_SAMPLE_LIMIT);
}

/** Window-local observations, isolated per server and bounded across server switches. */
export class ResourceTrendStore {
  private windows = new Map<string, readonly ResourceSample[]>();

  read(serverId: string): readonly ResourceSample[] {
    return this.windows.get(serverId) ?? EMPTY_SAMPLES;
  }

  record(snapshot: ServerSnapshot): readonly ResourceSample[] {
    const samples = appendResourceSample(this.read(snapshot.serverId), snapshot);
    if (!samples.length) return samples;
    this.windows.delete(snapshot.serverId);
    this.windows.set(snapshot.serverId, samples);
    while (this.windows.size > RESOURCE_TREND_SERVER_LIMIT) {
      this.windows.delete(this.windows.keys().next().value!);
    }
    return samples;
  }
}

export interface ResourceTrendPoint { x: number; y: number; value: number; collectedAt: string }

/** Fixed 0–100% y scale; missing readings and collection outages break the line. */
export function resourceTrendSegments(samples: readonly ResourceSample[], metric: ResourceMetric): ResourceTrendPoint[][] {
  if (!samples.length) return [];
  const first = samples[0]!.timestamp;
  const last = samples[samples.length - 1]!.timestamp;
  const segments: ResourceTrendPoint[][] = [];
  let segment: ResourceTrendPoint[] = [];
  let previousTime: number | undefined;
  for (const sample of samples) {
    const value = validPercent(sample[metric]);
    if (value === undefined) {
      segment = [];
      previousTime = undefined;
      continue;
    }
    if (!segment.length || (previousTime !== undefined && sample.timestamp - previousTime > RESOURCE_TREND_GAP_MS)) {
      segment = [];
      segments.push(segment);
    }
    segment.push({
      x: last === first ? 236 : 4 + ((sample.timestamp - first) / (last - first)) * 232,
      y: 50 - (value / 100) * 46,
      value,
      collectedAt: sample.collectedAt,
    });
    previousTime = sample.timestamp;
  }
  return segments;
}
