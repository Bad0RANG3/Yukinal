import assert from "node:assert/strict";
import test from "node:test";
import { conversationTurns, currentConversationTurn as current, conversationTurnTarget as target } from "../src/features/agent/conversation-scroll.js";
import type { Entry } from "../src/features/agent/transcript.js";

test("one user message is one turn regardless of assistant length, reasoning or tools", () => {
  const entries: Entry[] = [{ kind: "user", entryId: "u1", text: "检查服务器" }, { kind: "assistant", text: "a".repeat(50_000) }, { kind: "reasoning", text: "thinking" }, { kind: "tool", callId: "c", toolName: "server.info", target: "staging", riskLevel: "read", decision: "auto" }, { kind: "assistant", text: "结果" }, { kind: "user", entryId: "u2", text: "看看日志" }];
  const turns = conversationTurns(entries, "chat");
  assert.equal(turns.length, 2); assert.equal(turns[0]!.id, "chat-turn-u1"); assert.equal(turns[1]!.entryIndex, 5);
  assert.equal(conversationTurns([{ kind: "assistant", text: "orphan" }], "chat").length, 0);
});

test("anchors retain identity across history trimming, duplicate prompt text, and text edits", () => {
  const entries: Entry[] = [{ kind: "user", entryId: "first", text: "重复" }, { kind: "assistant", text: "回答" }, { kind: "user", entryId: "second", text: "重复" }];
  assert.notEqual(conversationTurns(entries, "a")[0]!.id, conversationTurns(entries, "a")[1]!.id);
  assert.equal(conversationTurns(entries.slice(2), "a")[0]!.id, "a-turn-second");
  assert.notEqual(conversationTurns(entries, "a")[0]!.id, conversationTurns(entries, "b")[0]!.id);
});

test("turn previews use bounded user text or attachment count, not assistant reasoning", () => {
  const turns = conversationTurns([{ kind: "user", text: "  hello\n world   " }, { kind: "user", text: "x".repeat(1000) }, { kind: "user", text: "", attachments: [{ type: "file", name: "note.txt", mediaType: "text/plain", data: "private body" }] }, { kind: "user", text: "" }], "scope");
  assert.equal(turns[0]!.preview, "hello world"); assert.equal(turns[1]!.preview.length, 180); assert.equal(turns[2]!.preview, "附加了 1 个文件"); assert.equal(turns[3]!.preview, "用户消息");
});

test("reading position uses actual message anchors rather than equal text-height intervals", () => {
  const tops = [24, 300, 8000];
  assert.equal(current(tops, 0, 500, 10000), 0);
  assert.equal(current(tops, 290, 500, 10000), 1);
  assert.equal(current(tops, 7000, 500, 10000), 1);
  assert.equal(current(tops, 8000, 500, 10000), 2);
  assert.equal(current(tops, 9500, 500, 10000), 2);
  assert.equal(current([], 0, 500, 500), -1);
});

test("anchor jumps clamp to real scroll bounds and keep a small reading inset", () => {
  assert.equal(target(24, 500, 5000), 12);
  assert.equal(target(1000, 500, 5000), 988);
  assert.equal(target(4999, 500, 5000), 4500);
  assert.equal(target(24, 1000, 500), 0);
});
