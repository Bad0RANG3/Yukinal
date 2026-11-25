import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { IPC_COMMANDS, type AgentMemory } from "@yukinal/shared";
import { useEffect, useRef, useState, type ChangeEvent } from "react";
import { Icon } from "../../components/Icon.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { errorMessage } from "../../lib/format.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { useWorkpagesStore } from "../workpages/workpages-store.js";
import "./memory-library.css";

const MEMORY_CONTENT_LIMIT = 12_000;
const MEMORY_TITLE_LIMIT = 200;
const KNOWLEDGE_FILE_PATTERN = /\.(?:md|markdown|txt)$/i;
const FRONT_MATTER_DELIMITER = /^(?:\+\+\+|---)[ \t]*$/;

export interface MemoryDraft {
  title: string;
  content: string;
}

function plainMemoryTitle(value: string): string {
  return value.replace(/[\u0000-\u001f\u007f]/g, " ").replace(/\s+/g, " ").trim();
}

function clipMemoryTitle(value: string): string {
  if (value.length <= MEMORY_TITLE_LIMIT) return value;
  let clipped = "";
  for (const character of value) {
    if (clipped.length + character.length > MEMORY_TITLE_LIMIT) break;
    clipped += character;
  }
  return clipped;
}

function readMemoryLine(text: string, start: number): { value: string; next: number } | null {
  if (start >= text.length) return null;
  let end = start;
  while (end < text.length && text[end] !== "\r" && text[end] !== "\n") end += 1;
  let next = end;
  if (text[next] === "\r") next += 1;
  if (text[next] === "\n") next += 1;
  return { value: text.slice(start, end), next };
}

export function hasMemoryFrontMatter(text: string): boolean {
  const start = text.startsWith("\uFEFF") ? 1 : 0;
  const opening = readMemoryLine(text, start);
  return Boolean(opening && FRONT_MATTER_DELIMITER.test(opening.value));
}

function firstMemoryHeading(content: string): string {
  let fence: { marker: string; length: number } | null = null;
  for (const line of content.split(/\r\n|\n|\r/)) {
    const fenceLine = /^ {0,3}(`{3,}|~{3,})(.*)$/.exec(line);
    if (fence) {
      if (fenceLine && fenceLine[1]![0] === fence.marker && fenceLine[1]!.length >= fence.length
        && /^[ \t]*$/.test(fenceLine[2]!)) fence = null;
      continue;
    }
    if (fenceLine && !(fenceLine[1]![0] === "`" && fenceLine[2]!.includes("`"))) {
      fence = { marker: fenceLine[1]![0]!, length: fenceLine[1]!.length };
      continue;
    }
    const heading = /^ {0,3}#(?:[ \t]+|$)(.*)$/.exec(line);
    if (heading) return heading[1]!.replace(/[ \t]+#+[ \t]*$/, "");
  }
  return "";
}

export function prepareMemoryDraft(fileName: string, text: string): MemoryDraft | null {
  if (hasMemoryFrontMatter(text)) return null;
  const content = text.trim();
  if (!content || content.length > MEMORY_CONTENT_LIMIT) return null;

  const baseName = fileName.split(/[\\/]/).pop() ?? "";
  const fileTitle = baseName.replace(KNOWLEDGE_FILE_PATTERN, "");
  const heading = firstMemoryHeading(content);
  const title = plainMemoryTitle(heading) || plainMemoryTitle(fileTitle) || "未命名知识";

  return {
    title: clipMemoryTitle(title),
    content,
  };
}

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
  const [importing, setImporting] = useState(false);
  // 删除与重置都用行内确认条，而不是 `window.confirm`：系统弹窗会盖住上下文、冻结
  // WebView2 的动画与事件，也是这套界面里唯一一个不属于它的对话框（同
  // `AgentHistoryDeleteConfirm` 与 `providers.css` 里已经写下的规矩）。
  // 一次只确认一件事：点开第二件会替换掉第一件，而不是叠两个确认条。
  const [confirming, setConfirming] = useState<{ kind: "delete"; memory: AgentMemory } | { kind: "reset" } | null>(null);
  const confirmCancelRef = useRef<HTMLButtonElement>(null);
  const editorInput = useRef<HTMLInputElement>(null);
  const importInput = useRef<HTMLInputElement>(null);
  const importingRef = useRef(false);
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
  async function importKnowledge(files: readonly File[]) {
    if (importingRef.current || files.length === 0) return;
    importingRef.current = true;
    const importScope = { ...memoryScope };
    let succeeded = 0;
    let skipped = 0;
    let metadataSkipped = 0;
    let failed = 0;
    setImporting(true);
    setNotice(`正在导入 0/${files.length}…`);
    try {
      for (const [index, file] of files.entries()) {
        if (!KNOWLEDGE_FILE_PATTERN.test(file.name)) {
          skipped += 1;
        } else {
          try {
            const text = await file.text();
            if (hasMemoryFrontMatter(text)) {
              metadataSkipped += 1;
              skipped += 1;
            } else {
              const draft = prepareMemoryDraft(file.name, text);
              if (!draft) {
                skipped += 1;
              } else {
                await callDesktop(IPC_COMMANDS.agentMemorySave, { ...importScope, ...draft });
                succeeded += 1;
              }
            }
          } catch {
            failed += 1;
          }
        }
        setNotice(`正在导入 ${index + 1}/${files.length}…`);
      }
    } finally {
      importingRef.current = false;
      setImporting(false);
      const metadataNotice = metadataSkipped > 0
        ? `（其中 ${metadataSkipped} 个含 ACL/环境前置元数据，请改用 yukinal-knowledge CLI）`
        : "";
      setNotice(`导入完成：成功 ${succeeded}，跳过 ${skipped}${metadataNotice}，失败 ${failed}`);
    }
    void refresh();
  }
  const selectKnowledge = (event: ChangeEvent<HTMLInputElement>) => {
    const input = event.currentTarget;
    const files = Array.from(input.files ?? []);
    input.value = "";
    if (files.length) void importKnowledge(files);
  };
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
      </div><div className="memory-toolbar-actions">
        <input ref={importInput} data-memory-import-input type="file" aria-label="选择知识文件" accept=".md,.markdown,.txt" multiple hidden onChange={selectKnowledge} />
        <button data-memory-import type="button" className="text-button" aria-disabled={importing || undefined} aria-busy={importing || undefined} title="导入 Markdown 或 TXT；空文、超过 12000 字符或含 ACL/环境前置元数据的文件会跳过" onClick={() => {
          if (!importing) importInput.current?.click();
        }}>{importing ? "导入中…" : "导入知识"}</button>
        <button data-memory-add type="button" className="text-button" onClick={() => edit("new")}>添加记忆</button>
      </div></div>
      <input aria-label="搜索记忆" placeholder="搜索记忆" value={query} onChange={(event) => setQuery(event.target.value)} />
      {error ? <p role="alert" className="inline-error">{errorMessage(error)}</p> : null}
      <div className="memory-content">
        {editing ? <form onSubmit={(event) => { event.preventDefault(); save.mutate(); }}>
          <label>标题<input ref={editorInput} required maxLength={MEMORY_TITLE_LIMIT} value={title} onChange={(event) => setTitle(event.target.value)} /></label>
          <label>内容<textarea required rows={8} maxLength={MEMORY_CONTENT_LIMIT} value={content} onChange={(event) => setContent(event.target.value)} /></label>
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
