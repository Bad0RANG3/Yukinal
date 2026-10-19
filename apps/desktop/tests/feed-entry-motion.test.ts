import assert from "node:assert/strict";
import test from "node:test";
import { nextFeedEntryMotion } from "../src/features/agent/feed-entry-motion.js";
import { appendAssistantDelta, appendEntries, entriesFromMessages, type Entry } from "../src/features/agent/transcript.js";

test("loading history and changing conversation never animate existing entries", () => {
  const history = entriesFromMessages([{ id: "old", sessionId: "session", role: "assistant", content: "saved answer", createdAt: "" }]);
  const first = nextFeedEntryMotion(null, [], false, "session");
  const loaded = nextFeedEntryMotion(first, history, false, "session");
  assert.equal(loaded.index, null);
  const replaced = nextFeedEntryMotion(loaded, history.map((entry) => ({ ...entry })), false, "session");
  assert.equal(replaced.index, null);
  assert.equal(nextFeedEntryMotion(replaced, history, true, "other-session").index, null);
});

test("only the last live appended entry gains a marker and token updates keep it stable", () => {
  const user: Entry = { kind: "user", text: "hello" };
  const initial = nextFeedEntryMotion<Entry>(null, [user], true, "session");
  const firstToken = appendAssistantDelta([user], "A");
  const started = nextFeedEntryMotion(initial, firstToken, true, "session");
  assert.equal(started.index, 1);
  const nextToken = appendAssistantDelta(firstToken, "B");
  const streamed = nextFeedEntryMotion(started, nextToken, true, "session");
  assert.equal(streamed.index, 1);
  const batch = appendEntries(nextToken, [{ kind: "system", text: "step" }, { kind: "error", text: "failed" }]);
  const appended = nextFeedEntryMotion(streamed, batch, true, "session");
  assert.equal(appended.index, 3);
});

test("completion clears the live marker for the same array and reactivation without an append cannot replay it", () => {
  const user: Entry = { kind: "user", text: "hello" };
  const initial = nextFeedEntryMotion<Entry>(null, [user], true, "session");
  const entries = appendAssistantDelta([user], "finished response");
  const live = nextFeedEntryMotion(initial, entries, true, "session");
  assert.equal(live.index, 1);
  const completed = nextFeedEntryMotion(live, entries, false, "session");
  assert.equal(completed.index, null);
  assert.equal(completed.entries, entries);
  assert.equal(completed.scope, "session");
  const activated = nextFeedEntryMotion(completed, entries, true, "session");
  assert.equal(activated.index, null);
  assert.equal(nextFeedEntryMotion(activated, [...entries], true, "session").index, null);
});

test("approval controls and reconstructed/replaced transcripts never gain append animation", () => {
  const user: Entry = { kind: "user", text: "hello" };
  const initial = nextFeedEntryMotion<Entry>(null, [user], true, "session");
  const approval = { kind: "approval" } as Entry;
  const waiting = nextFeedEntryMotion(initial, [user, approval], true, "session");
  assert.equal(waiting.index, null);
  const reconstructed: Entry[] = [{ kind: "user", text: "hello" }, { kind: "assistant", text: "old" }, { kind: "system", text: "old" }];
  assert.equal(nextFeedEntryMotion(waiting, reconstructed, true, "session").index, null);
});
