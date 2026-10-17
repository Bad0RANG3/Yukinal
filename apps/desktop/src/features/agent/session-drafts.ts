import type { AgentPromptPart } from "@yukinal/shared";

export type SessionDraft = { prompt: string; attachments: AgentPromptPart[] };

export function conversationKey(serverId: string | null, sessionId: string | null): string {
  return JSON.stringify([serverId, sessionId]);
}

/** Snapshot arrays so clearing one editor never clears another conversation's draft. */
export function rememberDraft(drafts: Map<string, SessionDraft>, serverId: string | null, sessionId: string | null, draft: SessionDraft): void {
  drafts.set(conversationKey(serverId, sessionId), { prompt: draft.prompt, attachments: [...draft.attachments] });
}

export function recalledDraft(drafts: ReadonlyMap<string, SessionDraft>, serverId: string | null, sessionId: string | null): SessionDraft {
  const draft = drafts.get(conversationKey(serverId, sessionId));
  return draft ? { prompt: draft.prompt, attachments: [...draft.attachments] } : { prompt: "", attachments: [] };
}
