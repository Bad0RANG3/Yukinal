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
  // 删除与重置都用行内确认条，而不是 `window.confirm`：系统弹窗会盖住上下文、冻结
  // WebView2 的动画与事件，也是这套界面里唯一一个不属于它的对话框（同
  // `AgentHistoryDeleteConfirm` 与 `providers.css` 里已经写下的规矩）。
  // 一次只确认一件事：点开第二件会替换掉第一件，而不是叠两个确认条。
  const [confirming, setConfirming] = useState<{ kind: "delete"; memory: AgentMemory } | { kind: "reset" } | null>(null);
  const confirmCancelRef = useRef<HTMLButtonElement>(null);
  const editorInput = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (!confirming) return;
    requestAnimationFrame(() => confirmCancelRef.current?.focus());
  }, [confirming]);
  const editorOrigin = useRef<string>("new");
  function finishEdit() {
    setEditing(null);
    requestAnimationFrame(() => {
      const records = Array.from(dialog.current?.querySelectorAll<HTMLButtonElement>("[data-memory-id]") ?? []);
      (records.find((button) => button.dataset.memoryId === editorOrigin.current) ?? dialog.current?.querySelector<HTMLButtonElement>("[data-memory-add]"))?.focus();
    });
  }
  const client = useQueryClient();
  const memoryScope = { scope, ...(scope === "server" && serverId ? { serverId } : {}) };
  const memories = useQuery({ queryKey: ["agent-memories", scope, serverId, query], enabled: isDesktopShell(),
    queryFn: async () => (await callDesktop(IPC_COMMANDS.agentMemoryList, { ...memoryScope, query, limit: 100 })).memories });
  const refresh = () => client.invalidateQueries({ queryKey: ["agent-memories"] });
  const save = useMutation({ mutationFn: () => callDesktop(IPC_COMMANDS.agentMemorySave, {
    ...memoryScope, ...(editing && editing !== "new" ? { id: editing.id } : {}), title: title.trim(), content: content.trim(),
  }), onSuccess: () => { finishEdit(); void refresh(); } });
  const remove = useMutation({ mutationFn: (memory: AgentMemory) => callDesktop(IPC_COMMANDS.agentMemoryDelete, {
    id: memory.id, scope: memory.scope, ...(memory.serverId ? { serverId: memory.serverId } : {}),
  }), onSuccess: () => { setConfirming(null); void refresh(); } });
  const reset = useMutation({ mutationFn: () => callDesktop(IPC_COMMANDS.agentContextResetAll, {}), onSuccess: (result) => {
    const state = useWorkspaceStore.getState();
    state.resetAgentContextView();
    state.setAgentDraft(""); state.setAgentAttachments([]); state.selectServer(null);
    for (const page of useWorkpagesStore.getState().pages) useWorkpagesStore.getState().close(page.id);
    setConfirming(null);
    setNotice(`已重置 ${result.count} 个上下文，历史和记忆已保留`);
    void client.invalidateQueries({ queryKey: ["agent-chat-sessions"] });
  } });
  const edit = (memory: AgentMemory | "new") => {
    editorOrigin.current = memory === "new" ? "new" : memory.id;
    setEditing(memory); setTitle(memory === "new" ? "" : memory.title); setContent(memory === "new" ? "" : memory.content);
    save.reset();
    requestAnimationFrame(() => editorInput.current?.focus());
  };
  const error = memories.error ?? save.error ?? remove.error ?? reset.error;
  return <dialog ref={dialog} aria-label="记忆库" className="memory-library" onCancel={(event) => { event.preventDefault(); onClose(); }}>
      <header><h2>记忆库</h2><button autoFocus type="button" className="icon-button" aria-label="关闭记忆库" onClick={onClose}><Icon name="close" /></button></header>
      <div className="memory-toolbar"><div role="group" aria-label="记忆范围">
        <button type="button" aria-pressed={scope === "global"} onClick={() => { setScope("global"); setEditing(null); setConfirming(null); }}>全局</button>
        <button type="button" disabled={!serverId} aria-pressed={scope === "server"} onClick={() => { setScope("server"); setEditing(null); setConfirming(null); }}>当前服务器</button>
      </div><button data-memory-add type="button" className="text-button" onClick={() => edit("new")}>添加记忆</button></div>
      <input aria-label="搜索记忆" placeholder="搜索记忆" value={query} onChange={(event) => setQuery(event.target.value)} />
      {error ? <p role="alert" className="inline-error">{errorMessage(error)}</p> : null}
      <div className="memory-content">
        {editing ? <form onSubmit={(event) => { event.preventDefault(); save.mutate(); }}>
          <label>标题<input ref={editorInput} required maxLength={200} value={title} onChange={(event) => setTitle(event.target.value)} /></label>
          <label>内容<textarea required rows={8} maxLength={12000} value={content} onChange={(event) => setContent(event.target.value)} /></label>
          <div className="memory-actions"><button type="button" className="text-button" onClick={finishEdit}>取消</button><button type="submit" className="button-secondary" disabled={!title.trim() || !content.trim() || save.isPending}>保存</button></div>
        </form> : memories.isLoading ? <p role="status">读取中…</p> : <ul>{memories.data?.map((memory) => <li key={memory.id}>
          <button data-memory-id={memory.id} type="button" className="memory-record" onClick={() => edit(memory)}><strong>{memory.title}</strong><span>{memory.content}</span></button>
          <button type="button" className="icon-button" aria-label={`删除记忆 ${memory.title}`} aria-disabled={remove.isPending || undefined} onClick={() => {
            if (!remove.isPending) setConfirming({ kind: "delete", memory });
          }}><Icon name="trash" size="sm" /></button>
        </li>)}{memories.isSuccess && memories.data.length === 0 ? <li className="compact-empty">暂无记忆</li> : null}</ul>}
      </div>
      {confirming?.kind === "delete" ? <div className="memory-confirm" role="group" aria-label="确认删除记忆">
        <p>删除记忆「{confirming.memory.title}」？删除后无法撤销。</p>
        <div className="memory-confirm-actions">
          <button ref={confirmCancelRef} type="button" className="text-button" onClick={() => setConfirming(null)}>取消</button>
          <button type="button" className="button-secondary memory-confirm-danger" aria-disabled={remove.isPending || undefined} onClick={() => { if (!remove.isPending) remove.mutate(confirming.memory); }}>{remove.isPending ? "删除中…" : "删除"}</button>
        </div>
      </div> : null}
      {confirming?.kind === "reset" ? <div className="memory-confirm" role="group" aria-label="确认重置上下文">
        <p>重置全部上下文和当前草稿？对话历史、记忆、服务器和连接配置会保留。</p>
        <div className="memory-confirm-actions">
          <button ref={confirmCancelRef} type="button" className="text-button" onClick={() => setConfirming(null)}>取消</button>
          <button type="button" className="button-secondary memory-confirm-danger" aria-disabled={busy || reset.isPending || undefined} onClick={() => { if (!busy && !reset.isPending) reset.mutate(); }}>{reset.isPending ? "重置中…" : "重置"}</button>
        </div>
      </div> : null}
      <footer><button type="button" className="text-button text-button-danger" aria-disabled={busy || reset.isPending || undefined} title={busy ? "对话正在运行，结束后再重置" : undefined} onClick={() => {
        if (!busy && !reset.isPending) setConfirming({ kind: "reset" });
      }}>重置全部上下文</button>{notice ? <span role="status">{notice}</span> : null}</footer>
  </dialog>;
}
