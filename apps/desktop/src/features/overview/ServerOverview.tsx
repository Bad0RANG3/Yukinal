/**
 * Server Overview turns collector samples into a small set of decisions. The
 * page never invents values: unavailable data stays visibly unavailable and
 * collection failures remain actionable errors.
 */

import { useQuery } from "@tanstack/react-query";
import {
  HEALTH_THRESHOLDS,
  IPC_COMMANDS,
  type HealthClass,
  type Server,
  type ServerSnapshot,
} from "@yukinal/shared";

import { EnvBadge } from "../../components/EnvBadge.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";

const HEALTH_DOT: Record<HealthClass | "unknown", string> = {
  healthy: "health-dot-healthy",
  warning: "health-dot-warning",
  critical: "health-dot-critical",
  unknown: "health-dot-unknown",
};

const HEALTH_LABEL: Record<HealthClass | "unknown", string> = {
  healthy: "健康",
  warning: "需要关注",
  critical: "严重",
  unknown: "未知",
};

function gaugeClass(usage: number | undefined, thresholds: { warning: number; critical: number }): HealthClass | "unknown" {
  if (usage === undefined) return "unknown";
  if (usage >= thresholds.critical) return "critical";
  if (usage >= thresholds.warning) return "warning";
  return "healthy";
}

function MetricCard({
  label,
  usage,
  thresholds,
  hint,
}: {
  label: string;
  usage: number | undefined;
  thresholds: { warning: number; critical: number };
  hint: string;
}) {
  const status = gaugeClass(usage, thresholds);
  const width = usage === undefined ? 0 : Math.min(100, Math.max(0, Math.round(usage)));
  return (
    <div className="metric-card">
      <div className="metric-card-topline">
        <span>{label}</span>
        <span className={`metric-status metric-status-${status}`}>{HEALTH_LABEL[status]}</span>
      </div>
      <strong>{usage === undefined ? "—" : `${Math.round(usage)}%`}</strong>
      <div className="metric-track" aria-hidden="true">
        <div className={`metric-fill metric-fill-${status}`} style={{ width: `${width}%` }} />
      </div>
      <span className="metric-hint">{hint}</span>
    </div>
  );
}

function formatUptime(seconds?: number): string {
  if (seconds === undefined) return "—";
  const days = Math.floor(seconds / 86_400);
  if (days >= 1) return `${days} 天`;
  const hours = Math.floor((seconds % 86_400) / 3_600);
  if (hours >= 1) return `${hours} 小时`;
  return `${Math.max(1, Math.floor(seconds / 60))} 分钟`;
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
  const shell = isDesktopShell();
  const serversQuery = useQuery({
    queryKey: ["servers"],
    enabled: shell,
    staleTime: 10_000,
    queryFn: async () => (await callDesktop(IPC_COMMANDS.serverList, {})).servers,
  });
  const server = serversQuery.data?.find((candidate: Server) => candidate.id === selectedServerId);

  const snapshotQuery = useQuery({
    queryKey: ["serverSnapshot", selectedServerId],
    queryFn: async () => {
      if (!selectedServerId) return null;
      return (await callDesktop(IPC_COMMANDS.serverSnapshot, { serverId: selectedServerId })).snapshot;
    },
    refetchInterval: 15_000,
    enabled: Boolean(selectedServerId) && shell,
  });

  if (!shell) return <PreviewEmpty />;

  if (!selectedServerId || !server) {
    return (
      <div className="empty-state page-empty">
        <span className="empty-state-mark">▦</span>
        <h2>选择一台服务器</h2>
        <p>从左侧列表选择目标环境，查看实时健康状态与运行中的容器。</p>
      </div>
    );
  }

  if (snapshotQuery.isLoading) {
    return (
      <div className="loading-panel">
        <div className="loading-spinner" />
        <strong>正在连接并采集</strong>
        <span>SSH · 7 个采集器 · 预计几秒完成</span>
      </div>
    );
  }

  if (snapshotQuery.isError || !snapshotQuery.data) {
    return (
      <div className="error-panel">
        <div className="error-panel-icon">!</div>
        <div>
          <strong>无法读取服务器状态</strong>
          <p>{snapshotQuery.error instanceof Error ? snapshotQuery.error.message : String(snapshotQuery.error)}</p>
          <button type="button" className="secondary-button" onClick={() => void snapshotQuery.refetch()}>
            重试采集
          </button>
        </div>
      </div>
    );
  }

  return <OverviewContent server={server} snapshot={snapshotQuery.data} refreshing={snapshotQuery.isFetching} onRefresh={() => void snapshotQuery.refetch()} />;
}

function OverviewContent({
  server,
  snapshot,
  refreshing,
  onRefresh,
}: {
  server: Server;
  snapshot: ServerSnapshot;
  refreshing: boolean;
  onRefresh: () => void;
}) {
  const health = snapshot.health;
  const cpu = snapshot.cpu;
  const memory = snapshot.memory;
  const disks = snapshot.disks ?? [];
  const docker = snapshot.docker;
  const diskUsage = disks.length > 0 ? Math.max(...disks.map((disk) => disk.usagePercent)) : undefined;

  return (
    <div className="overview-page">
      <section className="overview-hero">
        <div>
          <div className="identity-line">
            <EnvBadge environment={server.metadata.environment} serverName={server.name} region={server.metadata.region} />
            <span className={`health-label health-label-${health}`}>
              <span className={`health-dot ${HEALTH_DOT[health]}`} />
              {HEALTH_LABEL[health]}
            </span>
          </div>
          <h2>{server.name}</h2>
          <p className="overview-subtitle">
            {snapshot.os ? `${snapshot.os.distribution} ${snapshot.os.version}` : "操作系统未知"} · {server.connection.username}@{server.connection.host}
          </p>
        </div>
        <button type="button" className="secondary-button refresh-button" onClick={onRefresh} disabled={refreshing}>
          <span aria-hidden="true">↻</span> {refreshing ? "采集中" : "刷新状态"}
        </button>
      </section>

      <dl className="server-facts">
        <div><dt>区域</dt><dd>{server.metadata.region ?? "未设置"}</dd></div>
        <div><dt>计算</dt><dd>{cpu?.cores === undefined ? "—" : `${cpu.cores} 核 CPU`}</dd></div>
        <div><dt>内存</dt><dd>{formatBytes(memory?.totalBytes)}</dd></div>
        <div><dt>运行时间</dt><dd>{formatUptime(snapshot.uptimeSeconds)}</dd></div>
      </dl>

      <section className="section-block">
        <div className="section-heading">
          <div><p className="eyebrow">资源概况</p><h3>系统健康</h3></div>
          <span className="section-note">每 15 秒更新</span>
        </div>
        <div className="metric-grid">
          <MetricCard label="CPU" usage={cpu?.usagePercent} thresholds={HEALTH_THRESHOLDS.cpu} hint={`${cpu?.cores ?? "—"} 核处理器`} />
          <MetricCard label="内存" usage={memory?.usagePercent} thresholds={HEALTH_THRESHOLDS.memory} hint={memory ? `${formatBytes(memory.usedBytes)} 已使用` : "采集器未返回数据"} />
          <MetricCard label="磁盘" usage={diskUsage} thresholds={HEALTH_THRESHOLDS.disk} hint={disks.length ? `${disks.length} 个挂载点` : "未发现磁盘数据"} />
        </div>
      </section>

      {health !== "healthy" ? (
        <section className={`attention-panel attention-${health}`}>
          <span className="attention-icon">!</span>
          <div><strong>{health === "critical" ? "需要立即关注" : "有资源接近阈值"}</strong><p>CPU、内存或磁盘至少一项已达到 {health === "critical" ? "严重" : "警告"} 阈值。可以让 Agent 进一步检查原因。</p></div>
        </section>
      ) : null}

      <section className="split-sections">
        <div className="section-block section-block-flex">
          <div className="section-heading"><div><p className="eyebrow">容器运行时</p><h3>Docker</h3></div><span className="section-note">{docker?.available ? `${docker.containers.length} 个容器` : "不可用"}</span></div>
          {!docker ? <p className="muted-copy">尚未返回 Docker 采集结果。</p> : !docker.available ? <p className="muted-copy">目标服务器未安装或未启用 Docker。</p> : docker.containers.length === 0 ? <p className="muted-copy">当前没有运行中的容器。</p> : (
            <ul className="container-list">
              {docker.containers.slice(0, 8).map((container, index) => (
                <li key={`${container.name}-${index}`}><span className={`container-dot ${container.state === "running" ? "container-dot-running" : ""}`} /><span className="container-name">{container.name}</span><span className="container-meta">{container.image} · {container.status}</span></li>
              ))}
            </ul>
          )}
        </div>
        <div className="section-block section-block-flex">
          <div className="section-heading"><div><p className="eyebrow">审计流</p><h3>最近动态</h3></div><span className="section-note">暂无数据</span></div>
          <div className="activity-empty"><span aria-hidden="true">◷</span><p>活动历史接入后，这里会显示部署、登录与服务变更。</p></div>
        </div>
      </section>
    </div>
  );
}

function PreviewEmpty() {
  return <div className="empty-state page-empty"><span className="empty-state-mark">◌</span><h2>浏览器预览</h2><p>原生连接能力只在 Tauri 桌面壳中可用，预览不会伪造服务器数据。</p></div>;
}
