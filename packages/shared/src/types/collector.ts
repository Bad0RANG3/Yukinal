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

/** Configuration observed on the host, never a claim of Internet reachability. */
export interface NetworkInterfaceInfo {
  name: string;
  state: string;
  addresses: string[];
}

export interface NetworkRoute {
  destination: string;
  gateway?: string;
  interface?: string;
  metric?: number;
  type?: string;
}

export interface NetworkDetails {
  interfaces?: NetworkInterfaceInfo[];
  ipv4Routes?: NetworkRoute[];
  ipv6Routes?: NetworkRoute[];
  dnsServers?: string[];
  /** A missing section is unreported; an empty array is a successful empty result. */
  errors?: Partial<Record<"interfaces" | "ipv4Routes" | "ipv6Routes" | "dnsServers", string>>;
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

export interface GpuSample {
  status: "available" | "unavailable" | "error";
  message?: string;
  devices: Array<{
    index: number;
    name: string;
    usagePercent?: number;
    memoryUsedBytes?: number;
    memoryTotalBytes?: number;
    temperatureCelsius?: number;
  }>;
}

export interface SecuritySample {
  checks: Array<{
    id: string;
    label: string;
    status: "info" | "warning" | "unknown";
    detail: string;
  }>;
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
  networkDetails?: NetworkDetails;
  gpu?: GpuSample;
  security?: SecuritySample;
  toolkit?: { version: string; modules: string[] };
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
