import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { type ServiceSource, type ServiceState } from "@yukinal/shared";

import { errorMessage } from "../../lib/format.js";
import { isDesktopShell } from "../../lib/ipc.js";
import { serverServicesQueryOptions } from "../../lib/remote-feeds.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { EmptyPanel, ErrorPanel, LoadingPanel, PreviewEmpty } from "../../components/PanelStates.js";
import { Icon } from "../../components/Icon.js";
import { KeywordText } from "../../components/KeywordText.js";
import { PageToolbar } from "../../components/PageToolbar.js";
import { RemoteListPager } from "../../components/RemoteListPager.js";

const PAGE_SIZE = 20;
const STATE_LABEL: Record<ServiceState, string> = { running: "运行中", stopped: "已停止", failed: "失败", unknown: "未知" };
const SOURCE_LABEL: Record<ServiceSource, string> = { systemd: "systemd 服务", docker: "Docker 容器", unavailable: "未发现服务管理器" };

export function ServicesPane({ serverId, active = true }: { serverId?: string | null; active?: boolean } = {}) {
  const workspaceServerId = useWorkspaceStore((state) => state.selectedServerId);
  const selectedServerId = serverId === undefined ? workspaceServerId : serverId;
  const shell = isDesktopShell();
  const servicesQuery = useQuery(serverServicesQueryOptions(selectedServerId, active));
  const [search, setSearch] = useState("");
  const [state, setState] = useState<ServiceState | "all">("all");
  const [page, setPage] = useState(0);
  const refresh = () => void servicesQuery.refetch({ cancelRefetch: false });

  if (!active) return null;
  if (!shell) return <PreviewEmpty icon="services" body="桌面功能不可用" />;
  if (!selectedServerId) return <EmptyPanel icon="services" title="选择一台服务器" />;
  if (servicesQuery.isLoading) return <LoadingPanel title="正在读取服务状态" />;
  if (!servicesQuery.data) return <ErrorPanel showIcon title="无法读取服务状态" message={errorMessage(servicesQuery.error)} onRetry={refresh} />;

  const response = servicesQuery.data;
  const running = response.services.filter((service) => service.state === "running").length;
  const failed = response.services.filter((service) => service.state === "failed").length;
  const needle = search.trim().toLocaleLowerCase();
  const filtered = response.services.filter((service) => (state === "all" || service.state === state) && (!needle || `${service.name} ${service.description ?? ""}`.toLocaleLowerCase().includes(needle)));
  const currentPage = Math.min(page, Math.max(0, Math.ceil(filtered.length / PAGE_SIZE) - 1));
  const visible = filtered.slice(currentPage * PAGE_SIZE, (currentPage + 1) * PAGE_SIZE);

  return <div className="services-page">
    <PageToolbar actions={<button type="button" className="button-secondary" onClick={refresh} disabled={servicesQuery.isFetching} title="刷新服务" aria-label="刷新服务"><Icon name="refresh" size="sm" /> {servicesQuery.isFetching ? "读取中" : "刷新"}</button>}>
      <span className={`service-source service-source-${response.source}`}>{SOURCE_LABEL[response.source]}</span>
      <span>{response.services.length} 项 · {running} 运行{failed ? ` · ${failed} 失败` : ""} · 按需刷新</span>
    </PageToolbar>
    <div className="remote-list-filters">
      <input type="search" aria-label="筛选已加载服务" placeholder="筛选服务" value={search} onChange={(event) => { setSearch(event.target.value); setPage(0); }} />
      <select aria-label="服务状态" value={state} onChange={(event) => { setState(event.target.value as ServiceState | "all"); setPage(0); }}>
        <option value="all">全部状态</option><option value="failed">失败</option><option value="running">运行中</option><option value="stopped">已停止</option><option value="unknown">未知</option>
      </select>
    </div>
    {servicesQuery.isError ? <div className="service-notice" role="status">刷新失败，保留上次结果。{errorMessage(servicesQuery.error)}</div> : null}
    {response.message ? <div className="service-notice">{response.message}</div> : null}
    {filtered.length === 0 ? <EmptyPanel extraClass="service-empty" icon="services" title={response.services.length ? "没有匹配服务" : "没有可展示的服务"} body={response.message} /> : <ul className="service-list" aria-label="远程服务列表">
      {visible.map((service) => <li className="service-row" key={`${response.source}:${service.name}`}>
        <span className={`service-state-dot service-state-dot-${service.state}`} aria-hidden="true" />
        <div className="service-row-copy"><strong><KeywordText text={service.name} /></strong><span><KeywordText text={service.description ?? "无描述"} /></span></div>
        <code><KeywordText text={service.status} /></code>
        <span className={`service-state service-state-${service.state}`}>{STATE_LABEL[service.state]}</span>
      </li>)}
    </ul>}
    <RemoteListPager page={currentPage} pageSize={PAGE_SIZE} count={filtered.length} onPage={setPage} />
  </div>;
}
