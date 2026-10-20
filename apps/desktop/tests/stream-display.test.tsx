import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";
import { MarkdownText } from "../src/components/MarkdownText.js";
import { AgentEntryView } from "../src/features/agent/AgentEntryView.js";

const document = JSON.stringify({ version: 1, widgets: [{ type: "card", title: "检查结果", body: "三项指标均已返回" }, { type: "progress", title: "已读取", value: 2, max: 7 }] });
const fence = (source: string, closed = true, language = "yukinal-ui") => `\`\`\`${language}\n${source}${closed ? '\n```' : ''}`;
const markdown = (source: string, streaming = false) => renderToStaticMarkup(<MarkdownText text={source} streaming={streaming} />);
const entry = (text: string, streaming = false) => renderToStaticMarkup(<AgentEntryView entry={{ kind: "assistant", text }} streaming={streaming} onApproval={async () => {}} approvalBusy={false} />);

test("complete explicit widget payloads render inline before the closing fence arrives and survive history restore", () => {
  const live = markdown(fence(document, false), true);
  assert.match(live, /AI 摘要/);
  assert.match(live, /检查结果/);
  assert.match(live, /<progress[^>]*value="2"[^>]*max="7"|<progress[^>]*max="7"[^>]*value="2"/);
  assert.doesNotMatch(live, /md-code-language|agent-widget-fallback/);
  const history = markdown(fence(document));
  assert.match(history, /检查结果/);
  assert.doesNotMatch(history, /正在生成|生成中|正在输出/);
});
test("partial structured streams expose their raw prefix and do not flash schema errors", () => {
  const prefix = '{"version":1,"widgets":[{"type":"card","title":"未完成';
  const live = markdown(fence(prefix, false), true);
  assert.match(live, /正在生成展示内容/);
  assert.match(live, /查看已生成内容/);
  assert.match(live, /未完成/);
  assert.doesNotMatch(live, /未能解析|AI 摘要/);
  const ended = markdown(fence(prefix, false));
  assert.match(ended, /展示内容未能解析，原文已保留/);
  assert.match(ended, /未完成/);
});
test("invalid or executable widget requests remain inert visible text instead of silently disappearing", () => {
  const unsafe = JSON.stringify({ version: 1, widgets: [{ type: "button", title: '<img src="https://invalid.example/x">', command: "erase" }] });
  const markup = markdown(fence(unsafe));
  assert.match(markup, /原文已保留/);
  assert.match(markup, /erase/);
  assert.match(markup, /&lt;img/);
  assert.doesNotMatch(markup, /<img|<script|<iframe|onerror=/);
});
test("ordinary JSON code is never promoted to widgets and normal Markdown remains intact around cards", () => {
  assert.match(markdown(fence(document, true, "json")), /md-code-block/);
  assert.doesNotMatch(markdown(fence(document, true, "json")), /AI 摘要/);
  const markup = markdown(`前言\n\n${fence(document)}\n\n**结论保留**`);
  assert.match(markup, /前言/);
  assert.match(markup, /检查结果/);
  assert.match(markup, /<strong>(?:<span>)?结论保留(?:<\/span>)?<\/strong>/);
});
test("streaming reply status and copy labels follow actual liveness, including structured content", () => {
  const live = entry(fence(document, false), true);
  assert.match(live, /正在输出/);
  assert.match(live, /aria-label="复制当前已生成内容"/);
  assert.match(live, /AI 摘要/);
  const settled = entry(fence(document));
  assert.doesNotMatch(settled, /正在输出|复制当前已生成内容/);
  assert.match(settled, /aria-label="复制完整回答"/);
  const thinking = renderToStaticMarkup(<AgentEntryView entry={{ kind: "reasoning", text: "正在检查输入" }} streaming onApproval={async () => {}} approvalBusy={false} />);
  assert.match(thinking, /正在思考/);
  assert.match(thinking, /aria-busy="true"/);
  assert.doesNotMatch(thinking, /\bopen=/);
});
test("quiet tool rows retain the actual target and outcome with user controlled disclosure", () => {
  const markup = renderToStaticMarkup(<AgentEntryView entry={{ kind: "tool", callId: "call_fixture", toolName: "filesystem.list", target: "local:/workspace", riskLevel: "read", decision: "auto", result: { status: "failed", durationMs: 12, summary: "实际错误摘要" } }} onApproval={async () => {}} approvalBusy={false} />);
  assert.match(markup, /<summary/);
  assert.match(markup, /local:\/workspace/);
  assert.match(markup, /12\s*ms/);
  assert.match(markup, /实际错误摘要/);
  assert.doesNotMatch(markup, /\bopen=/);
});

test("earlier cards stay visible while a later card is still being streamed", () => {
  const prefix = '{"version":1,"widgets":[{"type":"card","title":"已收到的卡片","body":"实际数据"},{"type":"details","title":"未结束';
  const markup = markdown(fence(prefix, false), true);
  assert.match(markup, /已收到的卡片/);
  assert.match(markup, /实际数据/);
  assert.match(markup, /正在生成其余内容/);
  assert.doesNotMatch(markup, /未能解析/);
});
