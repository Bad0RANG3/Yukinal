/**
 * 对话记录 —— Agent 面板内的「记录」视图。
 *
 * 这个视图回答的是「我之前让 Agent 做过什么，结论在哪一段里」，而不是「现在在发生
 * 什么」：正在进行的事件流属于 `AgentFeed`，这里只读本机数据库里的会话。它在同一个
 * 面板里替换掉动态，因此切换记录不会打断工作区的上下文。
 *
 * 几条设计约束：
 *
 * - **标题是自动取的，所以必须能改。** 会话标题来自首条提示词（`sessionTitleFromPrompt`），
 *   它经常是「排查一下」这种事后没有信息量的名字。行内重命名是这里唯一的内容编辑，
 *   而且它**不动 `updated_at`** —— 改名是给对话贴标签，不是一次活动（见
 *   `crates/database/src/repositories/chat.rs` 的 `rename`）。
 * - **搜索与筛选都在宿主侧做。** 关键字同时匹配标题与消息正文，计数也按同一个关键字
 *   统计（`counts`），否则「已归档 3」说的只是当前这一页里的 3 条。
 * - **列表是有头的。** 一页 50 条，`载入更多` 用 offset 往下翻，而不是假装记录只有
 *   第一页那么多。
 * - **删除要二次确认，但不弹窗。** `window.confirm` 会挡住整个窗口，还是唯一一个不
 *   属于这套界面的对话框；删除确认因此落在行内，并且默认焦点给「取消」。
 *
 * 这个文件是**容器**：状态、数据获取、副作用、键盘游标都在这里。绘制拆在同目录的兄弟
 * 文件里 —— `AgentHistoryToolbar`（搜索与筛选）、`AgentHistoryRow`（一行）、
 * `AgentHistoryGroupHeader`（时间分组标题）、`AgentHistoryRow` 再组合
 * `AgentHistoryRenameForm` 与 `AgentHistoryDeleteConfirm`。纯逻辑（分组、命中切段、
 * 目标名）在 `history.ts`。
 */

import { IPC_COMMANDS, type ChatMessage, type ChatSession } from "@yukinal/shared";
import { keepPreviousData, useInfiniteQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent, type RefObject } from "react";

import { Icon } from "../../components/Icon.js";
import { errorMessage } from "../../lib/format.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { useServers } from "../../lib/servers.js";
import { AgentHistoryGroupHeader } from "./AgentHistoryGroupHeader.js";
import { AgentHistoryRow } from "./AgentHistoryRow.js";
import { AgentHistoryToolbar } from "./AgentHistoryToolbar.js";
import {
  groupSessionsByDay,
  historyFilterParam,
  historyFilterTotal,
  type HistoryFilter,
} from "./history.js";

/** 一页 50 条：与 `chat_session_list` 的默认值一致，也远小于它的上限 100。 */
const PAGE_SIZE = 50;
/** 相对时间每分钟重算一次，否则「刚刚」会在开着面板的十分钟里一直说「刚刚」。 */
const CLOCK_INTERVAL_MS = 60_000;

type ArchiveInput = { sessionId: string; archived: boolean };
type RenameInput = { sessionId: string; title: string };

export function AgentHistoryPane({
  activeSessionId,
  restoreFocusRef,
  onClose,
  onNewSession,
  onOpenSession,
  onSessionUpdated,
  onSessionDeleted,
}: {
  activeSessionId: string | null;
  /** 面板头部那个「对话记录」按钮。关闭时把焦点还回去，键盘用户不会掉到 body 上。 */
  restoreFocusRef?: RefObject<HTMLButtonElement | null>;
  onClose: () => void;
  onNewSession: () => void;
  onOpenSession: (session: ChatSession, messages: ChatMessage[]) => void;
  onSessionUpdated: (session: ChatSession) => void;
  onSessionDeleted: (sessionId: string) => void;
}) {
  const shell = isDesktopShell();
  const queryClient = useQueryClient();
  const servers = useServers();
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<HistoryFilter>("active");
  const [actionError, setActionError] = useState<string | null>(null);
  const [openingId, setOpeningId] = useState<string | null>(null);
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const [confirmingId, setConfirmingId] = useState<string | null>(null);
  const [focusIndex, setFocusIndex] = useState(0);
  /**
   * 「渲染完之后把焦点放回第 N 行」。
   *
   * 不能在事件处理里直接 focus：取消改名/取消删除时，那一行的按钮此刻还没回到 DOM
   * （行内容被输入框或确认条替换了），直接找只会focus 到别的行。等到状态落地、行重新
   * 挂载之后再执行，才是用户预期的「焦点回到我刚才那一行」。
   */
  const [focusRequest, setFocusRequest] = useState<number | null>(null);
  const [clock, setClock] = useState(() => Date.now());
  const listRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const renameRef = useRef<HTMLInputElement>(null);
  const cancelDeleteRef = useRef<HTMLButtonElement>(null);
  /**
   * 行号表的镜像。异步回调（改名成功后）要按 id 找到那一行，但它注册时 `rowIndex`
   * 还没算出来，闭包里的值会永远停在首次渲染的那一份。
   */
  const rowIndexRef = useRef<Map<string, number>>(new Map());

  useEffect(() => {
    const timer = window.setInterval(() => setClock(Date.now()), CLOCK_INTERVAL_MS);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    if (renamingId) renameRef.current?.select();
  }, [renamingId]);

  useEffect(() => {
    if (confirmingId) cancelDeleteRef.current?.focus();
  }, [confirmingId]);

  /**
   * 关闭（或换一段对话）时把焦点交还面板头部的入口按钮。
   *
   * 只在卸载时做：这个视图是「面板里的另一面」，进去时焦点留在触发它的按钮上，
   * 出来时若不管，焦点就掉到 `body`，键盘用户要重新 Tab 整个面板才能回到原处。
   */
  useEffect(() => () => restoreFocusRef?.current?.focus({ preventScroll: true }), [restoreFocusRef]);

  const sessions = useInfiniteQuery({
    queryKey: ["agent-chat-sessions", query, filter],
    enabled: shell,
    initialPageParam: 0,
    queryFn: async ({ pageParam }) =>
      callDesktop(IPC_COMMANDS.chatSessionList, {
        query: query.trim() || undefined,
        archived: historyFilterParam(filter),
        offset: pageParam,
        limit: PAGE_SIZE,
      }),
    getNextPageParam: (lastPage, pages) => {
      const loaded = pages.reduce((total, page) => total + page.sessions.length, 0);
      return loaded < historyFilterTotal(lastPage.counts, filter) ? loaded : undefined;
    },
    // 打字时保留上一份结果，列表不会每敲一个字就清空重画。`isPlaceholderData` 为真时
    // 计数与「还有多少段」一律不显示 —— 那是**上一个关键字**的数字，不能拿来充数。
    placeholderData: keepPreviousData,
  });

  const refresh = useCallback(
    () => queryClient.invalidateQueries({ queryKey: ["agent-chat-sessions"] }),
    [queryClient],
  );

  const archiveSession = useMutation({
    mutationFn: ({ sessionId, archived }: ArchiveInput) =>
      callDesktop(IPC_COMMANDS.chatSessionArchive, { sessionId, archived }),
    onSuccess: ({ session }) => {
      setActionError(null);
      onSessionUpdated(session);
      void refresh();
    },
    onError: (error) => setActionError(`更新归档状态失败：${errorMessage(error)}`),
  });
  const renameSession = useMutation({
    mutationFn: ({ sessionId, title }: RenameInput) =>
      callDesktop(IPC_COMMANDS.chatSessionRename, { sessionId, title }),
    onSuccess: ({ session }) => {
      setActionError(null);
      setRenamingId(null);
      // 输入框随改名一起消失，焦点必须落回这一行，否则会掉到 body 上。
      setFocusRequest(rowIndexRef.current.get(session.id) ?? 0);
      onSessionUpdated(session);
      void refresh();
    },
    // 失败时**不**关掉输入框：用户刚打的标题还在里面，关掉就等于让他重打一遍。
    onError: (error) => setActionError(`重命名失败：${errorMessage(error)}`),
  });
  const deleteSession = useMutation({
    mutationFn: (sessionId: string) => callDesktop(IPC_COMMANDS.chatSessionDelete, { sessionId }),
    onSuccess: (_response, sessionId) => {
      setActionError(null);
      setConfirmingId(null);
      // 被删掉的那一行没了，把焦点交给顶上来的那一行，键盘位置就不会丢。
      setFocusRequest(rowIndexRef.current.get(sessionId) ?? 0);
      onSessionDeleted(sessionId);
      void refresh();
    },
    onError: (error) => setActionError(`删除对话失败：${errorMessage(error)}`),
  });

  const rows = useMemo(
    () => sessions.data?.pages.flatMap((page) => page.sessions) ?? [],
    [sessions.data],
  );
  const rowIndex = useMemo(() => new Map(rows.map((session, index) => [session.id, index])), [rows]);
  useEffect(() => {
    rowIndexRef.current = rowIndex;
  }, [rowIndex]);
  const groups = useMemo(() => groupSessionsByDay(rows, clock), [rows, clock]);
  const counts = sessions.isPlaceholderData ? null : sessions.data?.pages[0]?.counts ?? null;
  const total = counts ? historyFilterTotal(counts, filter) : null;
  const busy = archiveSession.isPending || renameSession.isPending || deleteSession.isPending || openingId !== null;

  useEffect(() => {
    // 列表变短（换筛选、删掉一条）之后，游标可能停在已经不存在的行上。
    setFocusIndex((current) => (current > rows.length - 1 ? Math.max(0, rows.length - 1) : current));
  }, [rows.length]);

  const rowButtons = (): HTMLButtonElement[] =>
    Array.from(listRef.current?.querySelectorAll<HTMLButtonElement>("[data-history-row]") ?? []);

  const focusRow = useCallback((index: number): void => {
    const buttons = rowButtons();
    if (!buttons.length) return;
    const clamped = Math.min(Math.max(index, 0), buttons.length - 1);
    buttons[clamped]?.focus();
    setFocusIndex(clamped);
  }, []);

  useEffect(() => {
    if (focusRequest === null) return;
    focusRow(focusRequest);
    setFocusRequest(null);
  }, [focusRequest, focusRow]);

  const onListKeyDown = (event: KeyboardEvent<HTMLDivElement>): void => {
    // 正在改名或正在确认删除时，键盘属于那个输入框/确认条，不属于列表。
    if (renamingId || confirmingId) return;
    if (event.key === "ArrowDown") {
      event.preventDefault();
      focusRow(focusIndex + 1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      focusRow(focusIndex - 1);
    } else if (event.key === "Home") {
      event.preventDefault();
      focusRow(0);
    } else if (event.key === "End") {
      event.preventDefault();
      focusRow(rows.length - 1);
    }
  };

  /**
   * 面板外壳也在 `document` 上监听 Escape（收起整个面板）。这里先截住它，于是
   * Escape 的含义按「退一步」分级：退出改名 / 关掉确认 / 清空搜索 → 离开记录视图，
   * 再按一次才收起面板。`stopPropagation` 会挡住挂在 document 上的那个监听器，
   * 因为 React 的根容器在它下面。
   */
  const onPaneKeyDown = (event: KeyboardEvent<HTMLElement>): void => {
    if (event.key === "/" && !(event.target instanceof HTMLInputElement)) {
      event.preventDefault();
      searchRef.current?.focus();
      return;
    }
    if (event.key !== "Escape") return;
    event.stopPropagation();
    if (renamingId) {
      setRenamingId(null);
      return;
    }
    if (confirmingId) {
      setConfirmingId(null);
      setFocusRequest(rowIndexRef.current.get(confirmingId) ?? 0);
      return;
    }
    if (query) {
      setQuery("");
      return;
    }
    onClose();
  };

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

  const startRename = (session: ChatSession): void => {
    setConfirmingId(null);
    setRenameDraft(session.title);
    setRenamingId(session.id);
  };

  const submitRename = (session: ChatSession): void => {
    // 一次写入只发一次：Enter 连按、或者「保存」被点两次，都不该排第二个请求。
    if (renameSession.isPending) return;
    const title = renameDraft.trim();
    if (!title) return;
    // 没改就不发请求：一次没有变化的写入只会在审计里留下一条无意义的记录。
    if (title === session.title) {
      setRenamingId(null);
      setFocusRequest(rowIndex.get(session.id) ?? 0);
      return;
    }
    renameSession.mutate({ sessionId: session.id, title });
  };

  const cancelRename = (session: ChatSession): void => {
    // 写入在途时按钮看着是停住的，这里也必须真的停住 —— `aria-disabled` 不拦点击。
    if (renameSession.isPending) return;
    setRenamingId(null);
    setFocusRequest(rowIndex.get(session.id) ?? 0);
  };

  const cancelDelete = (session: ChatSession): void => {
    setConfirmingId(null);
    setFocusRequest(rowIndex.get(session.id) ?? 0);
  };

  const selectFilter = (next: HistoryFilter): void => {
    setFilter(next);
    setRenamingId(null);
    setConfirmingId(null);
    setFocusIndex(0);
  };

  /** 打字：换关键字的同时把行游标与行内编辑状态归零。 */
  const searchSessions = (value: string): void => {
    setQuery(value);
    setRenamingId(null);
    setConfirmingId(null);
    setFocusIndex(0);
  };

  // 计数为空只说明「这一份数字还没有」：换关键字时它会短暂为空，而重取同一份数据时
  // 它一直在。所以这里看的是计数在不在，不是「有没有在请求」—— 否则每次归档都会让
  // 标题闪一下「正在读取记录」，而列表其实一个字都没变。
  const summary = !shell
    ? "只在桌面应用里可用"
    : counts === null
      ? "正在读取记录"
      : `${query.trim() ? "匹配" : "共"} ${total} 段对话${total !== null && rows.length < total ? ` · 已载入 ${rows.length}` : ""}`;

  const emptyState = (): { icon: "search" | "logs" | "sparkle"; title: string; body: string } => {
    if (query.trim()) {
      return { icon: "search", title: "没有匹配的对话", body: "换个关键词试试，标题和消息正文都会被搜索。" };
    }
    if (filter === "archived") {
      return { icon: "logs", title: "暂无已归档的对话", body: "归档过的任务会集中显示在这里。" };
    }
    if (filter === "all") {
      return { icon: "sparkle", title: "还没有对话记录", body: "发送第一个任务后，记录会自动保存在这里。" };
    }
    return { icon: "sparkle", title: "暂无进行中的对话", body: "发送第一个任务后，记录会自动保存在这里。" };
  };

  const empty = emptyState();
  const hasRows = rows.length > 0;
  const hasMore = counts !== null && total !== null && rows.length < total;

  return (
    <section className="agent-history-pane" aria-label="对话记录" onKeyDown={onPaneKeyDown}>
      <div className="agent-history-heading">
        <div>
          <h3>对话记录</h3>
          <p className="agent-history-summary">{summary}</p>
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

      <AgentHistoryToolbar
        query={query}
        onSearch={searchSessions}
        onClearSearch={() => setQuery("")}
        filter={filter}
        counts={counts}
        isFetching={sessions.isFetching}
        onSelectFilter={selectFilter}
        searchRef={searchRef}
      />

      {actionError ? <div className="agent-history-error" role="alert">{actionError}</div> : null}

      <div
        className="agent-history-panel"
        id="agent-history-panel"
        role="tabpanel"
        aria-labelledby={`history-tab-${filter}`}
      >
        {!shell ? (
          <div className="agent-history-empty">
            <Icon name="logs" size="xl" />
            <strong>桌面应用中可查询记录</strong>
            <p>对话会保存到本机数据库，浏览器预览不会读取本地记录。</p>
          </div>
        ) : null}
        {shell && sessions.isLoading ? (
          <div className="agent-history-empty"><span className="loading-spinner" /><strong>正在读取记录</strong></div>
        ) : null}
        {shell && sessions.isError ? (
          <div className="agent-history-empty">
            <Icon name="warning" size="xl" />
            <strong>无法读取对话记录</strong>
            <p>{errorMessage(sessions.error)}</p>
            <button type="button" className="text-button" onClick={() => void sessions.refetch()}>重试</button>
          </div>
        ) : null}
        {shell && !sessions.isLoading && !sessions.isError && !hasRows ? (
          <div className="agent-history-empty">
            <Icon name={empty.icon} size="xl" />
            <strong>{empty.title}</strong>
            <p>{empty.body}</p>
          </div>
        ) : null}

        {shell && hasRows ? (
          <div className="agent-history-list" ref={listRef} onKeyDown={onListKeyDown} role="list">
            {groups.map((bucket) => (
              <section className="agent-history-group" key={bucket.key} role="group" aria-label={bucket.label}>
                <AgentHistoryGroupHeader label={bucket.label} count={bucket.sessions.length} />
                {bucket.sessions.map((session) => (
                  <AgentHistoryRow
                    key={session.id}
                    session={session}
                    index={rowIndex.get(session.id) ?? 0}
                    focusIndex={focusIndex}
                    isActive={activeSessionId === session.id}
                    renaming={renamingId === session.id}
                    confirming={confirmingId === session.id}
                    opening={openingId === session.id}
                    busy={busy}
                    query={query}
                    clock={clock}
                    servers={servers.data ?? []}
                    renameDraft={renameDraft}
                    onRenameDraftChange={setRenameDraft}
                    renameRef={renameRef}
                    cancelDeleteRef={cancelDeleteRef}
                    renameSession={renameSession}
                    archiveSession={archiveSession}
                    deleteSession={deleteSession}
                    onFocusRow={setFocusIndex}
                    onOpen={openSession}
                    onStartRename={startRename}
                    onSubmitRename={submitRename}
                    onCancelRename={cancelRename}
                    onCancelDelete={cancelDelete}
                    onRequestDelete={setConfirmingId}
                  />
                ))}
              </section>
            ))}
            {hasMore ? (
              <button
                type="button"
                className="agent-history-more"
                aria-disabled={sessions.isFetchingNextPage || undefined}
                onClick={() => {
                  if (!sessions.isFetchingNextPage) void sessions.fetchNextPage();
                }}
              >
                <Icon name="chevronDown" size="sm" />
                {sessions.isFetchingNextPage ? "正在载入" : `载入更早的对话（还有 ${(total ?? 0) - rows.length} 段）`}
              </button>
            ) : null}
          </div>
        ) : null}
      </div>
    </section>
  );
}
