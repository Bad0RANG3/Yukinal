import { useQuery } from "@tanstack/react-query";
import { IPC_COMMANDS, type Server } from "@yukinal/shared";
import { useId, useState, type ReactNode } from "react";

import { Icon } from "../../components/Icon.js";
import { errorMessage, formatTimestamp } from "../../lib/format.js";
import { useHostKeyStatus } from "../../lib/host-key.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { ENVIRONMENT_LABEL_SHORT, SERVER_STATUS_LABEL } from "../../lib/labels.js";
import { dashboardResources } from "../overview/dashboard-model.js";
import { useWorkpagesStore } from "../workpages/workpages-store.js";

export function ServerTreeItem({ server, selected, disabled, onSelect, onOpenWorkpage, children }: {
  server: Server; selected: boolean; disabled: boolean; onSelect: () => void; onOpenWorkpage?: () => void; children: ReactNode;
}) {
  const [expanded, setExpanded] = useState(false);
  const groupId = useId();
  const connected = server.status === "connected";
  const hostKey = useHostKeyStatus(server.id, { enabled: expanded && connected });
  const trusted = Boolean(server.connection.hostCertificateAuthority) || hostKey.data?.pinned === true;
  const snapshot = useQuery({
    queryKey: ["serverSnapshot", server.id],
    queryFn: async () => (await callDesktop(IPC_COMMANDS.serverSnapshot, { serverId: server.id })).snapshot,
    enabled: expanded && connected && trusted && isDesktopShell(),
    staleTime: 60_000, retry: false,
  });
  const resources = dashboardResources(server.id, snapshot.data);
  const openServices = () => { if (disabled) return; onSelect(); useWorkpagesStore.getState().openTool("services", { serverId: server.id }); onOpenWorkpage?.(); };
  return <li className="server-list-item">
    <div className={`server-row ${selected ? "server-row-selected" : ""}`}>
      {server.capabilities.docker ? <button type="button" className="server-tree-toggle" aria-label={`${expanded ? "收起" : "展开"} ${server.name} 的资源`} aria-expanded={expanded} aria-controls={groupId} onClick={() => setExpanded((value) => !value)}><Icon name={expanded ? "chevronDown" : "chevronRight"} size="xs" /></button> : <span className="server-tree-spacer" />}
      <button type="button" disabled={disabled} onClick={onSelect} className="server-row-main" aria-current={selected ? "true" : undefined} aria-label={`${server.name}，${server.connection.host}:${server.connection.port}，${ENVIRONMENT_LABEL_SHORT[server.metadata.environment]}，${SERVER_STATUS_LABEL[server.status]}`} title={`${server.name} · ${SERVER_STATUS_LABEL[server.status]}`}>
        <span className="server-row-heading">
          <span className={`server-row-name server-state-${server.status}`}>{server.name}</span>
          <span className={`server-row-status server-state-${server.status}`}>{SERVER_STATUS_LABEL[server.status]}</span>
        </span>
        <span className="server-row-detail"><span className="server-row-host" title={`${server.connection.host}:${server.connection.port}`}>{server.connection.host}</span><span className="server-row-port">:{server.connection.port}</span><span className="server-row-env">{ENVIRONMENT_LABEL_SHORT[server.metadata.environment]}</span></span>
      </button>
      <div className="server-row-actions">{children}</div>
    </div>
    {expanded && server.capabilities.docker ? <div className="server-tree-children" id={groupId}>
      <div className="server-tree-group"><Icon name="services" size="xs" /><span>Docker</span>{connected && trusted && resources.containers ? <span>{resources.containers.length}</span> : null}</div>
      {!connected ? <p className="server-tree-message">未连接</p> : hostKey.isLoading ? <p className="server-tree-message" role="status">读取身份…</p> : !trusted ? <button type="button" className="server-tree-message text-button" disabled={disabled} onClick={onSelect}>{hostKey.isError ? "身份读取失败，查看" : "核验主机身份"}</button> : <>
        {snapshot.isLoading ? <p className="server-tree-message" role="status">读取容器…</p> : null}
        {snapshot.isError ? <button type="button" className="server-tree-message text-button" title={errorMessage(snapshot.error)} disabled={snapshot.isFetching} onClick={() => void snapshot.refetch()}>{resources.containers ? "上次快照 · 重试" : "读取失败 · 重试"}</button> : null}
        {resources.containers ? resources.containers.length ? <ul className="server-tree-containers" aria-label={`${server.name} 的 Docker 容器`}>{resources.containers.map((container) => <li key={container.name}>
          <details>
            <summary title={`${container.name} · ${container.image} · ${container.state}${resources.snapshot ? ` · ${formatTimestamp(resources.snapshot.collectedAt)}` : ""}`} aria-label={`${server.name} · ${container.name}，${container.state}`}><span className={`server-container-name ${container.state === "running" ? "is-running" : "is-stopped"}`}>{container.name}</span><small>{container.state === "running" ? "运行中" : container.state === "exited" ? "已停止" : container.state}</small></summary>
            <div className="server-container-detail"><strong>{container.name}</strong><span title="镜像">{container.image}</span><span>{container.status}{container.restartCount !== undefined ? ` · 重启 ${container.restartCount}` : ""}</span></div>
          </details>
        </li>)}</ul> : <p className="server-tree-message">暂无容器</p> : snapshot.isSuccess ? <button type="button" className="server-tree-message text-button" disabled={disabled} onClick={openServices}>Docker 状态不可用</button> : null}
      </>}
    </div> : null}
  </li>;
}
