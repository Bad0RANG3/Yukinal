import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { IPC_COMMANDS, type Environment, type Server } from "@yukinal/shared";
import { useEffect, useMemo, useState } from "react";

import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { AddServerModal } from "./AddServerModal.js";

const ENV_LABEL: Record<Environment, string> = { production: "生产", staging: "预发", development: "开发", local: "本地", unknown: "未知" };
const ENV_DOT: Record<Environment, string> = { production: "server-dot-production", staging: "server-dot-staging", development: "server-dot-development", local: "server-dot-development", unknown: "server-dot-unknown" };

export function ServerList() {
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const selectServer = useWorkspaceStore((state) => state.selectServer);
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<Server | undefined>();
  const [filter, setFilter] = useState("");
  const shell = isDesktopShell();
  const queryClient = useQueryClient();
  const servers = useQuery({ queryKey: ["servers"], enabled: shell, staleTime: 10_000, refetchInterval: 60_000, queryFn: async () => (await callDesktop(IPC_COMMANDS.serverList, {})).servers });
  const refresh = () => { void queryClient.invalidateQueries({ queryKey: ["servers"] }); };
  const connect = useMutation({ mutationFn: (serverId: string) => callDesktop(IPC_COMMANDS.serverConnect, { serverId }), onSuccess: refresh });
  const disconnect = useMutation({ mutationFn: (serverId: string) => callDesktop(IPC_COMMANDS.serverDisconnect, { serverId }), onSuccess: refresh });
  const remove = useMutation({ mutationFn: (serverId: string) => callDesktop(IPC_COMMANDS.serverDelete, { serverId }), onSuccess: (_, serverId) => { if (selectedServerId === serverId) selectServer(null); refresh(); } });
  const serverRows = servers.data ?? [];
  const filteredServers = useMemo(() => { const needle = filter.trim().toLowerCase(); return needle ? serverRows.filter((server) => [server.name, server.connection.host, server.metadata.environment, server.metadata.region].filter((value): value is string => Boolean(value)).some((value) => value.toLowerCase().includes(needle))) : serverRows; }, [filter, serverRows]);
  useEffect(() => { if (!servers.isSuccess) return; const first = serverRows[0]; if (!first) { if (selectedServerId) selectServer(null); return; } if (!selectedServerId || !serverRows.some((server) => server.id === selectedServerId)) selectServer(first.id); }, [selectServer, selectedServerId, serverRows, servers.isSuccess]);

  return <aside className="server-sidebar">
    <div className="server-sidebar-header"><div><p className="eyebrow">工作区</p><h2>服务器</h2></div><button type="button" className="icon-button icon-button-accent" aria-label="添加服务器" title="添加服务器" onClick={() => setAdding(true)}>+</button></div>
    <label className="server-search"><span aria-hidden="true">⌕</span><input value={filter} onChange={(event) => setFilter(event.target.value)} placeholder="搜索名称或主机" aria-label="搜索服务器" />{filter ? <button type="button" className="search-clear" aria-label="清除搜索" onClick={() => setFilter("")}>×</button> : null}</label>
    <div className="server-list-meta"><span>{serverRows.length} 台服务器</span><button type="button" className="text-button" onClick={() => void servers.refetch()} disabled={servers.isFetching}>↻</button></div>
    {servers.isError ? <div className="inline-error"><strong>读取失败</strong><span>{servers.error instanceof Error ? servers.error.message : String(servers.error)}</span><button type="button" className="text-button text-button-danger" onClick={() => void servers.refetch()}>重试</button></div> : null}
    {!shell ? <div className="empty-state compact-empty"><p>浏览器预览模式</p><small>启动 Tauri 后加载本地服务器。</small></div> : null}
    {shell && !serverRows.length && !servers.isLoading ? <div className="empty-state compact-empty"><p>还没有服务器</p><button type="button" className="secondary-button" onClick={() => setAdding(true)}>添加服务器</button></div> : null}
    {servers.isLoading ? <div className="skeleton-list" aria-label="正在加载服务器" /> : null}
    {filteredServers.length ? <ul className="server-list">{filteredServers.map((server) => <li key={server.id} className="server-list-item"><div className={`server-row ${selectedServerId === server.id ? "server-row-selected" : ""}`}><button type="button" onClick={() => selectServer(server.id)} className="server-row-main"><span className={`server-dot ${ENV_DOT[server.metadata.environment]}`} /><span className="server-row-copy"><span className="server-row-name">{server.name}</span><span className="server-row-detail">{server.connection.host}:{server.connection.port}</span></span><span className="server-row-env">{ENV_LABEL[server.metadata.environment]}</span></button><div className="server-row-actions"><span className={`server-status-pill server-status-${server.status}`}>{server.status}</span>{server.status === "connected" ? <button type="button" title="断开" aria-label={`断开 ${server.name}`} onClick={() => disconnect.mutate(server.id)}>⏏</button> : <button type="button" title="连接" aria-label={`连接 ${server.name}`} onClick={() => connect.mutate(server.id)} disabled={server.status === "connecting"}>↗</button>}<button type="button" title="编辑" aria-label={`编辑 ${server.name}`} onClick={() => setEditing(server)}>✎</button><button type="button" title="删除" aria-label={`删除 ${server.name}`} onClick={() => { if (window.confirm(`删除服务器 ${server.name}？`)) remove.mutate(server.id); }}>×</button></div></div></li>)}</ul> : shell && serverRows.length ? <p className="no-results">没有匹配的服务器</p> : null}
    {adding ? <AddServerModal onClose={() => setAdding(false)} /> : null}{editing ? <AddServerModal server={editing} onClose={() => setEditing(undefined)} /> : null}
  </aside>;
}
