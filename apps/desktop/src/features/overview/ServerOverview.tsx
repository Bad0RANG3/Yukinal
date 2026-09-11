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
import { Icon } from "../../components/Icon.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { useServerAction, useServers } from "../../lib/servers.js";

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
  const serversQuery = useServers();
  const { action, busy } = useServerAction();
  const server = serversQuery.data?.find((candidate: Server) => candidate.id === selectedServerId);

  const snapshotQuery = useQuery({
    queryKey: ["serverSnapshot", selectedServerId],
    queryFn: async () => {
      if (!selectedServerId) return null;
      return (await callDesktop(IPC_COMMANDS.serverSnapshot, { serverId: selectedServerId })).snapshot;
    },
    refetchInterval: 15_000,
    enabled: Boolean(selectedServerId) && shell && server?.status === "connected" && !busy,
  });

  if (!shell) return <PreviewEmpty />;

  if (!selectedServerId || !server) {
    return (
      <div className="empty-state page-empty">
        <Icon name="servers" size="xl" />
        <h2>选择一台服务器</h2>
        <p>从左侧列表选择目标环境，查看实时健康状态与运行中的容器。</p>
      </div>
    );
  }

  if (server.status !== "connected") {
    return <div className="empty-state page-empty connection-empty">
      <span className="empty-icon"><Icon name="connect" size="xxl" /></span>
      <EnvBadge environment={server.metadata.environment} serverName={server.name} />
      <h2>{server.name}</h2>
      <p>{server.connection.username}@{server.connection.host}:{server.connection.port}</p>
      <small>建立 SSH 连接后，查看系统资源与服务状态。</small>
      <button type="button" className="button-primary" disabled={busy || server.status === "connecting"} onClick={() => action.mutate({ type: "connect", serverId: server.id })}><Icon name="connect" size="md" />{busy || server.status === "connecting" ? "正在连接…" : "连接服务器"}</button>
      {action.isError ? <p className="form-error" role="alert">{action.error.message}</p> : null}
    </div>;
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

  if (!snapshotQuery.data) {
    return (
      <div className="error-panel">
        <div className="error-panel-icon"><Icon name="warning" size="md" /></div>
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

  return <>
    {snapshotQuery.isError ? <p className="settings-notice" role="alert">最近一次刷新失败，以下保留上次采集结果。{snapshotQuery.error.message}</p> : null}
    <OverviewContent server={server} snapshot={snapshotQuery.data} refreshing={snapshotQuery.isFetching} onRefresh={() => void snapshotQuery.refetch()} />
  </>;
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
          {/* 这里原来是一个 <h2>{server.name}</h2>。名字在紧邻上方的
              .workspace-header 里已经是这一页的 <h1>，再标一次 h2 会让读屏用户
              连着听到两遍同一个名字、而且是两个不同层级（「Production API，
              一级标题」「Production API，二级标题」）。视觉上这个大字要保留，
              所以改成普通元素 + 同样的排版，不再参与标题层级 —— 页面的标题由
              h1 独占，下面的小节这才好落在 h2 上。 */}
          <p className="overview-title">{server.name}</p>
          <p className="overview-subtitle">
            {snapshot.os ? `${snapshot.os.distribution} ${snapshot.os.version}` : "操作系统未知"} · {server.connection.username}@{server.connection.host}
          </p>
        </div>
        <button type="button" className="secondary-button refresh-button" onClick={onRefresh} disabled={refreshing} title="刷新服务器状态" aria-label="刷新服务器状态">
          <Icon name="refresh" size="sm" /> {refreshing ? "采集中" : "刷新状态"}
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
          <div><p className="eyebrow">资源概况</p><h2>系统健康</h2></div>
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
          <span className="attention-icon"><Icon name="warning" size="md" /></span>
          <div><strong>{health === "critical" ? "需要立即关注" : "有资源接近阈值"}</strong><p>CPU、内存或磁盘至少一项已达到 {health === "critical" ? "严重" : "警告"} 阈值。可以让 Agent 进一步检查原因。</p></div>
        </section>
      ) : null}

      <section className="split-sections">
        <div className="section-block section-block-flex">
          <div className="section-heading"><div><p className="eyebrow">容器运行时</p><h2>Docker</h2></div><span className="section-note">{docker?.available ? `${docker.containers.length} 个容器` : "不可用"}</span></div>
          {!docker ? <p className="muted-copy">尚未返回 Docker 采集结果。</p> : !docker.available ? <p className="muted-copy">目标服务器未安装或未启用 Docker。</p> : docker.containers.length === 0 ? <p className="muted-copy">当前没有运行中的容器。</p> : (
            <ul className="container-list">
              {docker.containers.slice(0, 8).map((container, index) => (
                <li key={`${container.name}-${index}`}><span className={`container-dot ${container.state === "running" ? "container-dot-running" : ""}`} /><span className="container-name">{container.name}</span><span className="container-meta">{container.image} · {container.status}</span></li>
              ))}
            </ul>
          )}
        </div>
        <div className="section-block section-block-flex">
          <div className="section-heading"><div><p className="eyebrow">审计流</p><h2>最近动态</h2></div><span className="section-note">暂无数据</span></div>
          <div className="activity-empty"><Icon name="activity" size="lg" /><p>活动历史接入后，这里会显示部署、登录与服务变更。</p></div>
        </div>
      </section>
    </div>
  );
}

function PreviewEmpty() {
  const setPrimary = useWorkspaceStore((state) => state.setPrimary);
  return <div className="welcome-page">
    <div className="welcome-copy"><span className="welcome-kicker"><span className="health-dot health-dot-healthy" />远程工作，从这里开始</span><h2>连接环境。<br /><span>专注正在做的事。</span></h2><p>服务器、终端与 Agent，<br />在一个安静、有序的工作区里协作。</p></div>
    <div className="welcome-steps">
      <div><span className="welcome-step-number">01</span><Icon name="connect" size="lg" /><strong>连接服务器</strong><p>在桌面应用中添加 SSH 连接。</p></div>
      <div><span className="welcome-step-number">02</span><Icon name="activity" size="lg" /><strong>了解运行状态</strong><p>集中查看资源、日志和服务。</p></div>
      <div><span className="welcome-step-number">03</span><Icon name="agent" size="lg" /><strong>让 Agent 协助</strong><p>带着明确的目标，开始排查。</p></div>
    </div>
    <div className="welcome-footer"><span><Icon name="servers" size="md" />当前为界面预览，连接功能需在桌面应用中使用。</span><button type="button" className="button-secondary" onClick={() => setPrimary("settings")}>调整工作区 <Icon name="chevronRight" size="sm" /></button></div>
  </div>;
}
