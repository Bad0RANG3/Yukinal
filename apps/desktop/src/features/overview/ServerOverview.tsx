/**
 * Server Overview turns collector samples into a small set of decisions. The
 * page never invents values: unavailable data stays visibly unavailable and
 * collection failures remain actionable errors.
 */

import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  HEALTH_THRESHOLDS,
  IPC_COMMANDS,
  healthClass,
  type Activity,
  type HealthClass,
  type CollectorSample,
  type Server,
  type ServerSnapshot,
} from "@yukinal/shared";

import { errorMessage, formatBytes } from "../../lib/format.js";
import { EnvBadge } from "../../components/EnvBadge.js";
import { EmptyPanel, ErrorPanel, LoadingPanel } from "../../components/PanelStates.js";
import { Icon } from "../../components/Icon.js";
import { callDesktop, isDesktopShell, subscribeDesktop } from "../../lib/ipc.js";
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

const SNAPSHOT_STALE_AFTER_MS = 45_000;

const COLLECTORS = [
  { id: "os", label: "系统信息" },
  { id: "cpu", label: "CPU" },
  { id: "memory", label: "内存" },
  { id: "disk", label: "磁盘" },
  { id: "uptime", label: "运行时间" },
  { id: "network", label: "网络" },
  { id: "docker", label: "Docker" },
] as const;

function normalizeCollectorId(collectorId: string): string {
  return collectorId.startsWith("collector.") ? collectorId.slice("collector.".length) : collectorId;
}

function sampleFor(snapshot: ServerSnapshot, collectorId: string): CollectorSample | undefined {
  return snapshot.collectors?.find((sample) => normalizeCollectorId(sample.collectorId) === collectorId);
}

function formatTimestamp(timestamp: string): string {
  const value = new Date(timestamp);
  if (Number.isNaN(value.getTime())) return "时间格式无效";
  return new Intl.DateTimeFormat("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(value);
}

function formatAge(timestamp: string, now: number): { kind: "fresh" | "stale" | "unknown"; label: string } {
  const collectedAt = Date.parse(timestamp);
  if (!Number.isFinite(collectedAt)) return { kind: "unknown", label: "采集时间无效" };
  const age = now - collectedAt;
  if (age < 0) return { kind: "unknown", label: "采集时间晚于本机时钟" };
  if (age > SNAPSHOT_STALE_AFTER_MS) {
    const minutes = Math.floor(age / 60_000);
    const hours = Math.floor(age / 3_600_000);
    const days = Math.floor(age / 86_400_000);
    const label = days > 0 ? days + " 天前" : hours > 0 ? hours + " 小时前" : minutes > 0 ? minutes + " 分钟前" : "超过 45 秒";
    return { kind: "stale", label };
  }
  const seconds = Math.floor(age / 1_000);
  return { kind: "fresh", label: seconds < 10 ? "刚刚" : seconds + " 秒前" };
}

function collectorSummary(snapshot: ServerSnapshot) {
  const samples = snapshot.collectors;
  const sampleMap = new Map((samples ?? []).map((sample) => [normalizeCollectorId(sample.collectorId), sample]));
  const missing = COLLECTORS.filter((collector) => !sampleMap.has(collector.id));
  const failed = COLLECTORS.filter((collector) => sampleMap.get(collector.id)?.ok === false);
  const reported = COLLECTORS.length - missing.length;
  const kind = samples === undefined || samples.length === 0
    ? "unknown"
    : failed.length === COLLECTORS.length
      ? "failed"
      : failed.length > 0
        ? "partial"
        : missing.length > 0
          ? "incomplete"
          : "complete";
  const label = kind === "failed"
    ? "所有采集器均失败"
    : kind === "partial"
      ? "部分采集失败"
      : kind === "incomplete"
        ? "采集状态不完整"
        : kind === "unknown"
          ? "采集状态未知"
          : "采集完成";
  const description = samples === undefined || samples.length === 0
    ? "此快照没有提供采集器状态明细。"
    : reported + "/" + COLLECTORS.length + " 个采集器报告状态" +
      (failed.length > 0 ? " · " + failed.length + " 个失败" : "") +
      (missing.length > 0 ? " · " + missing.length + " 个未报告" : "");
  return { kind, label, description, sampleMap };
}

/**
 * 未取到读数时是 `unknown`，有读数时一律交给 `healthClass`。
 *
 * 这里曾经自己重写了一遍阈值比较。那正是 `types/health.ts:8-9` 明令禁止的事：
 * 「原始数字变成健康等级的换算全项目只有一处，就是这里，外加 Rust 镜像
 * （crates/core/src/health.rs）—— UI 和 Agent 不允许各算各的」。
 * 更糟的是本文件已经 `import { HEALTH_THRESHOLDS }` 了：阈值取自共享模块，
 * 比较逻辑却是本地副本，于是「共享的那份对了、这份没跟上」不会有任何编译或
 * 测试报错。现在只保留 `undefined` 这一层判断，那确实是 UI 关心的事
 * （`HealthClass` 里没有 `unknown`，它属于 `HealthState`，见 types/health.ts:18）。
 */
function gaugeClass(usage: number | undefined, thresholds: { warning: number; critical: number }): HealthClass | "unknown" {
  return usage === undefined ? "unknown" : healthClass(usage, thresholds);
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
  const snapshot = snapshotQuery.data?.serverId === server?.id ? snapshotQuery.data : undefined;
  const snapshotServerMismatch = Boolean(snapshotQuery.data && !snapshot);

  if (!shell) return <WelcomePanel />;

  if (!selectedServerId || !server) {
    return <EmptyPanel icon="servers" title="选择一台服务器" body="从左侧列表选择目标环境，查看实时健康状态与运行中的容器。" />;
  }

  if (server.status !== "connected" && !snapshot) {
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
    return <LoadingPanel title="正在连接并采集" hint="SSH · 7 个采集器 · 预计几秒完成" />;
  }

  if (!snapshot) {
    return (
      <ErrorPanel
        showIcon
        title="无法读取服务器状态"
        message={snapshotServerMismatch ? "收到的快照属于另一台服务器，已拒绝显示。请重试采集。" : errorMessage(snapshotQuery.error)}
        onRetry={() => void snapshotQuery.refetch()}
        retryLabel="重试采集"
      />
    );
  }

  return <>
    {snapshotQuery.isError ? <p className="settings-notice" role="alert">最近一次刷新失败，当前仍显示上次成功采集的快照。{errorMessage(snapshotQuery.error)}</p> : null}
    <OverviewContent
      server={server}
      snapshot={snapshot}
      refreshing={snapshotQuery.isFetching}
      onRefresh={() => void snapshotQuery.refetch()}
      onConnect={() => action.mutate({ type: "connect", serverId: server.id })}
      connecting={busy || server.status === "connecting"}
      connectionError={action.isError ? action.error.message : undefined}
    />
  </>;
}

function OverviewContent({
  server,
  snapshot,
  refreshing,
  onRefresh,
  onConnect,
  connecting,
  connectionError,
}: {
  server: Server;
  snapshot: ServerSnapshot;
  refreshing: boolean;
  onRefresh: () => void;
  onConnect: () => void;
  connecting: boolean;
  connectionError?: string;
}) {
  const [now, setNow] = useState(() => Date.now());
  const setServerPage = useWorkspaceStore((state) => state.setServerPage);
  const setPrimary = useWorkspaceStore((state) => state.setPrimary);
  useEffect(() => {
    const interval = window.setInterval(() => setNow(Date.now()), 10_000);
    return () => window.clearInterval(interval);
  }, []);

  const health = snapshot.health;
  const osSample = sampleFor(snapshot, "os");
  const os = osSample?.ok === false ? undefined : snapshot.os;
  const cpuSample = sampleFor(snapshot, "cpu");
  const memorySample = sampleFor(snapshot, "memory");
  const diskSample = sampleFor(snapshot, "disk");
  const cpu = cpuSample?.ok === false ? undefined : snapshot.cpu;
  const memory = memorySample?.ok === false ? undefined : snapshot.memory;
  const disks = diskSample?.ok === false ? [] : snapshot.disks ?? [];
  const uptime = sampleFor(snapshot, "uptime")?.ok === false ? undefined : snapshot.uptimeSeconds;
  const docker = sampleFor(snapshot, "docker")?.ok === false ? undefined : snapshot.docker;
  const diskUsage = disks.length > 0 ? Math.max(...disks.map((disk) => disk.usagePercent)) : undefined;
  const collection = collectorSummary(snapshot);
  const age = formatAge(snapshot.collectedAt, now);
  const connected = server.status === "connected";
  const freshnessKind = connected ? age.kind : "offline";
  const freshnessLabel = connected
    ? age.kind === "stale" ? "快照已过期" : age.kind === "unknown" ? "新鲜度未知" : "采集较新"
    : "缓存快照";
  const connectionLabel = server.status === "connected"
    ? "SSH 已连接"
    : server.status === "connecting"
      ? "正在连接"
      : server.status === "error"
        ? "连接异常"
        : "未连接";

  return (
    <div className="overview-page">
      {!connected ? (
        <section className={"overview-connection-notice overview-connection-" + server.status} role={server.status === "error" ? "alert" : "status"}>
          <div>
            <strong>{server.status === "connecting" ? "正在连接服务器" : server.status === "error" ? "服务器连接异常" : "服务器当前未连接"}</strong>
            <p>{server.status === "connecting" ? "下方保留最近一次快照；连接完成后才会重新采集。" : "下方数据来自缓存快照，不能代表主机当前状态。重新连接后会开始采集。"}</p>
            {connectionError ? <p className="overview-connection-error">{connectionError}</p> : null}
          </div>
          <button type="button" className="button-secondary" onClick={onConnect} disabled={connecting}>
            <Icon name="connect" size="sm" />{connecting ? "正在连接…" : "重新连接"}
          </button>
        </section>
      ) : null}

      <section className="overview-hero">
        <div>
          <div className="identity-line">
            <EnvBadge environment={server.metadata.environment} serverName={server.name} region={server.metadata.region} />
            <span className={"overview-connection-label overview-connection-label-" + server.status}>{connectionLabel}</span>
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
            {os ? os.distribution + " " + os.version : "操作系统未知"} · {server.connection.username}@{server.connection.host}
          </p>
        </div>
        <button type="button" className="button-secondary refresh-button" onClick={onRefresh} disabled={!connected || refreshing} title={connected ? "刷新服务器状态" : "重新连接后才能刷新"} aria-label={connected ? "刷新服务器状态" : "重新连接后才能刷新"}>
          <Icon name="refresh" size="sm" /> {refreshing ? "采集中" : connected ? "刷新状态" : "连接后刷新"}
        </button>
      </section>

      <section className={"overview-observation overview-observation-" + collection.kind} aria-label="状态数据来源和采集情况">
        <div className="overview-observation-heading">
          <span className={"overview-freshness overview-freshness-" + freshnessKind}><span aria-hidden="true" />{freshnessLabel}</span>
          <strong>{collection.label}</strong>
          <span>来源：SSH 远端采集器</span>
        </div>
        <p>
          最后采集：<time dateTime={snapshot.collectedAt} title={"本地时间 " + formatTimestamp(snapshot.collectedAt)}>{formatTimestamp(snapshot.collectedAt)}（{age.label}）</time>
          {!connected ? " · 当前显示缓存数据" : " · 每 15 秒自动采集"}
        </p>
        <p className="overview-collector-summary">{collection.description}</p>
        {refreshing ? <p className="overview-refreshing" role="status">正在采集新快照；页面仍显示最近一次已完成的结果。</p> : null}
        <details className="overview-collector-details">
          <summary>查看采集器明细</summary>
          <ul>
            {COLLECTORS.map((collector) => {
              const sample = collection.sampleMap.get(collector.id);
              const state = sample ? sample.ok ? "success" : "failure" : "unknown";
              const stateLabel = sample ? sample.ok ? "成功" : "失败" : "未报告";
              return (
                <li key={collector.id}>
                  <span>{collector.label}</span>
                  <span className={"overview-collector-state overview-collector-state-" + state}>{stateLabel}</span>
                  {sample ? <time dateTime={sample.collectedAt}>{formatTimestamp(sample.collectedAt)}</time> : <span>本次快照无状态记录</span>}
                </li>
              );
            })}
            {(snapshot.collectors ?? [])
              .filter((sample) => !COLLECTORS.some((collector) => collector.id === normalizeCollectorId(sample.collectorId)))
              .map((sample, index) => (
                <li key={sample.collectorId + "-" + index}>
                  <span>{sample.collectorId}</span>
                  <span className={"overview-collector-state overview-collector-state-" + (sample.ok ? "success" : "failure")}>{sample.ok ? "成功" : "失败"}</span>
                  <time dateTime={sample.collectedAt}>{formatTimestamp(sample.collectedAt)}</time>
                </li>
              ))}
          </ul>
        </details>
      </section>

      <nav className="overview-shortcuts" aria-label="服务器运维入口">
        <button type="button" className="button-primary" onClick={() => setPrimary("tasks")} aria-label={`让 AI 在 ${server.name} 上完成目标`}><Icon name="agent" size="sm" />让 AI 处理目标</button>
        <button type="button" className="button-secondary" onClick={() => setServerPage("services")}><Icon name="services" size="sm" />查看服务</button>
        <button type="button" className="button-secondary" onClick={() => setServerPage("logs")}><Icon name="logs" size="sm" />查看日志</button>
        <button type="button" className="button-secondary" onClick={() => setServerPage("files")}><Icon name="file" size="sm" />浏览文件</button>
        <button type="button" className="button-secondary" onClick={() => setServerPage("activity")}><Icon name="activity" size="sm" />活动记录</button>
      </nav>

      <dl className="server-facts">
        <div><dt>区域</dt><dd>{server.metadata.region ?? "未设置"}</dd></div>
        <div><dt>计算</dt><dd>{cpu?.cores === undefined ? "—" : `${cpu.cores} 核 CPU`}</dd></div>
        <div><dt>内存</dt><dd>{formatBytes(memory?.totalBytes)}</dd></div>
        <div><dt>运行时间</dt><dd>{formatUptime(uptime)}</dd></div>
      </dl>

      <section className="section-block">
        <div className="section-heading">
          <div><p className="eyebrow">资源概况</p><h2>系统健康</h2></div>
          <span className="section-note">{connected ? "每 15 秒自动采集" : "连接后恢复自动采集"}</span>
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
        <RecentServerActivity serverId={server.id} />
      </section>
    </div>
  );
}

function RecentServerActivity({ serverId }: { serverId: string }) {
  const queryClient = useQueryClient();
  const setServerPage = useWorkspaceStore((state) => state.setServerPage);
  const activities = useQuery({
    queryKey: ["activities", serverId],
    queryFn: async () => (await callDesktop(IPC_COMMANDS.activityList, { serverId, limit: 50 })).activities,
  });

  useEffect(() => {
    const subscription = subscribeDesktop("activity.created", (activity) => {
      if (activity.serverId !== serverId) return;
      queryClient.setQueryData<Activity[]>(["activities", serverId], (current) => {
        if (!current) return current;
        return [activity, ...current.filter((item) => item.id !== activity.id)].slice(0, 50);
      });
    });
    return subscription.stop;
  }, [queryClient, serverId]);

  const rows = activities.data?.slice(0, 3) ?? [];
  return (
    <div className="section-block section-block-flex">
      <div className="section-heading">
        <div><p className="eyebrow">审计流</p><h2>最近动态</h2></div>
        <button type="button" className="button-secondary" onClick={() => setServerPage("activity")}>全部活动</button>
      </div>
      {activities.isLoading ? <p className="muted-copy" role="status">正在读取服务器动态…</p> : null}
      {activities.isError ? <p className="error-copy" role="alert">无法读取服务器动态：{errorMessage(activities.error)}</p> : null}
      {!activities.isLoading && !activities.isError && rows.length === 0 ? <div className="activity-empty"><Icon name="activity" size="lg" /><p>暂无动态；连接、配置和 Agent 操作会记录在这里。</p></div> : null}
      {rows.length ? <ul className="overview-activity-list">{rows.map((activity) => (
        <li key={activity.id}>
          <strong>{activity.title}</strong>
          {activity.description || activity.reason ? <span>{activity.description ?? activity.reason}</span> : null}
          <time dateTime={activity.createdAt}>{formatTimestamp(activity.createdAt)}</time>
        </li>
      ))}</ul> : null}
    </div>
  );
}

/**
 * 浏览器预览时的欢迎页。
 *
 * 它原先叫 `PreviewEmpty` —— 与 `components/PanelStates.tsx` 里那个「浏览器预览
 * 占位」同名，但那两件事完全不同：那个是一句「这里没有数据」的占位，这个是
 * 一块带引导步骤和入口按钮的欢迎面板。名字撞上之后，读到 `PreviewEmpty` 的人
 * 得先确认是哪一个。改成它实际的样子。
 */
function WelcomePanel() {
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
