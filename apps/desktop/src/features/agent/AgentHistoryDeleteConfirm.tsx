/**
 * 行内删除确认。
 *
 * `window.confirm` 会挡住整个窗口，也是这套界面里唯一一个不属于它的对话框，所以删除的
 * 二次确认落在行里：先用一句话说清这一删的后果（消息一并删掉、没法回头），再给两个按钮。
 * 默认焦点在「取消」上（由容器在确认条挂载后交过来）。
 *
 * 「删除」按钮在写入在途时用 `aria-disabled` 而不是 `disabled`：disabled 会立刻让元素
 * 失焦，焦点掉到 `body`，键盘用户每删一条就得重新找回列表。
 */

import type { RefObject } from "react";

import type { ChatSession } from "@yukinal/shared";

/**
 * 删除写入的句柄。
 *
 * 这里交下去的是 mutation 本身而不是一个 `pending` 布尔值，因为这一行要知道的正是
 * 「这次写入在不在途」，而它的答案只有一个来源。
 */
export interface DeleteSessionHandle {
  isPending: boolean;
  mutate(sessionId: string): void;
}

export function AgentHistoryDeleteConfirm({
  session,
  deleteSession,
  cancelDeleteRef,
  onCancel,
}: {
  session: ChatSession;
  deleteSession: DeleteSessionHandle;
  /** 「取消」按钮的 ref：确认条出现之后焦点要落在它上面。 */
  cancelDeleteRef: RefObject<HTMLButtonElement | null>;
  onCancel: () => void;
}) {
  return (
    <div className="agent-history-confirm">
      <p>删除「{session.title}」？这条对话的消息会一并删除，不能撤销。</p>
      <div className="agent-history-confirm-actions">
        <button ref={cancelDeleteRef} type="button" className="text-button" onClick={onCancel}>
          取消
        </button>
        <button
          type="button"
          className="button-secondary agent-history-confirm-delete"
          aria-disabled={deleteSession.isPending || undefined}
          onClick={() => {
            if (!deleteSession.isPending) deleteSession.mutate(session.id);
          }}
        >
          删除
        </button>
      </div>
    </div>
  );
}
