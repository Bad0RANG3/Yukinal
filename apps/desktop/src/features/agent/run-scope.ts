/**
 * Which conversation a send belongs to, and which transcript rows are internal.
 *
 * The workspace keeps one conversation per server plus a global one. Picking the
 * scope is a routing decision, not an authorization decision: the host still
 * checks the selected server on every run.
 */
import { resolveMentionedServer, type MentionCandidate } from "./composer-triggers.js";
import type { Entry } from "./transcript.js";

/** Tool receipts for background delegation stay in the trace, not the feed. */
const INTERNAL_TOOL_NAMES = new Set([
  "subagents.start", "subagents.status", "subagents.stop",
  "teams.dispatch", "teams.status", "teams.cancel",
]);

export function agentEntries(entries: Entry[]): Entry[] {
  return entries.filter((entry) => entry.kind !== "tool" || !INTERNAL_TOOL_NAMES.has(entry.toolName));
}

/**
 * Resolve the server a message targets.
 *
 * An explicit @mention wins. Several mentions mean the request spans servers, so
 * the conversation has no single target. Otherwise the selected server, then the
 * implicit fallback (the sole connected server) applies.
 */
export function agentScope(
  prompt: string,
  candidates: readonly MentionCandidate[],
  selectedServerId: string | null,
  implicitServerId: string | null = null,
  explicitGlobalScope = false,
): string | null {
  const targets = candidates.filter((candidate) => resolveMentionedServer(prompt, [candidate]));
  return targets.length > 1 ? null : targets[0]?.id ?? selectedServerId ?? (explicitGlobalScope ? null : implicitServerId);
}
