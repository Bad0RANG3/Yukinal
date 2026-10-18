import assert from "node:assert/strict";
import test from "node:test";
import { parseConversationWidgets, MAX_CONVERSATION_WIDGET_SOURCE } from "../src/lib/conversation-widgets.js";

const wrap = (...widgets: unknown[]) => JSON.stringify({ version: 1, widgets });
const card = { type: "card", title: "结果", body: "只来自模型提供的数据" };
test("the five display types retain actual values without synthesizing progress or samples", () => {
  const source = wrap(card, { type: "steps", title: "计划", items: [{ title: "检查", status: "active" }] }, { type: "metrics", title: "用量", items: [{ label: "内存", value: 0, unit: "MiB", trend: [-1, 0, 2] }] }, { type: "progress", title: "已读取", value: 2, max: 7 }, { type: "details", title: "详情", body: "  原始\n文本\t " });
  const parsed = parseConversationWidgets(source);
  assert.equal(parsed.status, "ready");
  if (parsed.status !== "ready") return;
  assert.equal(parsed.document.widgets.length, 5);
  assert.deepEqual(parsed.document.widgets[0], { ...card, tone: "neutral" });
  assert.deepEqual(parsed.document.widgets[2], { type: "metrics", title: "用量", items: [{ label: "内存", value: 0, unit: "MiB", trend: [-1, 0, 2] }] });
  assert.deepEqual(parsed.document.widgets[3], { type: "progress", title: "已读取", value: 2, max: 7 });
  assert.deepEqual(parsed.document.widgets[4], { type: "details", title: "详情", body: "  原始\n文本\t " });
});
test("partial JSON is distinguishable from a complete unsupported shape", () => {
  assert.deepEqual(parseConversationWidgets('{"version":1,"widgets":['), { status: "invalid", reason: "json" });
  assert.deepEqual(parseConversationWidgets('{"version":2,"widgets":[]}'), { status: "invalid", reason: "shape" });
  assert.deepEqual(parseConversationWidgets(' '.repeat(MAX_CONVERSATION_WIDGET_SOURCE + 1)), { status: "invalid", reason: "size" });
});
test("unknown actions, HTML payload types, event handlers and prototype keys cannot enter the UI contract", () => {
  for (const value of [{ ...card, onClick: "delete" }, { type: "html", title: "x", body: "<script>run()</script>" }, { ...card, href: "javascript:run()" }, { type: "button", title: "执行", command: "run" }, { ...card, dangerouslySetInnerHTML: { __html: "<b>x</b>" } }]) {
    assert.equal(parseConversationWidgets(wrap(value)).status, "invalid");
  }
  assert.equal(parseConversationWidgets('{"version":1,"widgets":[{"type":"card","title":"x","body":"x","__proto__":{}}]}').status, "invalid");
  assert.equal(parseConversationWidgets(JSON.stringify({ version: 1, widgets: [card], fetch: "https://invalid.example" })).status, "invalid");
});
test("malformed or unbounded values fall back together rather than presenting partial misleading results", () => {
  for (const value of [
    { ...card, title: " " }, { ...card, title: "字".repeat(161) }, { ...card, body: "x".repeat(8001) }, { ...card, tone: "approved" },
    { type: "steps", title: "x", items: [{ title: "x", status: "success" }] },
    { type: "steps", title: "x", items: Array.from({ length: 21 }, () => ({ title: "x", status: "pending" })) },
    { type: "metrics", title: "x", items: [{ label: "x", value: {} }] },
    { type: "metrics", title: "x", items: [{ label: "x", value: 1, trend: ["2"] }] },
    { type: "metrics", title: "x", items: [{ label: "x", value: 1, trend: Array(41).fill(1) }] },
    { type: "metrics", title: "x", items: Array(9).fill({ label: "x", value: 1 }) },
    { type: "progress", title: "x", value: -1, max: 3 }, { type: "progress", title: "x", value: 4, max: 3 },
    { type: "progress", title: "x", value: "1", max: 3 }, { type: "progress", title: "x", value: 0, max: 0 },
  ]) assert.equal(parseConversationWidgets(wrap(card, value)).status, "invalid", JSON.stringify(value).slice(0, 150));
  assert.equal(parseConversationWidgets(wrap(...Array(13).fill(card))).status, "invalid");
  assert.equal(parseConversationWidgets('{"version":1,"widgets":[{"type":"progress","title":"x","value":1e400,"max":2}]}').status, "invalid");
});
test("strings stay inert exact text and no implicit trend or progress is added", () => {
  const parsed = parseConversationWidgets(wrap({ ...card, body: '<img src="https://invalid.example/x" onerror="run()">' }, { type: "metrics", title: "指标", items: [{ label: "unknown", value: "未上报" }] }));
  assert.equal(parsed.status, "ready");
  if (parsed.status !== "ready") return;
  assert.deepEqual(parsed.document.widgets[1], { type: "metrics", title: "指标", items: [{ label: "unknown", value: "未上报" }] });
});

test("completed widget items are available before subsequent items finish, with quoted braces kept intact", async () => {
  const { parseStreamingConversationWidgets } = await import("../src/lib/conversation-widgets.js");
  const first = { type: "card", title: "首项", body: '大括号 } 和逗号 , 以及引号 " 都是文本' };
  const prefix = '{"version":1,"widgets":[' + JSON.stringify(first);
  const parsed = parseStreamingConversationWidgets(prefix + ',{"type":"steps","title":"后续');
  assert.ok(parsed);
  assert.equal(parsed.widgets.length, 1);
  assert.deepEqual(parsed.widgets[0], { ...first, tone: "neutral" });
  assert.equal(parseStreamingConversationWidgets('{"version":1,"widgets":[{"type":"button","title":"x"},'), null);
  assert.equal(parseStreamingConversationWidgets(prefix + 'INVALID'), null);
});
