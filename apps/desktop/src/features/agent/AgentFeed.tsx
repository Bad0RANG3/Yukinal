/**
 * 动态列表：一段运行里发生过什么，按因果顺序读下来。
 *
 * 这里不做任何「补全」—— 没有运行就没有内容。空态是真实的空，不是占位对话。
 * 「为什么不能发」的提示由调用方作为 children 传入，因为它取决于面板之外的
 * 事实（sidecar、provider、是不是预览模式）。
 */

import { useId, useLayoutEffect, useMemo, useRef, type RefObject, type ReactNode, type UIEvent } from "react";

import type { ApprovalDecision, ApprovalRequest } from "@yukinal/shared";

import { AgentEntryView, toolActionLabel } from "./AgentEntryView.js";
import { ConversationScrollRail } from "./ConversationScrollRail.js";
import { conversationTurns } from "./conversation-scroll.js";
import { nextFeedEntryMotion, type FeedEntryMotionState } from "./feed-entry-motion.js";
import type { Entry } from "./transcript.js";

/** Fold only adjacent successful calls. Failures, approvals and live work stay visible. */
export function toolActivityRows(entries: Entry[]): { entry: Entry; index: number }[][] {
  const rows: { entry: Entry; index: number }[][] = [];
  entries.forEach((entry, index) => {
    const previous = rows.at(-1);
    const last = previous?.at(-1)?.entry;
    if (entry.kind === "tool" && entry.result?.status === "success" && last?.kind === "tool" && last.result?.status === "success") previous!.push({ entry, index });
    else rows.push([{ entry, index }]);
  });
  return rows;
}

/** Actual later activity makes a reply intermediate; never infer success from its prose. */
export function intermediateReplyIndices(entries: Entry[], running: boolean, runState: string | null): Set<number> {
  const indices = new Set<number>();
  let moreWork = running || runState === "failed" || runState === "cancelled";
  for (let index = entries.length - 1; index >= 0; index -= 1) {
    const entry = entries[index]!;
    if (entry.kind === "user") moreWork = false;
    else if (entry.kind === "assistant") {
      if (moreWork) indices.add(index);
      moreWork = true;
    } else if (entry.kind === "tool" || entry.kind === "approval" || entry.kind === "error") moreWork = true;
  }
  return indices;
}

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
  const turns = useMemo(() => conversationTurns(entries, feedId), [entries, feedId]);
  const currentTurnStart = turns.at(-1)?.entryIndex ?? entries.length;
  const turnAnchors = new Map(turns.map((turn) => [turn.entryIndex, turn.id]));
  const previousMotion = useRef<FeedEntryMotionState<Entry> | null>(null);
  const motion = nextFeedEntryMotion(previousMotion.current, entries, running, motionScope);
  const activityRows = useMemo(() => toolActivityRows(entries), [entries]);
  const intermediateReplies = useMemo(() => intermediateReplyIndices(entries, running, runState), [entries, running, runState]);
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
        activityRows.map((row) => {
          const content = row.map(({ entry, index }) => (
          <AgentEntryView
            key={entry.entryId ?? index}
            entry={entry}
            turnAnchor={turnAnchors.get(index)}
            isNewEntry={index === motion.index}
            intermediate={intermediateReplies.has(index)}
            onApproval={onApproval}
            approvalBusy={entry.kind === "approval" && pendingApprovalIds.includes(entry.approval.approvalId)}
            approvalStatus={
              entry.kind === "approval"
                ? approvalStatuses[entry.approval.approvalId] ?? (!running || index < currentTurnStart ? "本次运行已结束" : undefined)
                : undefined
            }
            // Only the tail of the feed can still be growing, and only a
            // running assistant turn grows at all: everything above it is
            // settled history.
            streaming={running && index === entries.length - 1 && (entry.kind === "assistant" || entry.kind === "reasoning")}
          />
          ));
          if (row.length === 1) return content;
          const last = row.at(-1)!.entry;
          return <details className="agent-tool-activity" key={row[0]!.entry.entryId ?? row[0]!.index}>
            <summary><span>执行了 {row.length} 项操作</span><span className="agent-tool-activity-caption">{last.kind === "tool" ? last.purpose ?? toolActionLabel(last.toolName) : ""}</span></summary>
            <div className="agent-tool-activity-body">{content}</div>
          </details>;
        })
      )}
      {!running && runState === "failed" && lastUserEntry ? (
        <div className="agent-retry">
          <span>这次运行未完成。</span>
          <button type="button" className="text-button" disabled={!canSend} onClick={() => onRetry(lastUserEntry)}>重试上一条</button>
        </div>
      ) : null}
      {children}
    </div>
    <ConversationScrollRail viewportRef={feedRef} viewportId={feedId} turns={turns} contentVersion={entries} />
    {showJumpControl ? <button type="button" className="agent-feed-jump" aria-controls={feedId} onClick={onJumpToLatest}>回到最新 <span aria-hidden="true">↓</span></button> : null}
    </div>
  );
}
