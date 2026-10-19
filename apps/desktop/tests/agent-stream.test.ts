import assert from "node:assert/strict";
import test, { type TestContext } from "node:test";

import {
  AgentRunStream,
  appendStreamBatch,
  MAX_PENDING_STREAM_CHARS,
  MAX_PENDING_STREAM_SEGMENTS,
  settleStreamText,
  STREAM_FLUSH_INTERVAL_MS,
  type RunStreamScope,
  type StreamDelta,
} from "../src/features/agent/run-stream.js";
import { appendEntries, appendToolCall, type Entry, type ReplyIdentity } from "../src/features/agent/transcript.js";


function assertEntryContent(actual: unknown, expected: unknown): void {
  const content = (value: unknown): unknown => {
    if (Array.isArray(value)) return value.map(content);
    if (value && typeof value === "object" && "kind" in value) {
      const { entryId: _entryId, ...rest } = value as Record<string, unknown>;
      return rest;
    }
    return value;
  };
  assert.deepEqual(content(actual), expected);
}

const identity: ReplyIdentity = { participant: { id: "coordinator", label: "协调助手", kind: "agent" }, model: "configured-model" };

function view(context: TestContext) {
  let entries: Entry[] = [{ kind: "user", text: "hello" }];
  const batches: (readonly StreamDelta[])[] = [];
  const stream = new AgentRunStream((deltas, capturedIdentity) => {
    batches.push(deltas);
    entries = appendStreamBatch(entries, deltas, capturedIdentity);
  });
  context.after(() => stream.reset());
  return {
    stream,
    batches,
    get entries() { return entries; },
    set entries(next: Entry[]) { entries = next; },
    begin(runId = "run_a", speaker = identity): RunStreamScope {
      const scope = stream.begin(runId, speaker);
      assert.ok(scope);
      scope.sessionId = `session_${runId}`;
      assert.equal(stream.started(runId), true);
      return scope;
    },
    complete(runId: string, text: string): boolean {
      const finished = stream.finish(runId);
      if (!finished) return false;
      entries = settleStreamText(entries, text, finished.scope.identity, finished.streamedText);
      return true;
    },
  };
}

test("a burst of real deltas produces one bounded-delay update without normalizing code or Unicode", (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const current = view(context);
  current.begin();
  const deltas = ["\tcode() {\r\n", ...Array.from({ length: 1000 }, (_, i) => `${i}你🙂 `), "\r\n}\r\n\r\n"];
  for (const text of deltas) current.stream.enqueue("run_a", "assistant", text);
  assert.equal(current.batches.length, 0);
  context.mock.timers.tick(STREAM_FLUSH_INTERVAL_MS - 1);
  assert.equal(current.batches.length, 0);
  context.mock.timers.tick(1);
  assert.equal(current.batches.length, 1, "one transcript updater instead of one per token");
  assertEntryContent(current.entries.at(-1), { kind: "assistant", text: deltas.join(""), ...identity });
  context.mock.timers.tick(STREAM_FLUSH_INTERVAL_MS * 5);
  assert.equal(current.batches.length, 1, "an idle buffer does not schedule periodic rendering");
});

test("text and thinking alternate in arrival order within one frame and retain the captured speaker", (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const current = view(context);
  current.begin();
  for (const [kind, text] of [["reasoning", "想"], ["reasoning", "一想"], ["assistant", "答"], ["reasoning", "再想"], ["assistant", "案"]] as const) {
    current.stream.enqueue("run_a", kind, text);
  }
  context.mock.timers.tick(STREAM_FLUSH_INTERVAL_MS);
  assert.equal(current.batches.length, 1);
  assertEntryContent(current.entries.slice(1), [
    { kind: "reasoning", text: "想一想" },
    { kind: "assistant", text: "答", ...identity },
    { kind: "reasoning", text: "再想" },
    { kind: "assistant", text: "案", ...identity },
  ]);
  const before = current.entries;
  assert.equal(current.complete("run_a", "答案"), true);
  assert.equal(current.entries, before, "an equal final snapshot never repeats the full answer after alternating segments");
});

test("tool and approval boundaries publish the current tail before a structural row", (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const current = view(context);
  current.begin();
  current.stream.enqueue("run_a", "assistant", "先核对");
  current.stream.boundary();
  current.entries = appendToolCall(current.entries, { callId: "call_a", toolName: "readonly.test", target: "fixture", riskLevel: "read", decision: "auto" });
  assert.deepEqual(current.entries.map((entry) => entry.kind), ["user", "assistant", "tool"]);
  current.stream.enqueue("run_a", "assistant", "继续");
  current.stream.boundary();
  current.entries = appendEntries(current.entries, [{ kind: "system", text: "fixture approval boundary" }]);
  assert.deepEqual(current.entries.map((entry) => entry.kind), ["user", "assistant", "tool", "assistant", "system"]);
  context.mock.timers.tick(1000);
  assert.equal(current.batches.length, 2, "the original delayed callback cannot append behind the tool/approval row");
});

test("completion flushes its last chunk immediately and duplicate terminal snapshots cannot settle twice", (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const current = view(context);
  current.begin();
  current.stream.enqueue("run_a", "assistant", "最后一块\n");
  assert.equal(current.complete("run_a", "最后一块\n"), true);
  assert.equal(current.batches.length, 1);
  assert.equal(current.entries.length, 2);
  assert.equal(current.complete("run_a", "最后一块\n"), false);
  current.stream.enqueue("run_a", "assistant", "late duplicate");
  context.mock.timers.tick(1000);
  assert.equal(current.batches.length, 1);
  assertEntryContent(current.entries.at(-1), { kind: "assistant", text: "最后一块\n", ...identity });
});

test("a terminal snapshot recovers only its missing suffix across interleaved thinking", (context) => {
  const current = view(context);
  current.begin();
  current.stream.enqueue("run_a", "assistant", "答");
  current.stream.enqueue("run_a", "reasoning", "公开状态");
  assert.equal(current.complete("run_a", "答案\n"), true);
  assertEntryContent(current.entries.slice(1), [
    { kind: "assistant", text: "答", ...identity },
    { kind: "reasoning", text: "公开状态" },
    { kind: "assistant", text: "案\n", ...identity },
  ]);
});

test("the final model turn cannot absorb or duplicate a pre-tool explanation", (context) => {
  const current = view(context);
  current.begin();
  current.stream.enqueue("run_a", "assistant", "先查看证据。");
  current.stream.boundary();
  current.entries = appendToolCall(current.entries, { callId: "c", toolName: "fixture.read", target: "local", riskLevel: "read", decision: "auto" });
  current.stream.boundary();
  current.stream.enqueue("run_a", "assistant", "证据确认。");
  current.complete("run_a", "证据确认。\n");
  assert.deepEqual(current.entries.filter((entry) => entry.kind === "assistant").map((entry) => entry.text), ["先查看证据。", "证据确认。\n"]);
});

test("the character high-water mark publishes early without dropping bytes or scheduling unbounded batches", (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const current = view(context);
  current.begin();
  const chunk = "🙂\t\r\n".repeat(4000); // 20,000 UTF-16 units, within the actual IPC event limit.
  for (let i = 0; i < 3; i++) current.stream.enqueue("run_a", "assistant", chunk);
  assert.equal(current.batches.length, 2);
  current.complete("run_a", chunk.repeat(3));
  assert.equal(current.batches.length, 3);
  for (const batch of current.batches) assert.ok(batch.reduce((length, delta) => length + delta.text.length, 0) <= MAX_PENDING_STREAM_CHARS);
  assertEntryContent(current.entries.at(-1), { kind: "assistant", text: chunk.repeat(3), ...identity });
  context.mock.timers.tick(1000);
  assert.equal(current.batches.length, 3);
});

test("rapid alternating kinds hit a finite segment bound and preserve every segment", (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const current = view(context);
  current.begin();
  for (let i = 0; i < MAX_PENDING_STREAM_SEGMENTS; i++) current.stream.enqueue("run_a", i % 2 ? "assistant" : "reasoning", `${i};`);
  assert.equal(current.batches.length, 1);
  assert.equal(current.batches[0]?.length, MAX_PENDING_STREAM_SEGMENTS);
  assert.equal(current.entries.length, MAX_PENDING_STREAM_SEGMENTS + 1);
});

test("an oversized caller delta bypasses the pending cache verbatim", (context) => {
  const current = view(context);
  current.begin();
  const text = "🙂\r\n".repeat(MAX_PENDING_STREAM_CHARS);
  current.stream.enqueue("run_a", "assistant", text);
  assert.equal(current.batches.length, 1);
  assertEntryContent(current.entries.at(-1), { kind: "assistant", text, ...identity });
});

test("foreign run chunks and starts cannot claim the admitted slot", (context) => {
  const current = view(context);
  const scope = current.stream.begin("run_owned", identity);
  assert.ok(scope);
  assert.equal(current.stream.begin("run_other", identity), null);
  assert.equal(current.stream.started("run_other"), false);
  assert.equal(current.stream.finish("run_other"), null);
  current.stream.enqueue("run_other", "assistant", "wrong");
  current.stream.enqueue("run_owned", "assistant", "before started");
  assert.equal(current.stream.started("run_owned"), true);
  current.stream.enqueue("run_owned", "assistant", "owned");
  current.stream.flush();
  assertEntryContent(current.entries.at(-1), { kind: "assistant", text: "owned", ...identity });
});

test("a terminal event before RPC acknowledgement releases the slot and the late acknowledgement cannot claim a new run", (context) => {
  const current = view(context);
  const old = current.begin();
  current.complete("run_a", "finished");
  const next = current.stream.begin("run_b", identity);
  assert.ok(next, "terminal arrival need not wait for the slower start RPC");
  assert.equal(current.stream.acknowledge(old), false);
  assert.equal(current.stream.isPending(next), true);
  assert.equal(current.stream.started("run_b"), true);
  assert.equal(current.stream.finish("run_a"), null);
});

test("retrying an admitted runId still isolates the old operation by scope identity", (context) => {
  const current = view(context);
  const old = current.begin("run_retried");
  current.stream.fail(old);
  const retry = current.stream.begin("run_retried", identity);
  assert.ok(retry);
  assert.notEqual(old, retry);
  assert.equal(current.stream.isCurrent(old), false);
  assert.equal(current.stream.fail(old), null);
  assert.equal(current.stream.acknowledge(old), false);
  assert.equal(current.stream.isPending(retry), true);
});

test("resetting or unmounting a view discards only its pending tail and rejects all late operations", (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const current = view(context);
  const old = current.begin();
  current.stream.enqueue("run_a", "assistant", "old pending");
  current.stream.reset();
  current.entries = [{ kind: "user", text: "new session" }];
  current.begin("run_b");
  current.stream.enqueue("run_a", "assistant", "late old text");
  assert.equal(current.stream.isCurrent(old), false);
  assert.equal(current.stream.isPending(old), false);
  assert.equal(current.stream.finish("run_a"), null);
  assert.equal(current.stream.acknowledge(old), false);
  context.mock.timers.tick(STREAM_FLUSH_INTERVAL_MS);
  assert.equal(current.batches.length, 0);
  current.stream.enqueue("run_b", "assistant", "new answer");
  current.stream.flush();
  assertEntryContent(current.entries, [{ kind: "user", text: "new session" }, { kind: "assistant", text: "new answer", ...identity }]);
});

test("late stop/approval promises cannot finish or write to the next conversation", async (context) => {
  const current = view(context);
  const old = current.begin();
  let resolve!: () => void;
  const pending = new Promise<void>((done) => { resolve = done; });
  const lateStop = pending.then(() => {
    if (!current.stream.isPending(old)) return;
    current.stream.finish(old.runId);
    current.entries = appendEntries(current.entries, [{ kind: "error", text: "old stop" }]);
  });
  const lateApproval = pending.then(() => {
    if (!current.stream.isCurrent(old)) return;
    current.entries = appendEntries(current.entries, [{ kind: "system", text: "old approval" }]);
  });
  current.stream.reset();
  const next = current.begin("run_b");
  resolve();
  await Promise.all([lateStop, lateApproval]);
  assert.equal(current.stream.isPending(next), true);
  assertEntryContent(current.entries, [{ kind: "user", text: "hello" }]);
});

test("failure and cancellation publish the last partial text before becoming terminal", (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const current = view(context);
  const first = current.begin();
  current.stream.enqueue("run_a", "assistant", "partial before failure");
  assert.equal(current.stream.fail(first), first);
  assert.equal(current.stream.isPending(first), false);
  assertEntryContent(current.entries.at(-1), { kind: "assistant", text: "partial before failure", ...identity });
  current.entries = appendEntries(current.entries, [{ kind: "user", text: "retry" }]);
  current.begin("run_b");
  current.stream.enqueue("run_b", "assistant", "partial before cancel");
  assert.ok(current.stream.finish("run_b"));
  context.mock.timers.tick(1000);
  assertEntryContent(current.entries.at(-1), { kind: "assistant", text: "partial before cancel", ...identity });
  assert.equal(current.batches.length, 2);
});

test("the synchronous terminal fallback settles exactly once even without a started event", (context) => {
  const current = view(context);
  const scope = current.stream.begin("run_sync", identity);
  assert.ok(scope);
  scope.sessionId = "session_sync";
  const finished = current.stream.finish("run_sync");
  assert.ok(finished);
  assert.equal(finished.scope.sessionId, "session_sync");
  current.entries = settleStreamText(current.entries, "sync result", finished.scope.identity, finished.streamedText);
  assert.equal(current.stream.finish("run_sync"), null);
  assert.equal(current.stream.isPending(scope), false);
  assertEntryContent(current.entries.at(-1), { kind: "assistant", text: "sync result", ...identity });
});

test("deferred transcript updaters use each run's immutable identity instead of a later mutable ref", (context) => {
  const updates: ((entries: Entry[]) => Entry[])[] = [];
  const stream = new AgentRunStream((deltas, captured) => updates.push((entries) => appendStreamBatch(entries, deltas, captured)));
  context.after(() => stream.reset());
  const firstIdentity: ReplyIdentity = { participant: { id: "one", label: "One", kind: "agent" }, model: "model-one" };
  const first = stream.begin("run_one", firstIdentity);
  assert.ok(first);
  stream.started("run_one");
  stream.enqueue("run_one", "assistant", "one");
  stream.finish("run_one");
  firstIdentity.participant!.label = "mutated externally";
  const second = stream.begin("run_two", { participant: { id: "two", label: "Two", kind: "agent" }, model: "model-two" });
  assert.ok(second);
  stream.started("run_two");
  stream.enqueue("run_two", "assistant", "two");
  stream.finish("run_two");
  const entries = updates.reduce((current, update) => update(current), [] as Entry[]);
  assertEntryContent(entries, [
    { kind: "assistant", text: "one", participant: { id: "one", label: "One", kind: "agent" }, model: "model-one" },
    { kind: "assistant", text: "two", participant: { id: "two", label: "Two", kind: "agent" }, model: "model-two" },
  ]);
});

test("a divergent or shorter terminal snapshot never erases the streamed evidence", () => {
  const entries: Entry[] = [{ kind: "assistant", text: "already visible", ...identity }];
  assert.equal(settleStreamText(entries, "already", identity, "already visible"), entries);
  assertEntryContent(settleStreamText(entries, "different final", identity, "already visible"), [
    ...entries, { kind: "assistant", text: "different final", ...identity },
  ]);
});
