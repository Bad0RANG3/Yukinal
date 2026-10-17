import type { ServerSnapshot } from "@yukinal/shared";

/** Failed collectors and snapshots from a different target must never become live metrics. */
export function dashboardResources(serverId: string, snapshot?: ServerSnapshot) {
  const matching = snapshot?.serverId === serverId ? snapshot : undefined;
  const usable = (id: string) => matching && !matching.collectors?.some((sample) =>
    sample.collectorId.replace(/^.*[.:/]/, "") === id && !sample.ok);
  return {
    snapshot: matching,
    cpu: usable("cpu") ? matching?.cpu?.usagePercent : undefined,
    memory: usable("memory") ? matching?.memory?.usagePercent : undefined,
    disk: usable("disk") && matching?.disks?.length ? Math.max(...matching.disks.map((disk) => disk.usagePercent)) : undefined,
    containers: usable("docker") && matching?.docker?.available ? matching.docker.containers : undefined,
  };
}
