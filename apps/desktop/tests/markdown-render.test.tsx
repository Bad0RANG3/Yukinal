/**
 * 渲染层的硬规则，钉死在静态标记上。
 *
 * 这三条都不是「好看不好看」的问题，而是安全与可用性的边界：
 *   - 正文里的 HTML 必须是**文字**（没有 `dangerouslySetInnerHTML`，也没有过滤规则
 *     需要维护对错）；
 *   - 链接不能生成可导航的 `<a href>` —— 窗口没有 opener 能力，点一下就会把 webview
 *     导航走，整个界面没了；
 *   - 图片不会变成 `<img>`，因此一条消息不会替用户向陌生主机发请求。
 */

import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";

import { MarkdownText } from "../src/components/MarkdownText.js";

const render = (text: string): string =>
  renderToStaticMarkup(<MarkdownText text={text} className="agent-entry-body agent-markdown" />);

test("structure reaches the DOM: headings, lists, code blocks and tables", () => {
  const markup = render("# 结论\n\n- 甲\n- 乙\n\n```sh\ndocker ps\n```\n\n| a | b |\n| --- | --- |\n| 1 | 2 |");
  for (const fragment of ["<h1", "<ul", "<li", "<pre", "<code", "<table", "<th", "<td"]) {
    assert.equal(markup.includes(fragment), true, `缺少 ${fragment}`);
  }
  // 代码块的语言写在块上，读的人不用猜这段是什么。
  assert.equal(markup.includes("md-code-language"), true);
  assert.equal(markup.includes("docker ps"), true);
});

test("inline code, emphasis and strike map to real elements", () => {
  const markup = render("**粗** *斜* ~~删~~ `路径`");
  // 文字本体总是包在 KeywordText 的 span 里（关键词着色由它负责），所以这里只认外层元素。
  assert.match(markup, /<strong><span[^>]*>粗<\/span><\/strong>/);
  assert.match(markup, /<em><span[^>]*>斜<\/span><\/em>/);
  assert.match(markup, /<del><span[^>]*>删<\/span><\/del>/);
  assert.match(markup, /<code class="md-code-inline"><span[^>]*>路径<\/span><\/code>/);
});

test("HTML in the answer is escaped text, never markup", () => {
  const markup = render('<img src=x onerror="alert(1)">\n\n<script>alert(1)</script>');
  assert.equal(markup.includes("&lt;img"), true);
  assert.equal(markup.includes("&lt;script"), true);
  assert.equal(markup.includes("<img"), false);
  assert.equal(markup.includes("<script"), false);
});

test("links never become navigable anchors", () => {
  const markup = render("见 [文档](https://example.test/a) 与 https://example.test/b");
  assert.equal(markup.includes("md-link"), true);
  assert.equal(markup.includes("<a "), false);
  assert.equal(markup.includes("href"), false);
  // 地址仍要看得见：窗口里打不开，至少能复制出去。
  assert.equal(markup.includes('title="https://example.test/a"'), true);
});

test("an unknown scheme stays plain text", () => {
  const markup = render("[x](javascript:alert(1))");
  assert.equal(markup.includes("md-link"), false);
  assert.equal(markup.includes("javascript:alert(1)"), true);
});

test("images show their alt text and are never fetched", () => {
  const markup = render("![拓扑图](https://example.test/a.png)");
  assert.equal(markup.includes("<img"), false);
  assert.equal(markup.includes("拓扑图"), true);
  assert.equal(markup.includes('title="https://example.test/a.png"'), true);
});

test("task lists show a box instead of a bullet", () => {
  const markup = render("- [x] 完成\n- [ ] 未完成");
  assert.equal(markup.includes("☑"), true);
  assert.equal(markup.includes("☐"), true);
});

test("the body stays a single selectable block, and labels stay outside it", () => {
  const markup = render("第一段\n\n第二段");
  assert.equal(markup.startsWith('<div class="agent-entry-body agent-markdown">'), true);
  assert.equal(markup.endsWith("</div>"), true);
});

test("empty and whitespace-only bodies render an empty container instead of throwing", () => {
  assert.equal(render(""), '<div class="agent-entry-body agent-markdown"></div>');
  assert.equal(render("\n\n   \n"), '<div class="agent-entry-body agent-markdown"></div>');
});
