import { IPC_COMMANDS, type ChatMessage, type ChatSession } from "@yukinal/shared";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";

import { errorMessage, formatTimestamp } from "../../lib/format.js";
import { Icon } from "../../components/Icon.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";

type ArchiveInput = { sessionId: string; archived: boolean };

export function AgentHistoryPane({
  activeSessionId,
  onClose,
  onNewSession,
  onOpenSession,
  onSessionUpdated,
  onSessionDeleted,
}: {
  activeSessionId: string | null;
  onClose: () => void;
  onNewSession: () => void;
  onOpenSession: (session: ChatSession, messages: ChatMessage[]) => void;
  onSessionUpdated: (session: ChatSession) => void;
  onSessionDeleted: (sessionId: string) => void;
}) {
  const shell = isDesktopShell();
  const queryClient = useQueryClient();
  const [query, setQuery] = useState("");
  const [archived, setArchived] = useState(false);
  const [openingId, setOpeningId] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const sessions = useQuery({
    queryKey: ["agent-chat-sessions", query, archived],
    enabled: shell,
    queryFn: async () => (
      await callDesktop(IPC_COMMANDS.chatSessionList, {
        query: query.trim() || undefined,
        archived,
        limit: 50,
      })
    ).sessions,
  });
  const archiveSession = useMutation({
    mutationFn: ({ sessionId, archived: nextArchived }: ArchiveInput) =>
      callDesktop(IPC_COMMANDS.chatSessionArchive, { sessionId, archived: nextArchived }),
    onSuccess: ({ session }) => {
      setActionError(null);
      onSessionUpdated(session);
      void queryClient.invalidateQueries({ queryKey: ["agent-chat-sessions"] });
    },
    onError: (error) => setActionError(`更新归档状态失败：${errorMessage(error)}`),
  });
  const deleteSession = useMutation({
    mutationFn: (sessionId: string) => callDesktop(IPC_COMMANDS.chatSessionDelete, { sessionId }),
    onSuccess: (_response, sessionId) => {
      setActionError(null);
      onSessionDeleted(sessionId);
      void queryClient.invalidateQueries({ queryKey: ["agent-chat-sessions"] });
    },
    onError: (error) => setActionError(`删除对话失败：${errorMessage(error)}`),
  });

  const openSession = async (session: ChatSession): Promise<void> => {
    setOpeningId(session.id);
    setActionError(null);
    try {
      const detail = await callDesktop(IPC_COMMANDS.chatSessionGet, { sessionId: session.id });
      onOpenSession(detail.session, detail.messages);
    } catch (error) {
      setActionError(`打开对话失败：${errorMessage(error)}`);
    } finally {
      setOpeningId(null);
    }
  };

  const busy = archiveSession.isPending || deleteSession.isPending || openingId !== null;

  return (
    <section className="agent-history-pane" aria-label="对话记录">
      <div className="agent-history-heading">
        <div>
          <p className="eyebrow">Agent 工作区</p>
          <h3>对话记录</h3>
        </div>
        <div className="agent-history-heading-actions">
          <button type="button" className="button-secondary agent-history-new" onClick={onNewSession} disabled={busy}>
            <Icon name="plus" size="sm" />新建任务
          </button>
          <button type="button" className="icon-button" aria-label="关闭对话记录" title="关闭对话记录" onClick={onClose}>
            <Icon name="close" size="md" />
          </button>
        </div>
      </div>

      <label className="agent-history-search">
        <Icon name="search" size="sm" />
        <input
          type="search"
          aria-label="搜索对话记录"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="搜索标题或消息内容"
          maxLength={200}
        />
        {query ? <button type="button" aria-label="清空搜索" title="清空搜索" onClick={() => setQuery("")}><Icon name="close" size="xs" /></button> : null}
      </label>

      <div className="agent-history-tabs" role="tablist" aria-label="对话状态">
        <button type="button" role="tab" aria-selected={!archived} className={!archived ? "is-selected" : ""} onClick={() => setArchived(false)}>进行中</button>
        <button type="button" role="tab" aria-selected={archived} className={archived ? "is-selected" : ""} onClick={() => setArchived(true)}>已归档</button>
      </div>

      {actionError ? <div className="agent-history-error" role="alert">{actionError}</div> : null}
      {!shell ? <div className="agent-history-empty"><Icon name="logs" size="xl" /><strong>桌面应用中可查询记录</strong><p>对话会保存到本机数据库，浏览器预览不会读取本地记录。</p></div> : null}
      {shell && sessions.isLoading ? <div className="agent-history-empty"><span className="loading-spinner" /><strong>正在读取记录</strong></div> : null}
      {shell && sessions.isError ? (
        <div className="agent-history-empty"><Icon name="warning" size="xl" /><strong>无法读取对话记录</strong><p>{errorMessage(sessions.error)}</p><button type="button" className="text-button" onClick={() => void sessions.refetch()}>重试</button></div>
      ) : null}
      {shell && !sessions.isLoading && !sessions.isError && (sessions.data?.length ?? 0) === 0 ? (
        <div className="agent-history-empty"><Icon name={query ? "search" : archived ? "logs" : "sparkle"} size="xl" /><strong>{query ? "没有匹配的对话" : archived ? "暂无归档对话" : "暂无进行中的对话"}</strong><p>{query ? "换个关键词试试，标题和消息内容都会被搜索。" : archived ? "归档的任务会集中显示在这里。" : "发送第一个任务后，记录会自动保存在这里。"}</p></div>
      ) : null}
      {shell && !sessions.isLoading && !sessions.isError && sessions.data?.length ? (
        <div className="agent-history-list">
          {sessions.data.map((session) => (
            <article className={`agent-history-item ${activeSessionId === session.id ? "is-active" : ""}`} key={session.id}>
              <button type="button" className="agent-history-item-open" onClick={() => void openSession(session)} disabled={busy}>
                <span className="agent-history-item-top"><strong>{session.title}</strong><time dateTime={session.updatedAt}>{formatTimestamp(session.updatedAt)}</time></span>
                <span className="agent-history-item-preview">{session.lastMessagePreview || "暂无消息"}</span>
                <span className="agent-history-item-meta">{session.messageCount} 条消息{session.serverId ? ` · ${session.serverId}` : " · 全局工作区"}</span>
              </button>
              <div className="agent-history-item-actions">
                <button type="button" className="text-button" disabled={busy} onClick={() => archiveSession.mutate({ sessionId: session.id, archived: !archived })}>{archived ? "恢复" : "归档"}</button>
                <button type="button" className="icon-button agent-history-delete" aria-label={`删除 ${session.title}`} title="删除对话" disabled={busy} onClick={() => { if (window.confirm(`确定删除“${session.title}”？对话消息也会一并删除。`)) deleteSession.mutate(session.id); }}><Icon name="trash" size="sm" /></button>
              </div>
            </article>
          ))}
        </div>
      ) : null}
    </section>
  );
}

/* `formatHistoryTime` 与 `errorMessage` 曾定义在这里，两者都与别处逐字节重复：
   前者等于 `ActivityFeed.tsx` 里的 `formatTimestamp`，后者等于 `useChatSessions.ts`
   里的 `errorText`。现已统一到 `lib/format.js`。 */

