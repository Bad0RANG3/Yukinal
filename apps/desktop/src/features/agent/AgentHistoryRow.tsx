/**
 * 记录列表里的一行：打开这一段的入口，加上这一行自己的三个动作（重命名 / 归档 / 删除）。
 *
 * 一个行同一时刻只有一副面孔，按状态三选一：
 *   - **改名中** —— 输入框替换正文（`AgentHistoryRenameForm`）；
 *   - **确认删除** —— 行内确认条替换正文（`AgentHistoryDeleteConfirm`）；
 *   - **其余** —— 打开按钮 + 三个动作按钮。
 *
 * 一条硬规则贯穿这一行的每一个按钮：**写入在途时不使用 `disabled`**。被禁用的元素会
 * 立刻失焦，焦点掉到 `body`，键盘用户每归档一条就得重新找回列表。待写入状态一律用
 * `aria-disabled` + 事件里的 early return 表达 —— 对读屏说的是同一件事，而焦点留在原地。
 *
 * 数据与动作都由容器（`AgentHistoryPane`）拥有，这里只画出来；连行的游标（`focusIndex`）
 * 也是容器的状态，所以方向键那套逻辑不在这里。
 */

import type { ChatSession } from "@yukinal/shared";
import type { RefObject } from "react";

import { Icon } from "../../components/Icon.js";
import { formatRelativeTime } from "../../lib/format.js";
import { historyTargetLabel, matchSegments } from "./history.js";
import { AgentHistoryDeleteConfirm, type DeleteSessionHandle } from "./AgentHistoryDeleteConfirm.js";
import { AgentHistoryRenameForm, type RenameSessionHandle } from "./AgentHistoryRenameForm.js";

/** 归档写入的句柄；这一行只调用它，不看它的在途状态（按钮的忙态由 `busy` 统一表达）。 */
export interface ArchiveSessionHandle {
  mutate(input: { sessionId: string; archived: boolean }): void;
}

/** 命中部分加底色。切段而不是拼 HTML —— 标题与预览都是不可信文本（ADR 0015）。 */
function Highlight({ text, query }: { text: string; query: string }) {
  return (
    <>
      {matchSegments(text, query).map((segment, index) =>
        segment.match ? (
          <mark className="agent-history-match" key={index}>{segment.text}</mark>
        ) : (
          <span key={index}>{segment.text}</span>
        ),
      )}
    </>
  );
}

export function AgentHistoryRow({
  session,
  index,
  focusIndex,
  isActive,
  renaming,
  confirming,
  opening,
  busy,
  query,
  clock,
  servers,
  renameDraft,
  onRenameDraftChange,
  renameRef,
  cancelDeleteRef,
  renameSession,
  archiveSession,
  deleteSession,
  onFocusRow,
  onOpen,
  onStartRename,
  onSubmitRename,
  onCancelRename,
  onCancelDelete,
  onRequestDelete,
}: {
  session: ChatSession;
  /** 这一行在已载入行里的序号；与 `focusIndex` 一起决定谁进 Tab 序列。 */
  index: number;
  focusIndex: number;
  isActive: boolean;
  renaming: boolean;
  confirming: boolean;
  opening: boolean;
  busy: boolean;
  /** 当前关键字：命中高亮按它切段。 */
  query: string;
  clock: number;
  /** 已经取到的服务器，用来把 `serverId` 说成人能读的名字。 */
  servers: { id: string; name: string }[];
  renameDraft: string;
  onRenameDraftChange: (value: string) => void;
  renameRef: RefObject<HTMLInputElement | null>;
  cancelDeleteRef: RefObject<HTMLButtonElement | null>;
  renameSession: RenameSessionHandle;
  archiveSession: ArchiveSessionHandle;
  deleteSession: DeleteSessionHandle;
  onFocusRow: (index: number) => void;
  onOpen: (session: ChatSession) => Promise<void>;
  onStartRename: (session: ChatSession) => void;
  onSubmitRename: (session: ChatSession) => void;
  onCancelRename: (session: ChatSession) => void;
  onCancelDelete: (session: ChatSession) => void;
  onRequestDelete: (sessionId: string) => void;
}) {
  const archived = Boolean(session.archivedAt);
  return (
    <article
      className={`agent-history-row${isActive ? " is-active" : ""}${renaming ? " is-editing" : ""}`}
      role="listitem"
    >
      {renaming ? (
        <AgentHistoryRenameForm
          session={session}
          draft={renameDraft}
          renameSession={renameSession}
          renameRef={renameRef}
          onDraftChange={onRenameDraftChange}
          onSubmit={() => onSubmitRename(session)}
          onCancel={() => onCancelRename(session)}
        />
      ) : confirming ? (
        <AgentHistoryDeleteConfirm
          session={session}
          deleteSession={deleteSession}
          cancelDeleteRef={cancelDeleteRef}
          onCancel={() => onCancelDelete(session)}
        />
      ) : (
        <>
          <button
            type="button"
            className="agent-history-row-open"
            data-history-row=""
            // 单点 Tab 停靠：整个列表只算一个 Tab 位，进来之后用方向键走。
            tabIndex={index === focusIndex ? 0 : -1}
            aria-current={isActive ? "true" : undefined}
            // 待写入时**不**用 `disabled`：被禁用的元素会立刻失焦，焦点掉到
            // body，键盘用户每归档一条就得重新找回列表。`aria-disabled` +
            // 事件里的 early return 对读屏说的是同一件事，但焦点留在原地。
            aria-disabled={busy || undefined}
            aria-busy={opening || undefined}
            onFocus={() => onFocusRow(index)}
            onClick={() => {
              if (!busy) void onOpen(session);
            }}
          >
            <span className="agent-history-row-top">
              <strong><Highlight text={session.title} query={query} /></strong>
              {opening ? (
                <span className="loading-spinner agent-history-row-spinner" />
              ) : (
                <time dateTime={session.updatedAt}>{formatRelativeTime(session.updatedAt, clock)}</time>
              )}
            </span>
            <span className="agent-history-row-preview">
              {session.lastMessagePreview ? <Highlight text={session.lastMessagePreview} query={query} /> : "暂无消息"}
            </span>
            <span className="agent-history-row-meta">
              <span className="agent-history-chip">{historyTargetLabel(session, servers)}</span>
              <span>{session.messageCount} 条消息</span>
              {archived ? <span className="agent-history-badge">已归档</span> : null}
            </span>
          </button>
          <div className="agent-history-row-actions">
            <button
              type="button"
              className="icon-button"
              title="重命名"
              aria-label={`重命名 ${session.title}`}
              aria-disabled={busy || undefined}
              onClick={() => {
                if (!busy) onStartRename(session);
              }}
            >
              <Icon name="edit" size="sm" />
            </button>
            <button
              type="button"
              className="icon-button"
              title={archived ? "恢复" : "归档"}
              aria-label={`${archived ? "恢复" : "归档"} ${session.title}`}
              aria-disabled={busy || undefined}
              onClick={() => {
                if (!busy) archiveSession.mutate({ sessionId: session.id, archived: !archived });
              }}
            >
              <Icon name="archive" size="sm" />
            </button>
            <button
              type="button"
              className="icon-button agent-history-delete"
              title="删除对话"
              aria-label={`删除 ${session.title}`}
              aria-disabled={busy || undefined}
              onClick={() => {
                if (!busy) onRequestDelete(session.id);
              }}
            >
              <Icon name="trash" size="sm" />
            </button>
          </div>
        </>
      )}
    </article>
  );
}
