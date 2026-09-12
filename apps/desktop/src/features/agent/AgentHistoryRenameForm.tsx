/**
 * 行内重命名。行里唯一的一处内容编辑：标题是自动从首条提示词取的，所以它必须能改。
 *
 * 两条约束写在两个按钮上：
 *   - 标题为空时「保存」是真的不可用（没有东西可保存），所以那一个用 `disabled`；
 *   - 写入在途时只是不接受第二次提交，走 `aria-disabled`，按钮不失焦。
 *
 * Escape 在这里被截住：这一次 Escape 只用来放弃改名，不该同时关掉整个记录视图。
 */

import type { RefObject } from "react";

import type { ChatSession } from "@yukinal/shared";

/**
 * 重命名写入的句柄。理由同删除那个：这一行要知道的是「这次写入在不在途」，而它的答案
 * 只有一个来源 —— mutation 自己。
 */
export interface RenameSessionHandle {
  isPending: boolean;
  mutate(input: { sessionId: string; title: string }): void;
}

export function AgentHistoryRenameForm({
  session,
  draft,
  renameSession,
  renameRef,
  onDraftChange,
  onSubmit,
  onCancel,
}: {
  session: ChatSession;
  draft: string;
  renameSession: RenameSessionHandle;
  /** 进入改名时输入框的全文选中（见容器里的 effect）。 */
  renameRef: RefObject<HTMLInputElement | null>;
  onDraftChange: (value: string) => void;
  onSubmit: () => void;
  onCancel: () => void;
}) {
  return (
    <div className="agent-history-rename">
      <label className="sr-only" htmlFor={`history-rename-${session.id}`}>对话标题</label>
      <input
        id={`history-rename-${session.id}`}
        ref={renameRef}
        className="form-input agent-history-rename-input"
        value={draft}
        maxLength={200}
        onChange={(event) => onDraftChange(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            onSubmit();
          } else if (event.key === "Escape") {
            // 先截住：这一次 Escape 只用来放弃改名，不该同时关掉记录视图。
            event.preventDefault();
            event.stopPropagation();
            onCancel();
          }
        }}
      />
      <div className="agent-history-rename-actions">
        <button
          type="button"
          className="text-button"
          // 已经发出去的写入没法撤回，所以此刻「取消」不接受点击；但用
          // `aria-disabled` 而不是 `disabled`，理由同列表里的动作按钮。
          aria-disabled={renameSession.isPending || undefined}
          onClick={onCancel}
        >
          取消
        </button>
        <button
          type="button"
          className="button-secondary"
          // 标题为空时是真的不可用（没有东西可保存）；写入进行中只是暂时
          // 不接受第二次提交，所以那一种走 `aria-disabled`，按钮不失焦。
          disabled={!draft.trim()}
          aria-disabled={renameSession.isPending || undefined}
          onClick={onSubmit}
        >
          保存
        </button>
      </div>
    </div>
  );
}
