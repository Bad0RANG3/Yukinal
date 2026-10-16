import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { IPC_COMMANDS, type AgentMemory } from "@yukinal/shared";
import { useEffect, useRef, useState } from "react";
import { Icon } from "../../components/Icon.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { errorMessage } from "../../lib/format.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { useWorkpagesStore } from "../workpages/workpages-store.js";
import "./memory-library.css";

export function MemoryLibrary({ onClose }: { onClose: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const element = dialog.current;
    const previous = document.activeElement;
    element?.showModal();
    return () => { element?.close(); if (previous instanceof HTMLElement) previous.focus(); };
  }, []);
  const serverId = useWorkspaceStore((state) => state.selectedServerId);
  const busy = useWorkspaceStore((state) => state.agentBusy);
  const [scope, setScope] = useState<"global" | "server">(serverId ? "server" : "global");
  const [query, setQuery] = useState("");
  const [editing, setEditing] = useState<AgentMemory | "new" | null>(null);
  const [title, setTitle] = useState("");
  const [content, setContent] = useState("");
  const [notice, setNotice] = useState("");
  const client = useQueryClient();
  const memoryScope = { scope, ...(scope === "server" && serverId ? { serverId } : {}) };
  const memories = useQuery({ queryKey: ["agent-memories", scope, serverId, query], enabled: isDesktopShell(),
    queryFn: async () => (await callDesktop(IPC_COMMANDS.agentMemoryList, { ...memoryScope, query, limit: 100 })).memories });
  const refresh = () => client.invalidateQueries({ queryKey: ["agent-memories"] });
  const save = useMutation({ mutationFn: () => callDesktop(IPC_COMMANDS.agentMemorySave, {
    ...memoryScope, ...(editing && editing !== "new" ? { id: editing.id } : {}), title: title.trim(), content: content.trim(),
  }), onSuccess: () => { setEditing(null); void refresh(); } });
  const remove = useMutation({ mutationFn: (memory: AgentMemory) => callDesktop(IPC_COMMANDS.agentMemoryDelete, {
    id: memory.id, scope: memory.scope, ...(memory.serverId ? { serverId: memory.serverId } : {}),
  }), onSuccess: refresh });
  const reset = useMutation({ mutationFn: () => callDesktop(IPC_COMMANDS.agentContextResetAll, {}), onSuccess: (result) => {
    const state = useWorkspaceStore.getState();
    state.resetAgentContextView();
    state.setAgentDraft(""); state.setAgentAttachments([]); state.setAgentRoleId(null); state.selectServer(null);
    for (const page of useWorkpagesStore.getState().pages) useWorkpagesStore.getState().close(page.id);
    setNotice(`已重置 ${result.count} 个上下文，历史和记忆已保留`);
    void client.invalidateQueries({ queryKey: ["agent-chat-sessions"] });
  } });
  const edit = (memory: AgentMemory | "new") => {
    setEditing(memory); setTitle(memory === "new" ? "" : memory.title); setContent(memory === "new" ? "" : memory.content);
    save.reset();
  };
  const error = memories.error ?? save.error ?? remove.error ?? reset.error;
  return <dialog ref={dialog} aria-label="记忆库" className="memory-library" onCancel={(event) => { event.preventDefault(); onClose(); }}>
      <header><h2>记忆库</h2><button autoFocus type="button" className="icon-button" aria-label="关闭记忆库" onClick={onClose}><Icon name="close" /></button></header>
      <div className="memory-toolbar"><div role="group" aria-label="记忆范围">
        <button type="button" aria-pressed={scope === "global"} onClick={() => { setScope("global"); setEditing(null); }}>全局</button>
        <button type="button" disabled={!serverId} aria-pressed={scope === "server"} onClick={() => { setScope("server"); setEditing(null); }}>当前服务器</button>
      </div><button type="button" className="text-button" onClick={() => edit("new")}>添加记忆</button></div>
      <input aria-label="搜索记忆" placeholder="搜索记忆" value={query} onChange={(event) => setQuery(event.target.value)} />
      {error ? <p role="alert" className="inline-error">{errorMessage(error)}</p> : null}
      <div className="memory-content">
        {editing ? <form onSubmit={(event) => { event.preventDefault(); save.mutate(); }}>
          <label>标题<input required maxLength={200} value={title} onChange={(event) => setTitle(event.target.value)} /></label>
          <label>内容<textarea required rows={8} maxLength={12000} value={content} onChange={(event) => setContent(event.target.value)} /></label>
          <div className="memory-actions"><button type="button" className="text-button" onClick={() => setEditing(null)}>取消</button><button type="submit" className="button-secondary" disabled={!title.trim() || !content.trim() || save.isPending}>保存</button></div>
        </form> : memories.isLoading ? <p role="status">读取中…</p> : <ul>{memories.data?.map((memory) => <li key={memory.id}>
          <button type="button" className="memory-record" onClick={() => edit(memory)}><strong>{memory.title}</strong><span>{memory.content}</span></button>
          <button type="button" className="icon-button" aria-label={`删除记忆 ${memory.title}`} disabled={remove.isPending} onClick={() => {
            if (window.confirm(`删除记忆“${memory.title}”？`)) remove.mutate(memory);
          }}><Icon name="trash" size="sm" /></button>
        </li>)}{!memories.data?.length ? <li className="compact-empty">暂无记忆</li> : null}</ul>}
      </div>
      <footer><button type="button" className="text-button text-button-danger" disabled={busy || reset.isPending} onClick={() => {
        if (window.confirm("重置全部上下文和当前草稿？对话历史、记忆、服务器和连接配置会保留。")) reset.mutate();
      }}>重置全部上下文</button>{notice ? <span role="status">{notice}</span> : null}</footer>
  </dialog>;
}
