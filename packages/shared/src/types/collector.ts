/**
 * Collector output + derived health.
 *
 * Dashboard rule: raw numbers become `healthy | warning | critical | information`.
 * The translation happens once, here, so UI and Agent never disagree about it.
 */

import type { ServerCapabilities } from "./server.js";

export const HEALTH_STATES = ["healthy", "warning", "critical", "unknown"] as const;
export type HealthState = (typeof HEALTH_STATES)[number];

export interface DiskUsage {
  device: string;
  mountPoint: string;
  totalBytes: number;
  usedBytes: number;
  usagePercent: number;
}

export interface NetworkInterfaceSample {
  name: string;
  rxBytes: number;
  txBytes: number;
}

export interface ContainerInfo {
  name: string;
  image: string;
  /** running | restarting | exited | paused | ... */
  state: string;
  status: string;
  restartCount: number;
}

export interface CollectorSample {
  collectorId: string;
  collectedAt: string;
  ok: boolean;
  error?: string;
}

/** One row of the `snapshots` table. */
export interface ServerSnapshot {
  id: string;
  serverId: string;
  collectedAt: string;
  health: HealthState;
  os?: { distribution: string; version: string; hostname: string; kernel: string; arch: string };
  cpu?: { model: string; cores: number; usagePercent: number; loadAverage: [number, number, number] };
  memory?: { totalBytes: number; usedBytes: number; availableBytes: number; usagePercent: number };
  disks?: DiskUsage[];
  uptimeSeconds?: number;
  network?: NetworkInterfaceSample[];
  docker?: { available: boolean; containers: ContainerInfo[] };
  capabilities: ServerCapabilities;
  /** Per-collector health so a failing collector degrades one card, not the page. */
  collectors?: CollectorSample[];
}

/** "Disk usage increased 12% this week" (Attention section). */
export interface AttentionItem {
  id: string;
  serverId: string;
  state: Exclude<HealthState, "unknown">;
  /** One sentence, user-facing language, no command output (). */
  message: string;
  detectedAt: string;
  /** What the user can do next — seeds the Agent's suggested plan. */
  suggestedAction?: string;
}
