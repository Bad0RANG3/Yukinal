import type { Entry } from "./transcript.js";

export const CONVERSATION_NAVIGATE_EVENT = "yukinal:conversation-turn";
export const CONVERSATION_LATEST_EVENT = "yukinal:conversation-latest";
export interface ConversationTurn { id: string; entryIndex: number; preview: string }

/** Exactly one anchor per retained user message, regardless of tools or reply length. */
export function conversationTurns(entries: readonly Entry[], scope: string): ConversationTurn[] {
  return entries.flatMap((entry, entryIndex) => entry.kind === "user" ? [{
    id: `${scope}-turn-${entry.entryId ?? `legacy-${entryIndex}`}`,
    entryIndex,
    preview: entry.text.replace(/\s+/g, " ").trim().slice(0, 180) || (entry.attachments?.length ? `附加了 ${entry.attachments.length} 个文件` : "用户消息"),
  }] : []);
}

/** Resolve a reading position against real anchor offsets; never synthesize ticks. */
export function currentConversationTurn(tops: readonly number[], scrollTop: number, viewportHeight: number, contentHeight: number): number {
  if (tops.length === 0) return -1;
  const top = Number.isFinite(scrollTop) ? Math.max(0, scrollTop) : 0;
  const max = Math.max(0, contentHeight - viewportHeight);
  if (max > 1 && top >= max - 2) return tops.length - 1;
  const readingLine = top + 24;
  let active = 0;
  for (let index = 0; index < tops.length; index++) {
    if (tops[index]! > readingLine) break;
    active = index;
  }
  return active;
}

export function conversationTurnTarget(anchorTop: number, viewportHeight: number, contentHeight: number): number {
  return Math.max(0, Math.min(Math.max(0, contentHeight - viewportHeight), anchorTop - 12));
}
