import assert from "node:assert/strict";
import test from "node:test";
import { conversationReplyCopyText } from "../src/lib/conversation-reply-copy.js";
const document = JSON.stringify({ version: 1, widgets: [{ type: "card", title: "结论", body: "保留正文\n第二行" }, { type: "progress", title: "已读取", value: 0, max: 7 }] });
test("copying a reply gives readable widget values while preserving surrounding Markdown", () => {
  const source = `  前言\r\n\r\n\`\`\`yukinal-ui\r\n${document}\r\n\`\`\`\r\n\r\n**尾部**  `;
  assert.equal(conversationReplyCopyText(source), "  前言\r\n\r\n结论\n保留正文\n第二行\n\n已读取：0 / 7\r\n\r\n**尾部**  ");
});
test("ordinary code, invalid display payloads and fenced examples retain their exact source", () => {
  for (const source of ["\t  正文\r\n", `\`\`\`json\n${document}\n\`\`\`\n`, '```yukinal-ui\n{"version":2}\n```', '````markdown\n```yukinal-ui\n' + document + '\n```\n````\n', '```sh\r\n\tprintf test\r\n```\r\n']) assert.equal(conversationReplyCopyText(source), source);
});
test("a complete widget can be copied before its fence closes; an incomplete one remains available as source", () => {
  assert.equal(conversationReplyCopyText('```yukinal-ui\n' + document), "结论\n保留正文\n第二行\n\n已读取：0 / 7");
  const partial = '```yukinal-ui\n{"version":1,';
  assert.equal(conversationReplyCopyText(partial), partial);
});
test("steps and metrics copy only supplied states, values and sample points", () => {
  const source = '~~~yukinal-ui\n' + JSON.stringify({ version: 1, widgets: [{ type: "steps", title: "步骤", items: [{ title: "一", status: "done" }, { title: "二", status: "active", detail: "实测" }] }, { type: "metrics", title: "指标", items: [{ label: "CPU", value: 0, unit: "%", trend: [2, 1, 0] }, { label: "内存", value: "未上报" }] }, { type: "details", title: "详情", body: "原文" }] }) + '\n~~~';
  assert.equal(conversationReplyCopyText(source), "步骤\n[已完成] 一\n[进行中] 二\n  实测\n\n指标\nCPU：0 %\n  趋势：2 → 1 → 0\n内存：未上报\n\n详情\n原文");
});

test("large ordinary code blocks copy without an argument-spread stack overflow", () => {
  const source = "```text\n" + "line\n".repeat(100_000) + "```\n";
  assert.equal(conversationReplyCopyText(source), source);
});
