/**
 * 动态列表：一段运行里发生过什么，按因果顺序读下来。
 *
 * 这里不做任何「补全」—— 没有运行就没有内容。空态是真实的空，不是占位对话。
 * 「为什么不能发」的提示由调用方作为 children 传入，因为它取决于面板之外的
 * 事实（sidecar、provider、是不是预览模式）。
 */

import type { RefObject, ReactNode, UIEvent } from "react";

import type { ApprovalDecision, ApprovalRequest } from "@yukinal/shared";

import { Icon } from "../../components/Icon.js";
import { AgentEntryView } from "./AgentEntryView.js";
import type { Entry } from "./transcript.js";

export function AgentFeed({
  entries,
  running,
  runState,
  lastUserPrompt,
  canSend,
  onRetry,
  onApproval,
  pendingApprovalIds,
  approvalStatuses,
  feedRef,
  onScroll,
  children,
}: {
  entries: Entry[];
  running: boolean;
  runState: string | null;
  lastUserPrompt?: string;
  canSend: boolean;
  onRetry: (prompt: string) => void;
  onApproval: (approval: ApprovalRequest, decision: ApprovalDecision) => Promise<void>;
  pendingApprovalIds: string[];
  approvalStatuses: Record<string, string>;
  feedRef: RefObject<HTMLDivElement | null>;
  onScroll: (event: UIEvent<HTMLDivElement>) => void;
  /** 就绪提示等面板级说明，渲染在动态之后。 */
  children?: ReactNode;
}) {
  return (
    <div ref={feedRef} className="agent-feed" onScroll={onScroll}>
      {entries.length === 0 ? (
        <div className="agent-empty">
          <span className="agent-empty-mark"><Icon name="sparkle" size="xl" /></span>
          <strong>随心输入</strong>
          <p>描述你想完成的事情，Agent 会在当前上下文里协助你。</p>
        </div>
      ) : (
        entries.map((entry, index) => (
          <AgentEntryView
            key={index}
            entry={entry}
            onApproval={onApproval}
            approvalBusy={entry.kind === "approval" && pendingApprovalIds.includes(entry.approval.approvalId)}
            approvalStatus={
              entry.kind === "approval"
                ? approvalStatuses[entry.approval.approvalId] ?? (!running ? "本次运行已结束" : undefined)
                : undefined
            }
            // Only the tail of the feed can still be growing, and only a
            // running assistant turn grows at all: everything above it is
            // settled history.
            streaming={running && index === entries.length - 1 && entry.kind === "assistant"}
          />
        ))
      )}
      {!running && runState === "failed" && lastUserPrompt ? (
        <div className="agent-retry">
          <span>这次运行未完成。</span>
          <button type="button" className="text-button" disabled={!canSend} onClick={() => onRetry(lastUserPrompt)}>重试上一条</button>
        </div>
      ) : null}
      {children}
    </div>
  );
}
