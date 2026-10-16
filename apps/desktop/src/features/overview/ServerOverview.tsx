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
import { EmptyPanel, ErrorPanel, LoadingPanel } from "../../components/PanelStates.js";
import { Icon } from "../../components/Icon.js";
import { callDesktop, isDesktopShell, subscribeDesktop } from "../../lib/ipc.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { useServerAction, useServers } from "../../lib/servers.js";

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

export function snapshotAge(timestamp: string, displayClock: number, receivedAt: number): { kind: "fresh" | "stale" | "unknown"; label: string } {
  const collectedAt = Date.parse(timestamp);
  if (!Number.isFinite(collectedAt)) return { kind: "unknown", label: "采集时间无效" };
  // Polling and the 10-second display clock have separate schedules. A newly
  // received sample must be judged at least at its local receipt time; otherwise
  // it can appear to be in the future until the next display-clock tick.
  const age = Math.max(displayClock, receivedAt) - collectedAt;
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

function ResourceRow({
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
    <div className="overview-resource-row" role="group" aria-label={`${label} ${usage === undefined ? "读数不可用" : `${Math.round(usage)}%`}`}>
      <span className="overview-resource-label">{label}</span>
      <strong>{usage === undefined ? "—" : `${Math.round(usage)}%`}</strong>
      <div className="overview-resource-track" aria-hidden="true">
        <div className={`metric-fill metric-fill-${status}`} style={{ width: `${width}%` }} />
      </div>
      <span className="overview-resource-hint">{hint}</span>
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
      <h2>尚未连接</h2>
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
      receivedAt={snapshotQuery.dataUpdatedAt}
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
  receivedAt,
  refreshing,
  onRefresh,
  onConnect,
  connecting,
  connectionError,
}: {
  server: Server;
  snapshot: ServerSnapshot;
  receivedAt: number;
  refreshing: boolean;
  onRefresh: () => void;
  onConnect: () => void;
  connecting: boolean;
  connectionError?: string;
}) {
  const [now, setNow] = useState(() => Date.now());
  const setPrimary = useWorkspaceStore((state) => state.setPrimary);
  useEffect(() => {
    const interval = window.setInterval(() => setNow(Date.now()), 10_000);
    return () => window.clearInterval(interval);
  }, []);

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
  const age = snapshotAge(snapshot.collectedAt, now, receivedAt);
  const connected = server.status === "connected";
  const freshnessKind = connected ? age.kind : "offline";
  const freshnessLabel = connected
    ? age.kind === "stale" ? "快照已过期" : age.kind === "unknown" ? "新鲜度未知" : "采集较新"
    : "缓存快照";
  const network = sampleFor(snapshot, "network")?.ok === false ? undefined : snapshot.network;
  const failedSamples = (snapshot.collectors ?? []).filter((sample) => !sample.ok);
  const diagnosticsExpanded = collection.kind !== "complete" || age.kind !== "fresh" || !connected || failedSamples.length > 0;
  const runningContainers = docker?.containers.filter((container) => container.state === "running").length ?? 0;
  const otherContainers = (docker?.containers.length ?? 0) - runningContainers;
  const diagnosticsLabel = failedSamples.length > 0 ? `${failedSamples.length} 项失败` : collection.label;
  const reportedCollectors = COLLECTORS.filter((collector) => collection.sampleMap.has(collector.id)).length;

  return (
    <div className="overview-page overview-inspector">
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

      <section className="overview-status" aria-label="采集状态">
        <div className="overview-status-heading">
          <div className="overview-observed-at">
            <span className={"overview-freshness overview-freshness-" + freshnessKind}><span aria-hidden="true" />{freshnessLabel}</span>
            <time dateTime={snapshot.collectedAt} title={"本地时间 " + formatTimestamp(snapshot.collectedAt)}>{age.label}</time>
          </div>
          <button type="button" className="icon-button overview-refresh" onClick={onRefresh} disabled={!connected || refreshing} title={refreshing ? "正在采集新快照" : connected ? "刷新服务器状态" : "重新连接后才能刷新"} aria-label={refreshing ? "正在采集新快照" : connected ? "刷新服务器状态" : "重新连接后才能刷新"}>
            <Icon name="refresh" size="sm" />
          </button>
        </div>
        {diagnosticsExpanded ? <p className="overview-collection-warning" role="status">{diagnosticsLabel} · {collection.description}</p> : null}
        {refreshing ? <p className="overview-collection-progress" role="status">正在采集，当前显示上次完成的结果。</p> : null}
      </section>

      <section className="overview-resources" aria-label="系统资源读数">
        <div className="overview-resource-list">
          <ResourceRow label="CPU" usage={cpu?.usagePercent} thresholds={HEALTH_THRESHOLDS.cpu} hint={cpu ? `${cpu.cores} 核处理器` : "采集器未返回可用读数"} />
          <ResourceRow label="内存" usage={memory?.usagePercent} thresholds={HEALTH_THRESHOLDS.memory} hint={memory ? `${formatBytes(memory.usedBytes)} / ${formatBytes(memory.totalBytes)}` : "采集器未返回可用读数"} />
          <ResourceRow label="磁盘" usage={diskUsage} thresholds={HEALTH_THRESHOLDS.disk} hint={disks.length ? `${disks.length} 个挂载点 · 显示最高占用` : "采集器未返回可用读数"} />
        </div>
        <button type="button" className="button-secondary overview-task-action" onClick={() => setPrimary("tasks")} aria-label={`在 ${server.name} 上创建任务`}><Icon name="agent" size="sm" /><span>创建排查任务</span><Icon name="chevronRight" size="sm" /></button>
      </section>

      <details className="overview-inspector-section">
        <summary><Icon name="chevronRight" size="xs" /><span>系统属性</span><small>{os ? `${os.distribution} ${os.version}` : "信息不完整"}</small></summary>
        <div className="overview-section-content">
          <dl className="overview-properties">
            <div><dt>主机名</dt><dd>{os?.hostname ?? "—"}</dd></div>
            <div><dt>系统</dt><dd>{os ? `${os.distribution} ${os.version}` : "—"}</dd></div>
            <div><dt>内核</dt><dd>{os?.kernel ?? "—"}</dd></div>
            <div><dt>架构</dt><dd>{os?.arch ?? "—"}</dd></div>
            <div><dt>运行时间</dt><dd>{formatUptime(uptime)}</dd></div>
            <div><dt>处理器</dt><dd>{cpu?.model ?? "—"}</dd></div>
            <div><dt>核心数</dt><dd>{cpu?.cores ?? "—"}</dd></div>
            <div><dt>系统负载</dt><dd>{cpu?.loadAverage.join(" / ") ?? "—"}</dd></div>
            <div><dt>总内存</dt><dd>{formatBytes(memory?.totalBytes)}</dd></div>
            <div><dt>可用内存</dt><dd>{formatBytes(memory?.availableBytes)}</dd></div>
          </dl>
          <h3 className="overview-subsection-title">磁盘挂载</h3>
          {disks.length === 0 ? <p className="muted-copy">尚无可用磁盘读数。</p> : <ul className="overview-property-list">{disks.map((disk, index) => <li key={`${disk.device}-${disk.mountPoint}-${index}`}>
            <strong>{disk.mountPoint}</strong><span>{disk.device}</span>
            <span>{formatBytes(disk.usedBytes)} / {formatBytes(disk.totalBytes)} · {Math.round(disk.usagePercent)}%</span>
          </li>)}</ul>}
          <h3 className="overview-subsection-title">网络累计流量</h3>
          {!network?.length ? <p className="muted-copy">尚无可用网络读数。</p> : <ul className="overview-property-list">{network.map((item, index) => <li key={`${item.name}-${index}`}>
            <strong>{item.name}</strong><span>接收 {formatBytes(item.rxBytes)} · 发送 {formatBytes(item.txBytes)}</span>
          </li>)}</ul>}
        </div>
      </details>

      <details className="overview-inspector-section" key={`docker-${server.id}`}>
        <summary><Icon name="chevronRight" size="xs" /><span>Docker</span><small>{docker?.available ? `${docker.containers.length} 个容器${otherContainers > 0 ? ` · ${otherContainers} 非运行` : ""}` : "不可用"}</small></summary>
        <div className="overview-section-content">
          {!docker ? <p className="muted-copy">尚未返回 Docker 采集结果。</p> : !docker.available ? <p className="muted-copy">目标服务器未安装或未启用 Docker。</p> : docker.containers.length === 0 ? <p className="muted-copy">当前没有容器。</p> : <ul className="overview-container-list">
            {docker.containers.map((container, index) => <li key={`${container.name}-${index}`}>
              <div><span className={`container-dot ${container.state === "running" ? "container-dot-running" : ""}`} /><strong>{container.name}</strong><small>{container.state}</small></div>
              <span>{container.image}</span><span>{container.status} · 重启 {container.restartCount} 次</span>
            </li>)}
          </ul>}
        </div>
      </details>

      <RecentServerActivity serverId={server.id} />

      <details className="overview-inspector-section overview-diagnostics" key={`diagnostics-${server.id}-${diagnosticsExpanded}`} open={diagnosticsExpanded}>
        <summary><Icon name="chevronRight" size="xs" /><span>采集诊断</span><small>{reportedCollectors}/{COLLECTORS.length} · {diagnosticsLabel}</small></summary>
        <div className="overview-section-content">
          <p className="overview-diagnostics-description">{collection.description}</p>
          <dl className="overview-properties">
            <div><dt>数据来源</dt><dd>SSH 远端采集器</dd></div>
            <div><dt>最后采集</dt><dd><time dateTime={snapshot.collectedAt}>{formatTimestamp(snapshot.collectedAt)}</time></dd></div>
            <div><dt>更新频率</dt><dd>{connected ? "每 15 秒自动采集" : "连接后恢复自动采集"}</dd></div>
            {!connected ? <div><dt>当前数据</dt><dd>缓存快照</dd></div> : null}
          </dl>
          <ul className="overview-collector-list">
            {COLLECTORS.map((collector) => <CollectorRow key={collector.id} label={collector.label} sample={collection.sampleMap.get(collector.id)} />)}
            {(snapshot.collectors ?? [])
              .filter((sample) => !COLLECTORS.some((collector) => collector.id === normalizeCollectorId(sample.collectorId)))
              .map((sample, index) => <CollectorRow key={`${sample.collectorId}-${index}`} label={sample.collectorId} sample={sample} />)}
          </ul>
        </div>
      </details>
    </div>
  );
}

function CollectorRow({ label, sample }: { label: string; sample?: CollectorSample }) {
  const state = sample ? sample.ok ? "success" : "failure" : "unknown";
  return <li>
    <span>{label}</span><span className={`overview-collector-state overview-collector-state-${state}`}>{sample ? sample.ok ? "成功" : "失败" : "未报告"}</span>
    {sample ? <time dateTime={sample.collectedAt}>{formatTimestamp(sample.collectedAt)}</time> : <small>本次快照无状态记录</small>}
    {sample?.error ? <p className="overview-collector-error">{sample.error}</p> : null}
  </li>;
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
    <details className="overview-inspector-section">
      <summary><Icon name="chevronRight" size="xs" /><span>最近动态</span><small>{activities.isLoading ? "读取中" : activities.isError ? "读取失败" : rows.length > 0 ? `最近 ${rows.length} 条` : "暂无动态"}</small></summary>
      <div className="overview-section-content">
      {activities.isLoading ? <p className="muted-copy" role="status">正在读取服务器动态…</p> : null}
      {activities.isError ? <p className="error-copy" role="alert">无法读取服务器动态：{errorMessage(activities.error)}</p> : null}
      {!activities.isLoading && !activities.isError && rows.length === 0 ? <div className="activity-empty"><Icon name="activity" size="lg" /><p>暂无动态</p></div> : null}
      {rows.length ? <ul className="overview-activity-list">{rows.map((activity) => (
        <li key={activity.id}>
          <strong>{activity.title}</strong>
          {activity.description || activity.reason ? <span>{activity.description ?? activity.reason}</span> : null}
          <time dateTime={activity.createdAt}>{formatTimestamp(activity.createdAt)}</time>
        </li>
      ))}</ul> : null}
      <button type="button" className="overview-all-activity" onClick={() => setServerPage("activity")}>查看全部活动<Icon name="chevronRight" size="xs" /></button>
      </div>
    </details>
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
    <div className="welcome-copy"><h2>暂无服务器</h2></div>
    <div className="welcome-footer"><button type="button" className="button-secondary" onClick={() => setPrimary("settings")}>设置 <Icon name="chevronRight" size="sm" /></button></div>
  </div>;
}
