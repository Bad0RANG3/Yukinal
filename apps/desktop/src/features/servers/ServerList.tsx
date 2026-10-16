import { type Server } from "@yukinal/shared";
import { useEffect, useMemo, useState } from "react";

import { errorMessage } from "../../lib/format.js";
import { isDesktopShell } from "../../lib/ipc.js";
import { useServerAction, useServers } from "../../lib/servers.js";
import { isPreviewFixture } from "../../lib/preview-fixtures.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { Icon } from "../../components/Icon.js";
import { AddServerModal } from "./AddServerModal.js";
import { ServerTreeItem } from "./ServerTreeItem.js";

export function ServerList({ onClose, onSelect, embedded = false }: { onClose?: () => void; onSelect?: () => void; embedded?: boolean }) {
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const selectServer = useWorkspaceStore((state) => state.selectServer);
  const syncServerSelection = useWorkspaceStore((state) => state.syncServerSelection);
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<Server | undefined>();
  const [filter, setFilter] = useState("");
  const shell = isDesktopShell();
  const preview = isPreviewFixture();
  const protectedInput = useWorkspaceStore((state) => state.agentBusy || Boolean(state.agentDraft.trim()) || state.agentAttachments.length > 0);
  const agentBusy = useWorkspaceStore((state) => state.agentBusy);
  const servers = useServers({ poll: true });
  const { action, busy } = useServerAction();
  const serverRows = servers.data ?? [];
  const filteredServers = useMemo(() => { const needle = filter.trim().toLowerCase(); return needle ? serverRows.filter((server) => [server.name, server.connection.host, server.metadata.environment, server.metadata.region].filter((value): value is string => Boolean(value)).some((value) => value.toLowerCase().includes(needle))) : serverRows; }, [filter, serverRows]);
  useEffect(() => {
    if (servers.data) syncServerSelection(servers.data.map((server) => server.id));
  }, [syncServerSelection, servers.data]);

  return (
    <aside id="server-sidebar" className={`server-sidebar ${embedded ? "server-sidebar-embedded" : ""}`} aria-label="服务器列表">
      <div className="server-sidebar-header">
        <div className="server-sidebar-heading">{!embedded ? <p className="eyebrow">工作区</p> : null}<h2>服务器</h2>{embedded && servers.isSuccess ? <span className="server-sidebar-count" aria-label={`${serverRows.length} 台服务器`}>{serverRows.length}</span> : null}</div>
        <div className="server-sidebar-header-actions">
          {onClose ? <button type="button" className="icon-button server-sidebar-close" aria-label="关闭服务器列表" title="关闭服务器列表" onClick={onClose}><Icon name="close" size="md" /></button> : null}
          {embedded ? <button type="button" className="icon-button" onClick={() => void servers.refetch()} disabled={!shell || servers.isFetching} aria-label="刷新服务器列表" title="刷新服务器列表"><Icon name="refresh" size="sm" /></button> : null}
          <button type="button" className="icon-button icon-button-accent" aria-label="添加服务器" title={preview ? "退出模拟后可添加服务器" : "添加服务器"} disabled={preview} onClick={() => setAdding(true)}>
            <Icon name="plus" size="md" />
          </button>
        </div>
      </div>

      {import.meta.env.DEV ? <div className="server-preview-control"><span>{preview ? "模拟数据" : ""}</span><button type="button" className="text-button" disabled={protectedInput} title={protectedInput ? "先完成或清空当前草稿，再切换数据" : preview ? "退出模拟，恢复真实数据" : "加载 6 台模拟服务器"} onClick={() => {
        if (protectedInput) return;
        const url = new URL(window.location.href);
        if (preview) url.searchParams.delete("fixture");
        else url.searchParams.set("fixture", "1");
        window.location.assign(url.href);
      }}>{preview ? "退出模拟" : "加载模拟数据"}</button></div> : null}

      <div className="server-search">
        <Icon name="search" size="sm" />
        <input value={filter} onChange={(event) => setFilter(event.target.value)} placeholder="搜索名称或主机" aria-label="搜索服务器" />
        {filter ? <button type="button" className="search-clear" aria-label="清除搜索" onClick={() => setFilter("")}><Icon name="close" size="sm" /></button> : null}
      </div>

      {!embedded || filter.trim() ? <div className="server-list-meta">
        <span>{filter.trim() ? `${filteredServers.length} / ${serverRows.length}` : serverRows.length} 台服务器</span>
        {!embedded ? <button type="button" className="text-button icon-text-button" onClick={() => void servers.refetch()} disabled={!shell || servers.isFetching} aria-label="刷新服务器列表" title="刷新服务器列表">
          <Icon name="refresh" size="sm" />
        </button> : null}
      </div> : null}

      {servers.isError ? <div className="inline-error"><strong>读取失败</strong><span>{errorMessage(servers.error)}</span><button type="button" className="text-button text-button-danger" onClick={() => void servers.refetch()}>重试</button></div> : null}
      {action.isError ? <div className="inline-error" role="alert"><strong>操作未完成</strong><span>{action.error.message}</span><button type="button" className="text-button" onClick={() => action.reset()}>关闭提示</button></div> : null}
      {!shell ? <div className="empty-state compact-empty"><p>浏览器预览模式</p></div> : null}
      {shell && !serverRows.length && servers.isSuccess ? <div className="empty-state compact-empty server-list-empty"><span className="server-empty-mark"><Icon name="servers" size="xl" /></span><p>暂无服务器</p><button type="button" className="button-secondary" onClick={() => setAdding(true)}><Icon name="plus" size="sm" />添加服务器</button></div> : null}
      {servers.isLoading ? <div className="skeleton-list" aria-label="正在加载服务器" /> : null}
      {filteredServers.length ? (
        <ul className="server-list">
          {filteredServers.map((server) => (
            <ServerTreeItem key={server.id} server={server} selected={selectedServerId === server.id} disabled={agentBusy && selectedServerId !== server.id} onSelect={() => { selectServer(server.id); onSelect?.(); }}>
              {server.status === "connected" ? (
                <button type="button" title="断开连接" aria-label={`断开 ${server.name}`} disabled={busy} onClick={() => action.mutate({ type: "disconnect", serverId: server.id })}><Icon name="disconnect" size="sm" /></button>
              ) : (
                <button type="button" title="连接服务器" aria-label={`连接 ${server.name}`} onClick={() => action.mutate({ type: "connect", serverId: server.id })} disabled={busy || server.status === "connecting"}><Icon name="connect" size="sm" /></button>
              )}
              <button type="button" title={preview ? "退出模拟后可编辑服务器" : "编辑服务器"} aria-label={`编辑 ${server.name}`} disabled={busy || preview} onClick={() => setEditing(server)}><Icon name="edit" size="sm" /></button>
              <button type="button" title="删除服务器" aria-label={`删除 ${server.name}`} disabled={busy} onClick={() => { if (window.confirm(`确定删除服务器“${server.name}”？`)) action.mutate({ type: "delete", serverId: server.id }); }}><Icon name="trash" size="sm" /></button>
            </ServerTreeItem>
          ))}
        </ul>
      ) : shell && serverRows.length ? <p className="no-results">没有匹配的服务器</p> : null}

      {adding ? <AddServerModal onClose={() => setAdding(false)} /> : null}
      {editing ? <AddServerModal server={editing} onClose={() => setEditing(undefined)} /> : null}
    </aside>
  );
}
