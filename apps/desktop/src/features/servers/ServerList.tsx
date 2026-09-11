import { type Server } from "@yukinal/shared";
import { useEffect, useMemo, useState } from "react";

import { errorMessage } from "../../lib/format.js";
import { isDesktopShell } from "../../lib/ipc.js";
import { ENVIRONMENT_LABEL_SHORT, SERVER_STATUS_LABEL } from "../../lib/labels.js";
import { useServerAction, useServers } from "../../lib/servers.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { Icon } from "../../components/Icon.js";
import { AddServerModal } from "./AddServerModal.js";

const ENV_DOT: Record<Server["metadata"]["environment"], string> = { production: "server-dot-production", staging: "server-dot-staging", development: "server-dot-development", local: "server-dot-local", unknown: "server-dot-unknown" };

export function ServerList({ onClose }: { onClose?: () => void }) {
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const selectServer = useWorkspaceStore((state) => state.selectServer);
  const syncServerSelection = useWorkspaceStore((state) => state.syncServerSelection);
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<Server | undefined>();
  const [filter, setFilter] = useState("");
  const shell = isDesktopShell();
  const servers = useServers();
  const { action, busy } = useServerAction();
  const serverRows = servers.data ?? [];
  const filteredServers = useMemo(() => { const needle = filter.trim().toLowerCase(); return needle ? serverRows.filter((server) => [server.name, server.connection.host, server.metadata.environment, server.metadata.region].filter((value): value is string => Boolean(value)).some((value) => value.toLowerCase().includes(needle))) : serverRows; }, [filter, serverRows]);
  useEffect(() => {
    if (servers.data) syncServerSelection(servers.data.map((server) => server.id));
  }, [syncServerSelection, servers.data]);

  return (
    <aside id="server-sidebar" className="server-sidebar" aria-label="服务器列表">
      <div className="server-sidebar-header">
        <div><p className="eyebrow">工作区</p><h2>服务器</h2></div>
        <div className="server-sidebar-header-actions">
          {onClose ? <button type="button" className="icon-button server-sidebar-close" aria-label="关闭服务器列表" title="关闭服务器列表" onClick={onClose}><Icon name="close" size="md" /></button> : null}
          <button type="button" className="icon-button icon-button-accent" aria-label="添加服务器" title="添加服务器" onClick={() => setAdding(true)}>
            <Icon name="plus" size="md" />
          </button>
        </div>
      </div>

      <div className="server-search">
        <Icon name="search" size="sm" />
        <input value={filter} onChange={(event) => setFilter(event.target.value)} placeholder="搜索名称或主机" aria-label="搜索服务器" />
        {filter ? <button type="button" className="search-clear" aria-label="清除搜索" onClick={() => setFilter("")}><Icon name="close" size="sm" /></button> : null}
      </div>

      <div className="server-list-meta">
        <span>{filter.trim() ? `${filteredServers.length} / ${serverRows.length}` : serverRows.length} 台服务器</span>
        <button type="button" className="text-button icon-text-button" onClick={() => void servers.refetch()} disabled={!shell || servers.isFetching} aria-label="刷新服务器列表" title="刷新服务器列表">
          <Icon name="refresh" size="sm" />
        </button>
      </div>

      {servers.isError ? <div className="inline-error"><strong>读取失败</strong><span>{errorMessage(servers.error)}</span><button type="button" className="text-button text-button-danger" onClick={() => void servers.refetch()}>重试</button></div> : null}
      {action.isError ? <div className="inline-error" role="alert"><strong>操作未完成</strong><span>{action.error.message}</span><button type="button" className="text-button" onClick={() => action.reset()}>关闭提示</button></div> : null}
      {!shell ? <div className="empty-state compact-empty"><p>浏览器预览模式</p><small>启动 Tauri 后加载本地服务器。</small></div> : null}
      {shell && !serverRows.length && servers.isSuccess ? <div className="empty-state compact-empty"><Icon name="servers" size="xxl" /><p>连接你的第一台服务器</p><small>添加 SSH 连接，开始管理远程环境。</small><button type="button" className="secondary-button" onClick={() => setAdding(true)}>添加服务器</button></div> : null}
      {servers.isLoading ? <div className="skeleton-list" aria-label="正在加载服务器" /> : null}
      {filteredServers.length ? (
        <ul className="server-list">
          {filteredServers.map((server) => (
            <li key={server.id} className="server-list-item">
              <div className={`server-row ${selectedServerId === server.id ? "server-row-selected" : ""}`}>
                {/* 这一行是整块可点的按钮，里面拼了三段文字（名称、主机端口、环境）。
                    不给它一个明确的 aria-label 的话，无障碍名称就是这三段的直接
                    连接 —— 实测会被念成「Production APIapi.example.com:22生产」，
                    三段黏成一坨，读屏用户分不出哪儿是哪儿。显式写出来，用中文逗号
                    断句。

                    title 也是必需的：名称在 218px 宽的侧栏里会被省略号截断
                    （「Staging Database Cluster」实测溢出 40px），而这是列表里
                    唯一能看全它的地方。 */}
                <button type="button" onClick={() => selectServer(server.id)} className="server-row-main" aria-current={selectedServerId === server.id ? "true" : undefined} aria-label={`${server.name}，${server.connection.host}:${server.connection.port}，${ENVIRONMENT_LABEL_SHORT[server.metadata.environment]}，${SERVER_STATUS_LABEL[server.status]}`} title={server.name}>
                  <span className={`server-dot ${ENV_DOT[server.metadata.environment]}`} aria-hidden="true" />
                  <span className="server-row-copy">
                    <span className="server-row-name">{server.name}</span>
                    <span className="server-row-detail">
                      <span className="server-row-host">{server.connection.host}</span>
                      <span className="server-row-port">:{server.connection.port}</span>
                    </span>
                  </span>
                  <span className="server-row-env" aria-hidden="true">{ENVIRONMENT_LABEL_SHORT[server.metadata.environment]}</span>
                </button>
                <div className="server-row-actions">
                  <span className={`server-status-pill server-status-${server.status}`}>{action.isPending && action.variables.serverId === server.id ? "处理中…" : SERVER_STATUS_LABEL[server.status]}</span>
                  {server.status === "connected" ? (
                    <button type="button" title="断开连接" aria-label={`断开 ${server.name}`} disabled={busy} onClick={() => action.mutate({ type: "disconnect", serverId: server.id })}><Icon name="disconnect" size="sm" /></button>
                  ) : (
                    <button type="button" title="连接服务器" aria-label={`连接 ${server.name}`} onClick={() => action.mutate({ type: "connect", serverId: server.id })} disabled={busy || server.status === "connecting"}><Icon name="connect" size="sm" /></button>
                  )}
                  <button type="button" title="编辑服务器" aria-label={`编辑 ${server.name}`} disabled={busy} onClick={() => setEditing(server)}><Icon name="edit" size="sm" /></button>
                  <button type="button" title="删除服务器" aria-label={`删除 ${server.name}`} disabled={busy} onClick={() => { if (window.confirm(`确定删除服务器“${server.name}”？`)) action.mutate({ type: "delete", serverId: server.id }); }}><Icon name="trash" size="sm" /></button>
                </div>
              </div>
            </li>
          ))}
        </ul>
      ) : shell && serverRows.length ? <p className="no-results">没有匹配的服务器</p> : null}

      {adding ? <AddServerModal onClose={() => setAdding(false)} /> : null}
      {editing ? <AddServerModal server={editing} onClose={() => setEditing(undefined)} /> : null}
    </aside>
  );
}
