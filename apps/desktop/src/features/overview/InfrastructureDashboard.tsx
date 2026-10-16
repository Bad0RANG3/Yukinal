import { useQuery } from "@tanstack/react-query";
import { IPC_COMMANDS, type Server } from "@yukinal/shared";
import { useEffect, useRef, useState } from "react";

import { Icon } from "../../components/Icon.js";
import { errorMessage, formatRelativeTime, formatTimestamp } from "../../lib/format.js";
import { useHostKeyStatus } from "../../lib/host-key.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { ENVIRONMENT_LABEL_SHORT, SERVER_STATUS_LABEL } from "../../lib/labels.js";
import { useServerAction, useServers } from "../../lib/servers.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { dashboardResources } from "./dashboard-model.js";

export function InfrastructureDashboard({ onAttentionRequested }: { onAttentionRequested?: () => void }) {
  const servers = useServers();
  const rows = servers.data ?? [];
  const connected = rows.filter((server) => server.status === "connected").length;
  return <section className="infrastructure-dashboard" aria-label="服务器仪表台">
    <div className="dashboard-summary"><div><strong>{rows.length}</strong><span>台服务器</span></div><span>{connected} 已连接</span></div>
    {servers.isError ? <p className="form-error" role="alert">{errorMessage(servers.error)}</p> : null}
    {servers.isLoading ? <p className="muted-copy" role="status">正在读取服务器…</p> : null}
    {!rows.length && !servers.isLoading && !servers.isError ? <div className="dashboard-empty">
      <h2>暂无服务器</h2>
    </div> : null}
    <div className="infrastructure-cards">{rows.map((server) => <InfrastructureCard key={server.id} server={server} onAttentionRequested={onAttentionRequested} />)}</div>
  </section>;
}

function InfrastructureCard({ server, onAttentionRequested }: { server: Server; onAttentionRequested?: () => void }) {
  const ref = useRef<HTMLElement>(null);
  const [visible, setVisible] = useState(false);
  useEffect(() => {
    if (!ref.current) return;
    const observer = new IntersectionObserver(([entry]) => setVisible(Boolean(entry?.isIntersecting)), { rootMargin: "64px" });
    observer.observe(ref.current);
    return () => observer.disconnect();
  }, []);
  const selectServer = useWorkspaceStore((state) => state.selectServer);
  const setServerPage = useWorkspaceStore((state) => state.setServerPage);
  const setPrimary = useWorkspaceStore((state) => state.setPrimary);
  const setAgentDraft = useWorkspaceStore((state) => state.setAgentDraft);
  const agentBusy = useWorkspaceStore((state) => state.agentBusy);
  const hasDraft = useWorkspaceStore((state) => Boolean(state.agentDraft.trim()) || state.agentAttachments.length > 0);
  const selected = useWorkspaceStore((state) => state.selectedServerId === server.id);
  const { action, busy } = useServerAction();
  const hostKey = useHostKeyStatus(server.id, { enabled: visible });
  const trusted = Boolean(server.connection.hostCertificateAuthority) || hostKey.data?.pinned === true;
  const connected = server.status === "connected";
  const query = useQuery({
    queryKey: ["serverSnapshot", server.id],
    queryFn: async () => (await callDesktop(IPC_COMMANDS.serverSnapshot, { serverId: server.id })).snapshot,
    enabled: visible && connected && trusted && isDesktopShell() && !busy,
    staleTime: 60_000,
    refetchInterval: visible ? 60_000 : false,
    retry: false,
  });
  const resources = dashboardResources(server.id, query.data);
  const inspect = () => selectServer(server.id);
  const askAI = () => {
    // Only fill an empty draft. Never overwrite the user's ongoing question or send it automatically.
    if (agentBusy || hasDraft) return;
    selectServer(server.id);
    setPrimary("servers");
    setAgentDraft(`请对 @${server.name} 做一次只读健康巡检，检查系统资源和容器状态，给出异常依据与建议。不要修改文件或重启服务。`);
    onAttentionRequested?.();
    requestAnimationFrame(() => document.querySelector<HTMLTextAreaElement>(".agent-composer textarea")?.focus());
  };
  return <article ref={ref} className={`infrastructure-card ${selected ? "is-selected" : ""}`} aria-label={server.name}>
    <header className="infrastructure-card-header"><button type="button" className="infrastructure-card-identity" onClick={inspect} title={server.name}><span className="infrastructure-server-icon"><Icon name="servers" size="md" /></span><span><strong>{server.name}</strong><small title={server.connection.host}>{server.connection.host}</small></span></button></header>
    <div className="infrastructure-card-meta"><span className={`infrastructure-status server-status-${server.status}`}><span className="server-dot" />{SERVER_STATUS_LABEL[server.status]}</span><span>{ENVIRONMENT_LABEL_SHORT[server.metadata.environment]}</span>{server.metadata.region ? <span>{server.metadata.region}</span> : null}</div>
    <div className="infrastructure-metrics" aria-label="资源使用率"><ResourceMetric label="CPU" value={resources.cpu} /><ResourceMetric label="内存" value={resources.memory} /><ResourceMetric label="磁盘" value={resources.disk} /></div>
    {resources.snapshot ? <p className="infrastructure-collected"><time dateTime={resources.snapshot.collectedAt} title={formatTimestamp(resources.snapshot.collectedAt)}>{formatRelativeTime(resources.snapshot.collectedAt)}采集</time>{!connected || query.isError ? " · 上次快照" : ""}</p> : <p className="infrastructure-collected">{!connected ? "未连接" : hostKey.isError ? "无法读取身份状态" : !trusted ? "核验主机身份后采集" : query.isFetching ? "正在采集…" : "尚无资源快照"}</p>}
    {query.isError ? <p className="infrastructure-card-error" role="status">刷新失败：{errorMessage(query.error)}</p> : null}
    {resources.containers?.length ? <details className="infrastructure-resource-group"><summary><Icon name="services" size="sm" /><span>Docker 容器</span><span>{resources.containers.length}</span><Icon name="chevronDown" size="sm" /></summary><ul>{resources.containers.map((container) => <li key={container.name}><span className={`container-dot ${container.state === "running" ? "container-dot-running" : ""}`} /><span className="resource-identity"><strong title={container.name}>{container.name}</strong><small title={container.image}>{container.image}</small></span><span className="resource-state">{container.state === "running" ? "运行中" : container.state}</span></li>)}</ul><button type="button" className="text-button resource-group-more" onClick={() => { selectServer(server.id); setServerPage("services"); }}>查看服务详情<Icon name="chevronRight" size="xs" /></button></details> : null}
    <footer className="infrastructure-card-footer"><button type="button" className="text-button" onClick={inspect}>查看详情<Icon name="chevronRight" size="xs" /></button>{connected ? <button type="button" className="text-button" disabled={agentBusy || hasDraft} title={hasDraft ? "先完成当前输入，再开始新的巡检" : "填入巡检草稿"} onClick={askAI}><Icon name="activity" size="sm" />检查资源</button> : <button type="button" className="text-button" disabled={busy || server.status === "connecting"} onClick={() => action.mutate({ type: "connect", serverId: server.id })}><Icon name="connect" size="sm" />连接</button>}</footer>
    {action.isError ? <p className="infrastructure-card-error" role="alert">{errorMessage(action.error)}</p> : null}
  </article>;
}

function ResourceMetric({ label, value }: { label: string; value?: number }) {
  const available = value !== undefined && Number.isFinite(value);
  const percent = available ? Math.max(0, Math.min(100, value)) : undefined;
  return <div className="infrastructure-metric"><span>{label}</span><strong>{percent === undefined ? "—" : `${Math.round(percent)}%`}</strong><span className={`infrastructure-meter ${percent !== undefined && percent >= 90 ? "is-high" : ""}`} aria-hidden="true"><span style={{ width: `${percent ?? 0}%` }} /></span></div>;
}
