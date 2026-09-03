/**
 * Server Overview: raw collector data → health classes (thresholds live in
 * `@yukinal/shared`, the same file the Rust core compiles in).
 *
 * Everything rendered here comes from a real `server_snapshot` run: if the host is
 * unreachable / auth failed / collectors failed, the user gets the error instead of
 * a made-up dashboard.
 */

import { useQuery } from "@tanstack/react-query";
import {
  HEALTH_THRESHOLDS,
  IPC_COMMANDS,
  type HealthClass,
  type Server,
  type ServerSnapshot,
} from "@yukinal/shared";

import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { EnvBadge } from "../../components/EnvBadge.js";

const HEALTH_DOT: Record<HealthClass | "unknown", string> = {
  healthy: "bg-emerald-500",
  warning: "bg-amber-400",
  critical: "bg-rose-500",
  unknown: "bg-zinc-600",
};

const HEALTH_LABEL: Record<HealthClass | "unknown", string> = {
  healthy: "Healthy",
  warning: "Warning",
  critical: "Critical",
  unknown: "Unknown",
};

function gaugeClass(
  usage: number | undefined,
  thresholds: { warning: number; critical: number },
): HealthClass | "unknown" {
  if (usage === undefined) return "unknown";
  if (usage >= thresholds.critical) return "critical";
  if (usage >= thresholds.warning) return "warning";
  return "healthy";
}

function Gauge({ label, usage }: { label: string; usage: number | undefined }) {
  const thresholds =
    label === "CPU" ? HEALTH_THRESHOLDS.cpu : label === "Memory" ? HEALTH_THRESHOLDS.memory : HEALTH_THRESHOLDS.disk;
  const klass = gaugeClass(usage, thresholds);
  const color =
    klass === "critical" ? "bg-rose-500" : klass === "warning" ? "bg-amber-400" : "bg-emerald-500";
  const width = usage === undefined ? 0 : Math.min(100, Math.max(0, Math.round(usage)));
  return (
    <div className="min-w-32 flex-1">
      <div className="mb-1 flex items-baseline justify-between text-xs">
        <span className="text-zinc-400">{label}</span>
        <span className="font-mono">{usage === undefined ? "—" : `${Math.round(usage)}%`}</span>
      </div>
      <div className="h-1.5 w-full overflow-hidden rounded bg-zinc-800">
        <div className={`h-full ${color}`} style={{ width: `${width}%` }} />
      </div>
    </div>
  );
}

function formatUptime(seconds?: number): string {
  if (seconds === undefined) return "—";
  const days = Math.floor(seconds / 86_400);
  if (days >= 1) return `${days}d`;
  const hours = Math.floor((seconds % 86_400) / 3_600);
  if (hours >= 1) return `${hours}h`;
  return `${Math.floor(seconds / 60)}m`;
}

function formatBytes(bytes?: number): string {
  if (bytes === undefined) return "—";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value >= 10 ? Math.round(value) : value.toFixed(1)} ${units[unit]}`;
}

export function ServerOverview() {
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const serversQuery = useQuery({
    queryKey: ["servers"],
    queryFn: async () =>
      isDesktopShell() ? (await callDesktop(IPC_COMMANDS.serverList, {})).servers : [],
  });
  const server = serversQuery.data?.find((candidate: Server) => candidate.id === selectedServerId);

  const snapshotQuery = useQuery({
    queryKey: ["serverSnapshot", selectedServerId],
    queryFn: async () => {
      if (!selectedServerId) return null;
      return (await callDesktop(IPC_COMMANDS.serverSnapshot, { serverId: selectedServerId })).snapshot;
    },
    refetchInterval: 15_000,
    enabled: Boolean(selectedServerId) && isDesktopShell(),
  });

  if (!selectedServerId || !server) {
    return <p className="text-zinc-400">从左侧选择一台服务器查看概览。</p>;
  }

  if (snapshotQuery.isLoading) {
    return (
      <div className="flex h-full min-h-[240px] items-center justify-center text-sm text-zinc-500">
        正在连接并采集（SSH + 7 个采集器）…
      </div>
    );
  }

  if (snapshotQuery.isError || !snapshotQuery.data) {
    return (
      <div className="flex h-full min-h-[240px] items-center justify-center text-sm text-rose-400">
        采集失败：{snapshotQuery.error instanceof Error ? snapshotQuery.error.message : String(snapshotQuery.error)}
      </div>
    );
  }

  const snapshot: ServerSnapshot = snapshotQuery.data;
  const health = snapshot.health;
  const cpu = snapshot.cpu;
  const memory = snapshot.memory;
  const disks = snapshot.disks ?? [];
  const docker = snapshot.docker;

  return (
    <div className="space-y-5">
      {/* 标题行：名称 + 环境徽标 + 状态点 + 区域（production/api/singapore 常显） */}
      <div className="flex items-center gap-3">
        <h2 className="text-lg font-semibold">{server.name}</h2>
        <EnvBadge environment={server.metadata.environment} serverName={server.name} region={server.metadata.region} />
        <span className="flex items-center gap-1.5 text-sm">
          <span className={`size-2 rounded-full ${HEALTH_DOT[health]}`} />
          <span className={health === "critical" ? "text-rose-400" : health === "warning" ? "text-amber-400" : "text-zinc-300"}>
            {HEALTH_LABEL[health]}
          </span>
        </span>
        {server.metadata.region ? (
          <span className="text-xs text-zinc-500">{server.metadata.region}</span>
        ) : null}
      </div>

      {/* meta 行 */}
      <p className="text-xs text-zinc-400">
        {snapshot.os ? `${snapshot.os.distribution} ${snapshot.os.version}` : "—"} ·{" "}
        {server.metadata.region ?? "unknown region"} · {cpu?.cores ?? "—"} CPU ·{" "}
        {memory ? formatBytes(memory.totalBytes) : "—"} RAM · Uptime {formatUptime(snapshot.uptimeSeconds)}
      </p>

      {/* gauges */}
      <div className="flex flex-wrap gap-4 rounded-lg border border-zinc-800 p-4">
        <Gauge label="CPU" usage={cpu?.usagePercent} />
        <Gauge label="Memory" usage={memory?.usagePercent} />
        <Gauge label="Disk" usage={disks.length > 0 ? Math.max(...disks.map((d) => d.usagePercent)) : undefined} />
      </div>

      {/* docker */}
      {docker && docker.available ? (
        <div className="rounded-lg border border-zinc-800 p-4">
          <div className="mb-2 text-xs uppercase tracking-wide text-zinc-500">Docker</div>
          {docker.containers.length === 0 ? (
            <p className="text-sm text-zinc-500">no containers</p>
          ) : (
            <ul className="space-y-1 text-sm">
              {docker.containers.slice(0, 12).map((container, index) => (
                <li key={`${container.name}-${index}`} className="flex items-center gap-2">
                  <span
                    className={`size-1.5 rounded-full ${container.state === "running" ? "bg-emerald-500" : "bg-zinc-600"}`}
                  />
                  <span className="font-mono">{container.name}</span>
                  <span className="text-xs text-zinc-500">
                    {container.image} · {container.status}
                  </span>
                </li>
              ))}
            </ul>
          )}
        </div>
      ) : docker ? (
        <div className="rounded-lg border border-zinc-800 p-4 text-sm text-zinc-500">Docker 不可用</div>
      ) : null}

      {/* attention：由当前健康级驱动（趋势版在快照历史接入后补） */}
      {health !== "healthy" ? (
        <div className="rounded-lg border border-amber-500/30 bg-amber-500/5 p-4">
          <div className="mb-1 text-xs font-medium uppercase tracking-wide text-amber-400">Attention</div>
          {health === "critical" ? (
            <p className="text-sm">CPU / 内存 / 磁盘至少一项处于 critical（{HEALTH_LABEL[health]}）</p>
          ) : (
            <p className="text-sm">CPU / 内存 / 磁盘至少一项达到 warning 阈值</p>
          )}
        </div>
      ) : null}

      {/* recent activity：活动流在后续步骤接入，先给诚实的空态 */}
      <div className="rounded-lg border border-zinc-800 p-4">
        <div className="mb-2 text-xs uppercase tracking-wide text-zinc-500">Recent Activity</div>
        <p className="text-sm text-zinc-600">活动流接入前暂无数据。</p>
      </div>
    </div>
  );
}