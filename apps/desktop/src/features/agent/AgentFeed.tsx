/**
 * 动态列表：一段运行里发生过什么，按因果顺序读下来。
 *
 * 这里不做任何「补全」—— 没有运行就没有内容。空态是真实的空，不是占位对话。
 * 「为什么不能发」的提示由调用方作为 children 传入，因为它取决于面板之外的
 * 事实（sidecar、provider、是不是预览模式）。
 */

import { useId, useLayoutEffect, useRef, type RefObject, type ReactNode, type UIEvent } from "react";

import type { ApprovalDecision, ApprovalRequest } from "@yukinal/shared";

import { AgentEntryView } from "./AgentEntryView.js";
import { ConversationScrollRail } from "./ConversationScrollRail.js";
import { nextFeedEntryMotion, type FeedEntryMotionState } from "./feed-entry-motion.js";
import type { Entry } from "./transcript.js";

export function AgentFeed({
  entries,
  running,
  runState,
  lastUserEntry,
  canSend,
  onRetry,
  onApproval,
  pendingApprovalIds,
  approvalStatuses,
  feedRef,
  frameRef,
  onScroll,
  showJumpControl,
  onJumpToLatest,
  motionScope,
  children,
}: {
  entries: Entry[];
  running: boolean;
  runState: string | null;
  lastUserEntry?: Extract<Entry, { kind: "user" }>;
  canSend: boolean;
  onRetry: (entry: Extract<Entry, { kind: "user" }>) => void;
  onApproval: (approval: ApprovalRequest, decision: ApprovalDecision) => Promise<void>;
  pendingApprovalIds: string[];
  approvalStatuses: Record<string, string>;
  feedRef: RefObject<HTMLDivElement | null>;
  frameRef: RefObject<HTMLDivElement | null>;
  onScroll: (event: UIEvent<HTMLDivElement>) => void;
  showJumpControl: boolean;
  onJumpToLatest: () => void;
  motionScope: unknown;
  /** 就绪提示等面板级说明，渲染在动态之后。 */
  children?: ReactNode;
}) {
  const feedId = useId();
  const previousMotion = useRef<FeedEntryMotionState<Entry> | null>(null);
  const motion = nextFeedEntryMotion(previousMotion.current, entries, running, motionScope);
  useLayoutEffect(() => { previousMotion.current = motion; }, [motion]);
  return (
    <div ref={frameRef} className="agent-feed-frame">
    <div id={feedId} ref={feedRef} className="agent-feed" role="region" aria-label="对话内容" tabIndex={0} onScroll={onScroll}>
      {entries.length === 0 ? (
        <div className="agent-empty">
          <div className="agent-empty-copy">
            <strong>新任务</strong>
          </div>
        </div>
      ) : (
        entries.map((entry, index) => (
          <AgentEntryView
            key={entry.entryId ?? index}
            entry={entry}
            isNewEntry={index === motion.index}
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
            streaming={running && index === entries.length - 1 && (entry.kind === "assistant" || entry.kind === "reasoning")}
          />
        ))
      )}
      {!running && runState === "failed" && lastUserEntry ? (
        <div className="agent-retry">
          <span>这次运行未完成。</span>
          <button type="button" className="text-button" disabled={!canSend} onClick={() => onRetry(lastUserEntry)}>重试上一条</button>
        </div>
      ) : null}
      {children}
    </div>
    <ConversationScrollRail viewportRef={feedRef} viewportId={feedId} contentVersion={entries} />
    {showJumpControl ? <button type="button" className="agent-feed-jump" aria-controls={feedId} onClick={onJumpToLatest}>回到最新 <span aria-hidden="true">↓</span></button> : null}
    </div>
  );
}
