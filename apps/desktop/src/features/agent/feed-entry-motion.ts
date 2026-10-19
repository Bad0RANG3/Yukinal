/** A live append is different from loading history or changing a streamed row. */
export interface FeedEntryMotionState<T> { entries: readonly T[]; scope: unknown; index: number | null }

export function nextFeedEntryMotion<T extends { kind: string }>(
  previous: FeedEntryMotionState<T> | null,
  entries: readonly T[],
  running: boolean,
  scope: unknown,
): FeedEntryMotionState<T> {
  if (!running || !previous || previous.scope !== scope) return { entries, scope, index: null };
  if (entries === previous.entries) return previous;
  const appended = running && entries.length > previous.entries.length
    && previous.entries.every((entry, index) => entry === entries[index]);
  if (appended) {
    const index = entries.length - 1;
    return { entries, scope, index: entries[index]?.kind === "approval" ? null : index };
  }
  // Keeping the same class on the mounted streaming row prevents animation
  // restarts on every token. Replacements/history loads do not gain a marker.
  const tailUpdate = entries.length === previous.entries.length
    && previous.entries.every((entry, index) => index === entries.length - 1 || entry === entries[index]);
  return { entries, scope, index: tailUpdate ? previous.index : null };
}
