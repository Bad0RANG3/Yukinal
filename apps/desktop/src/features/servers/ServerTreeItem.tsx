import { useQuery } from "@tanstack/react-query";
import { IPC_COMMANDS, type Server } from "@yukinal/shared";
import { useId, useState, type ReactNode } from "react";

import { Icon } from "../../components/Icon.js";
import { errorMessage, formatTimestamp } from "../../lib/format.js";
import { useHostKeyStatus } from "../../lib/host-key.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { ENVIRONMENT_LABEL_SHORT, SERVER_STATUS_LABEL } from "../../lib/labels.js";
import { useWorkpagesStore } from "../workpages/workpages-store.js";
import { dashboardResources } from "../overview/dashboard-model.js";
import { TeamActionCue, TeamOperationOutline } from "../agent-nist/TeamActivity.js";
import { ServerRowActions } from "./ServerRowActions.js";

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
  const host = server.connection.host.includes(":") ? `[${server.connection.host}]` : server.connection.host;
  const address = `${server.connection.username}@${host}:${server.connection.port}`;
  const openServices = () => { if (disabled) return; onSelect(); useWorkpagesStore.getState().openTool("services", { serverId: server.id }); onOpenWorkpage?.(); };
  return <li className="server-list-item">
    <div className={`server-row team-operation-target ${selected ? "server-row-selected" : ""}`}>
      {server.capabilities.docker ? <button type="button" className="server-tree-toggle" aria-label={`${expanded ? "收起" : "展开"} ${server.name} 的资源`} aria-expanded={expanded} aria-controls={groupId} onClick={() => setExpanded((value) => !value)}><Icon name={expanded ? "chevronDown" : "chevronRight"} size="xs" /></button> : <span className="server-tree-spacer" />}
      <button type="button" disabled={disabled} onClick={onSelect} className="server-row-main" data-attention="server:select" data-server-id={server.id} aria-current={selected ? "true" : undefined} aria-label={`${server.name}，${address}，${ENVIRONMENT_LABEL_SHORT[server.metadata.environment]}，${SERVER_STATUS_LABEL[server.status]}`} title={`${server.name}\n${address}\n${ENVIRONMENT_LABEL_SHORT[server.metadata.environment]} · ${SERVER_STATUS_LABEL[server.status]}`}>
        <span className="server-row-heading">
          <span className="server-row-name">{server.name}</span><TeamActionCue serverId={server.id} />
          <span className={`server-connection-mark server-state-${server.status}`} title={SERVER_STATUS_LABEL[server.status]} aria-hidden="true">{server.status === "error" ? <Icon name="warning" size="xs" /> : server.status === "connecting" ? <Icon name="refresh" size="xs" /> : <span />}</span>
        </span>
        <span className="server-row-detail"><span className="server-row-host">{host}</span>{server.connection.port !== 22 ? <span className="server-row-port">:{server.connection.port}</span> : null}</span>
      </button>
      <ServerRowActions name={server.name}>{children}</ServerRowActions>
      <TeamOperationOutline serverId={server.id} />
    </div>
    {expanded && server.capabilities.docker ? <div className="server-tree-children" id={groupId}>
      <div className="server-tree-group"><Icon name="services" size="xs" /><span>Docker</span>{connected && trusted && resources.containers ? <span className="server-tree-count">{resources.containers.length}</span> : null}{connected && trusted ? <button type="button" className="icon-button" onClick={openServices} disabled={disabled} aria-label={`打开 ${server.name} 的服务工作页`} title="在中间查看服务"><Icon name="chevronRight" size="xs" /></button> : null}</div>
      {!connected ? <p className="server-tree-message">未连接</p> : hostKey.isLoading ? <p className="server-tree-message" role="status">读取身份…</p> : !trusted ? <button type="button" className="server-tree-message text-button" disabled={disabled} onClick={onSelect}>{hostKey.isError ? "身份读取失败，查看" : "核验主机身份"}</button> : <>
        {snapshot.isLoading ? <p className="server-tree-message" role="status">读取容器…</p> : null}
        {snapshot.isError ? <button type="button" className="server-tree-message text-button" title={errorMessage(snapshot.error)} disabled={snapshot.isFetching} onClick={() => void snapshot.refetch()}>{resources.containers ? "上次快照 · 重试" : "读取失败 · 重试"}</button> : null}
        {resources.containers ? resources.containers.length ? <ul className="server-tree-containers" aria-label={`${server.name} 的 Docker 容器`}>{resources.containers.map((container) => <li key={container.name}>
          <details>
            <summary title={`${container.name} · ${container.image} · ${container.state}${resources.snapshot ? ` · ${formatTimestamp(resources.snapshot.collectedAt)}` : ""}`} aria-label={`${server.name} · ${container.name}，${container.state}`}><Icon name="chevronRight" size="xs" /><span className="server-container-name">{container.name}</span><small className={container.state === "running" ? "is-running" : "is-stopped"}>{container.state === "running" ? "运行中" : container.state === "exited" ? "已停止" : container.state}</small></summary>
            <dl className="server-container-detail"><div><dt>镜像</dt><dd>{container.image}</dd></div><div><dt>状态</dt><dd>{container.status}</dd></div>{container.restartCount !== undefined ? <div className="server-container-restarts"><dt>重启</dt><dd>{container.restartCount} 次</dd></div> : null}</dl>
          </details>
        </li>)}</ul> : <p className="server-tree-message">暂无容器</p> : snapshot.isSuccess ? <button type="button" className="server-tree-message text-button" disabled={disabled} onClick={openServices}>Docker 状态不可用</button> : null}
      </>}
    </div> : null}
  </li>;
}
