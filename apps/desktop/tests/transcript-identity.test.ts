import assert from "node:assert/strict";
import test from "node:test";
import type { ChatMessage } from "@yukinal/shared";
import {
  appendAssistantDelta, appendEntries, appendReasoningDelta, appendToolCall,
  entriesFromMessages, MAX_TRANSCRIPT_ENTRIES, settleAssistantText, settleToolResult,
  type Entry, type ToolCallFacts,
} from "../src/features/agent/transcript.js";
import { AgentRunStream, appendStreamBatch, settleStreamText } from "../src/features/agent/run-stream.js";

function ids(entries: Entry[]): string[] {
  return entries.map((entry) => {
    assert.equal(typeof entry.entryId, "string");
    assert.ok(entry.entryId);
    return entry.entryId;
  });
}
const call: ToolCallFacts = { callId: "call_fixture", toolName: "fixture.read", target: "offline", riskLevel: "read", decision: "auto" };

test("head eviction after more than 500 rows retains every surviving row's identity and object", () => {
  let entries = appendEntries([], Array.from({ length: MAX_TRANSCRIPT_ENTRIES }, (_, index) => ({ kind: "system", text: `row ${index}` })));
  const original = entries;
  const originalIds = ids(entries);
  for (let index = 0; index < 125; index++) entries = appendEntries(entries, [{ kind: "system", text: `new ${index}` }]);
  assert.equal(entries.length, MAX_TRANSCRIPT_ENTRIES);
  assert.equal(new Set(ids(entries)).size, MAX_TRANSCRIPT_ENTRIES);
  assert.deepEqual(ids(entries.slice(0, 375)), originalIds.slice(125));
  entries.slice(0, 375).forEach((entry, index) => assert.equal(entry, original[index + 125]));
  assert.equal((entries.at(-1) as Extract<Entry, { kind: "system" }>).text, "new 124");
});

test("tail deltas and final calibration keep the created id while unrelated rows stay the same objects", () => {
  let entries = appendEntries([], [{ kind: "user", text: "keep code" }]);
  entries = appendAssistantDelta(entries, "\t");
  const user = entries[0];
  const tailId = entries.at(-1)?.entryId;
  for (let index = 0; index < 1000; index++) entries = appendAssistantDelta(entries, "x");
  assert.equal(entries[0], user);
  assert.equal(entries.at(-1)?.entryId, tailId);
  entries = settleAssistantText(entries, "\t" + "x".repeat(1000) + "\r\n");
  assert.equal(entries.at(-1)?.entryId, tailId);
  const unchanged = settleAssistantText(entries, "\t");
  assert.equal(unchanged, entries);
  entries = settleAssistantText(entries, "a divergent final answer");
  assert.notEqual(entries.at(-1)?.entryId, tailId);
  assert.equal(entries.at(-2)?.entryId, tailId);
});

test("equal text in separate reasoning, answer and tool rounds never shares an entry id", () => {
  let entries = appendAssistantDelta([], "same");
  entries = appendReasoningDelta(entries, "same");
  entries = appendAssistantDelta(entries, "same");
  entries = appendToolCall(entries, call);
  entries = appendAssistantDelta(entries, "same");
  assert.deepEqual(entries.map((entry) => entry.kind), ["assistant", "reasoning", "assistant", "tool", "assistant"]);
  assert.equal(new Set(ids(entries)).size, entries.length);
});

test("out-of-order tool results retain the pending card id; orphan results get their own id", () => {
  const second = { ...call, callId: "call_second" };
  let entries = appendToolCall(appendToolCall([], call), second);
  const before = ids(entries);
  entries = settleToolResult(entries, second, { status: "success", durationMs: 2, summary: "second" });
  entries = settleToolResult(entries, call, { status: "failed", durationMs: 3, summary: "first" });
  assert.deepEqual(ids(entries), before);
  entries = settleToolResult(entries, { ...call, callId: "orphan" }, { status: "cancelled", durationMs: 0, summary: "orphan" });
  assert.equal(new Set(ids(entries)).size, 3);
});

test("history restoration uses durable message ids across repeated loads and truncation", () => {
  const messages: ChatMessage[] = Array.from({ length: 530 }, (_, index) => ({
    id: `persisted_${index}`, sessionId: "session_fixture", role: index % 2 ? "assistant" : "user",
    content: "equal repeated content", createdAt: "2026-10-18T00:00:00Z",
  }));
  messages.splice(10, 0, { id: "internal", sessionId: "session_fixture", role: "tool", content: "private", createdAt: "" });
  const first = entriesFromMessages(messages);
  const second = entriesFromMessages(messages.map((message) => ({ ...message })));
  assert.equal(first.length, 500);
  assert.deepEqual(ids(first), Array.from({ length: 500 }, (_, index) => `persisted_${index + 30}`));
  assert.deepEqual(ids(first), ids(second));
});

test("hydrating old callers assigns missing ids once and preserves existing durable ids", () => {
  const durable: Entry = { kind: "user", entryId: "persisted_user", text: "same" };
  const first = appendEntries([], [durable, { kind: "system", text: "same" }]);
  const second = appendEntries([], first);
  assert.equal(first[0], durable);
  assert.equal(second[0], first[0]);
  assert.equal(second[1], first[1]);
  assert.deepEqual(ids(second), ids(first));
});

test("real buffered streaming and terminal snapshots preserve ids through multiple rounds of one run", () => {
  let entries = appendEntries([], [{ kind: "user", entryId: "msg_fixture", text: "work" }]);
  const stream = new AgentRunStream((deltas, identity) => { entries = appendStreamBatch(entries, deltas, identity); });
  try {
    assert.ok(stream.begin("run_fixture", { model: "configured-model" }));
    stream.started("run_fixture");
    stream.enqueue("run_fixture", "assistant", "same");
    stream.flush();
    const firstId = entries.at(-1)?.entryId;
    stream.enqueue("run_fixture", "assistant", " tail");
    stream.boundary();
    assert.equal(entries.at(-1)?.entryId, firstId);
    entries = appendToolCall(entries, call);
    stream.enqueue("run_fixture", "assistant", "same");
    const finished = stream.finish("run_fixture");
    assert.ok(finished);
    const secondId = entries.at(-1)?.entryId;
    entries = settleStreamText(entries, "same\n", finished.scope.identity, finished.streamedText);
    assert.notEqual(firstId, secondId);
    assert.equal(entries.at(-1)?.entryId, secondId);
    assert.equal(new Set(ids(entries)).size, entries.length);
  } finally { stream.reset(); }
});
