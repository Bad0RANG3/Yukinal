import assert from "node:assert/strict";
import test from "node:test";
import { Children, isValidElement, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";

import { ConversationWidgets } from "../src/components/ConversationWidgets.js";
import { parseConversationWidgets, type ConversationWidget, type ConversationWidgetDocument } from "../src/lib/conversation-widgets.js";

const documentOf = (...widgets: ConversationWidget[]): ConversationWidgetDocument => ({ version: 1, widgets });
const render = (document: ConversationWidgetDocument, streaming = false) => renderToStaticMarkup(<ConversationWidgets document={document} streaming={streaming} />);
function elements(node: ReactNode): ReactElement<Record<string, unknown>>[] {
  return Children.toArray(node).flatMap((child) => isValidElement<Record<string, unknown>>(child)
    ? [child, ...elements(child.props.children as ReactNode)] : []);
}

test("AI summary data remains escaped text with no executable elements, links or host actions", () => {
  const body = '<script>alert("fixture")</script>\n<img src="https://example.invalid/image" onerror="alert(1)">\n[open](javascript:alert(1))';
  const data = documentOf({ type: "card", title: '<b onclick="fixture()">标题</b>', body, tone: "warning" },
    { type: "details", title: "普通详情", body: '<iframe src="https://example.invalid"></iframe>' });
  const before = JSON.stringify(data);
  const markup = render(data);
  assert.match(markup, /AI 摘要/);
  assert.match(markup, /title="由本条 AI 回复提供的展示数据"/);
  assert.ok(markup.includes("&lt;script&gt;"));
  assert.ok(markup.includes("&lt;img src="));
  assert.ok(markup.includes("&lt;iframe src="));
  assert.doesNotMatch(markup, /<(?:script|img|iframe|a|button|input|form)\b|<[a-z][^>]*\son(?:click|error)=/i);
  assert.doesNotMatch(markup, /approval-actions|approved|已获批准|已验证/);
  assert.equal(JSON.stringify(data), before, "display must not mutate the provided document");
});

test("step statistics and visible status labels account for exactly the supplied items", () => {
  const data = documentOf({ type: "steps", title: "AI 整理的步骤", items: [
    { title: "第一项", status: "done", detail: "给定说明" },
    { title: "第二项", status: "active" },
    { title: "第三项", status: "pending" },
    { title: "第四项", status: "error" },
    { title: "第五项", status: "done" },
  ] });
  const markup = render(data);
  assert.match(markup, /共 5 项 · 已完成 2 · 进行中 1 · 待处理 1 · 异常 1/);
  assert.equal((markup.match(/<li\b/g) ?? []).length, 5);
  assert.equal((markup.match(/data-step-status="done"/g) ?? []).length, 2);
  for (const label of ["已完成", "进行中", "待处理", "异常", "给定说明"]) assert.ok(markup.includes(label));
  assert.doesNotMatch(markup, /耗时|倒计时|重试|执行命令|批准/);
});

test("native progress preserves fractional values and the supplied maximum across streaming state", () => {
  const data = documentOf({ type: "progress", title: "给定进度", value: 7.5, max: 12.5, detail: "由回复提供" });
  const first = render(data, true);
  const settled = render(data);
  const tag = (markup: string) => /<progress\b[^>]*>/.exec(markup)?.[0];
  assert.equal(tag(first), tag(settled));
  assert.match(first, /<progress value="7\.5" max="12\.5" aria-label="给定进度" aria-valuetext="7\.5 \/ 12\.5">/);
  assert.match(first, /7\.5 \/ 12\.5/);
  assert.match(first, /生成中/);
  assert.match(first, /aria-busy="true"/);
  assert.doesNotMatch(settled, /生成中|aria-busy/);
  assert.doesNotMatch(first, /%|倒计时|预计|秒/);
  assert.match(render(documentOf({ type: "progress", title: "未开始", value: 0, max: 8 })), /<progress value="0" max="8"/);
});

test("defensive progress rendering never lets native clamping invent a valid result", () => {
  const markup = render(documentOf({ type: "progress", title: "越界输入", value: 9, max: 5 }));
  assert.match(markup, /9 \/ 5/);
  assert.doesNotMatch(markup, /<progress\b|100%|已完成/);
});

test("tiny progress against a huge maximum stays the supplied incomplete value without ratio rounding", () => {
  const value = Number.MIN_VALUE;
  const max = Number.MAX_VALUE;
  const markup = render(documentOf({ type: "progress", title: "小进度", value, max }), true);
  assert.ok(markup.includes(`<progress value="${value}" max="${max}"`));
  assert.ok(markup.includes(`aria-valuetext="${value} / ${max}"`));
  assert.doesNotMatch(markup, /100%|已完成|NaN|Infinity/);
});

test("metrics preserve zero and text values and draw only the actual supplied trend samples", () => {
  const data = documentOf({ type: "metrics", title: "AI 汇总", items: [
    { label: "读数", value: 0, unit: "ms", trend: [0, 5, 10] },
    { label: "说明", value: "待核对" },
    { label: "单点", value: 7, trend: [7] },
    { label: "空点列", value: 3, trend: [] },
  ] });
  const before = JSON.stringify(data);
  const markup = render(data);
  assert.match(markup, /conversation-widget-metric-value">0<\/span><small>ms/);
  assert.match(markup, /待核对/);
  assert.equal((markup.match(/<polyline\b/g) ?? []).length, 1);
  assert.match(markup, /points="2,30 60,16 118,2"/);
  assert.match(markup, /AI 提供的 3 个数据点/);
  assert.match(markup, /数据点：0、5、10/);
  assert.doesNotMatch(markup, /暂无趋势|未提供趋势|最近\d+|分钟|采集成功/);
  assert.equal(JSON.stringify(data), before);
});

test("flat, negative and extreme finite sample ranges keep each supplied point finite", () => {
  const markup = render(documentOf({ type: "metrics", title: "点列", items: [
    { label: "恒定", value: 4, trend: [4, 4, 4] },
    { label: "负数", value: 3, trend: [-3, 0, 3] },
    { label: "大数", value: "范围", trend: [-Number.MAX_VALUE, 0, Number.MAX_VALUE] },
  ] }));
  const lines = Array.from(markup.matchAll(/<polyline points="([^"]+)"/g), (match) => match[1]);
  assert.deepEqual(lines, ["2,16 60,16 118,16", "2,30 60,16 118,2", "2,30 60,16 118,2"]);
  assert.doesNotMatch(markup, /NaN|Infinity/);
});

test("details use a closed native disclosure with the literal provided title and body", () => {
  const markup = render(documentOf({ type: "details", title: "可展开详情", body: "line one\n<em>line two</em>" }));
  assert.match(markup, /<details class="conversation-widget conversation-widget-details"><summary>/);
  assert.match(markup, /<span>可展开详情<\/span><\/summary>/);
  assert.ok(markup.includes("line one\n&lt;em&gt;line two&lt;/em&gt;"));
  assert.doesNotMatch(markup, /\bopen=|role="button"|tabindex="-1"|<em>/);
});

test("token text and streaming updates keep widget identities stable rather than remounting animations", () => {
  const first = documentOf({ type: "card", title: "摘要", body: "A", tone: "neutral" }, { type: "details", title: "详情", body: "B" });
  const next = documentOf({ type: "card", title: "摘要更新", body: "AB", tone: "neutral" }, { type: "details", title: "详情更新", body: "BC" });
  const firstTree = ConversationWidgets({ document: first, streaming: true });
  const nextTree = ConversationWidgets({ document: next, streaming: false });
  const widgetKeys = (tree: ReactNode) => elements(tree).filter((element) => Object.hasOwn(element.props, "widget")).map((element) => element.key);
  assert.deepEqual(widgetKeys(firstTree), widgetKeys(nextTree));
  assert.equal(widgetKeys(firstTree).length, 2);
  assert.equal(firstTree?.type, nextTree?.type);
  assert.match(render(first, true), /class="conversation-widgets is-entering"/);
  assert.doesNotMatch(render(first), /\bis-entering\b/, "loading saved history must not replay entrance motion");
  assert.doesNotMatch(render(next, false), /\bis-entering\b/, "completed reply replay has no entrance marker");
});

test("normalized parser output renders without adding metrics, trends or progress to a card-only reply", () => {
  const parsed = parseConversationWidgets(JSON.stringify({ version: 1, widgets: [{ type: "card", title: "摘要", body: "只有给定内容" }] }));
  assert.equal(parsed.status, "ready");
  if (parsed.status !== "ready") assert.fail("valid fixture must normalize");
  const markup = render(parsed.document, true);
  assert.match(markup, /只有给定内容|生成中/);
  assert.doesNotMatch(markup, /<progress\b|<polyline\b|\d+%|已完成/);
  assert.equal(render(documentOf()), "");
});
