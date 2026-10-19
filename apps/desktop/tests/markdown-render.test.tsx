/**
 * 渲染层的硬规则，钉死在静态标记上。
 *
 * 这三条都不是「好看不好看」的问题，而是安全与可用性的边界：
 *   - 正文里的 HTML 必须是**文字**（没有 `dangerouslySetInnerHTML`，也没有过滤规则
 *     需要维护对错）；
 *   - 链接不能生成可导航的 `<a href>`；点击必须经过受限 opener 交给系统浏览器；
 *   - 远程图片只有在用户明确点击后才创建 `<img>`，并使用 lazy/no-referrer。
 */

import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";

import {
  approvedRemoteImageSource,
  headingSlug,
  MarkdownText,
  markdownLinkTarget,
  openMarkdownExternalLink,
} from "../src/components/MarkdownText.js";

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

test("links use the external opener path, never an in-WebView anchor", () => {
  const markup = render("见 [文档](https://example.test/a) 与 https://example.test/b");
  assert.equal(markup.includes("md-link"), true);
  assert.equal(markup.includes("<a "), false);
  assert.equal(markup.includes("href"), false);
  assert.equal(markup.includes("<button"), true);
  assert.equal(markup.includes('title="https://example.test/a"'), true);
});

test("headings carry deduplicated ids and #links jump in-page instead of opening externally", () => {
  const markup = render("# 结论\n\n## 结论\n\n回到 [结论](#结论)");
  assert.match(markup, /<h1 class="md-heading" id="结论">/u);
  assert.match(markup, /<h2 class="md-heading" id="结论-2">/u, "a repeat heading needs a distinct id");
  assert.ok(markup.includes('data-anchor="结论"'), "the anchor link must be marked as an in-page jump");
  assert.ok(markup.includes('title="#结论"'));
  // The shape stays the same as any other link: a button, never a navigable anchor.
  assert.equal(markup.includes("<a "), false);
  assert.equal(markup.includes("href"), false);
  assert.equal(markdownLinkTarget("#结论").kind, "anchor");
  assert.deepEqual(markdownLinkTarget("https://example.test/a"), { kind: "external", url: "https://example.test/a" });
  assert.equal(headingSlug("结论", new Set(["结论"])), "结论-2");
});

test("a failed external open becomes a visible notice, not an unhandled rejection", async () => {
  const notice = await openMarkdownExternalLink("https://example.test", async () => {
    throw new Error("opener unavailable");
  });
  assert.equal(notice, "无法打开链接：opener unavailable");
  assert.equal(await openMarkdownExternalLink("https://example.test", async () => undefined), null);
});

test("an unknown scheme stays plain text", () => {
  const markup = render("[x](javascript:alert(1))");
  assert.equal(markup.includes("md-link"), false);
  assert.equal(markup.includes("javascript:alert(1)"), true);
});

test("remote images render as inert text until the user explicitly loads them", () => {
  const markup = render("![拓扑图](https://example.test/a.png)");
  assert.equal(markup.includes("<img"), false, "rendering Markdown must not request the image");
  assert.equal(markup.includes(" src="), false, "no resource-bearing src may exist before consent");
  assert.equal(markup.includes("alt="), false, "the inert placeholder is not an image");
  assert.equal(markup.includes("md-image-prompt"), true);
  assert.equal(markup.includes("加载远程图片"), true);
  assert.equal(markup.includes("拓扑图"), true);
});

test("image consent is bound to the exact URL that was approved", () => {
  const approved = "https://example.test/a.png";
  assert.equal(approvedRemoteImageSource(approved, approved), approved);
  assert.equal(approvedRemoteImageSource(approved, "https://attacker.test/b.png"), null);
  assert.equal(approvedRemoteImageSource(null, approved), null);
});

test("task lists show a box instead of a bullet", () => {
  const markup = render("- [x] 完成\n- [ ] 未完成");
  assert.equal(markup.includes("☑"), true);
  assert.equal(markup.includes("☐"), true);
});

test("tight lists omit direct paragraph wrappers and loose lists keep them", () => {
  const tight = render("- 甲\n- 乙");
  assert.equal(tight.includes("<p class=\"md-paragraph\">"), false);

  const loose = render("- 甲\n\n- 乙");
  assert.equal((loose.match(/<p class="md-paragraph">/g) ?? []).length, 2);
});

test("footnotes render as structured text, never as HTML", () => {
  const markup = render("结论[^1]\n\n[^1]: 来源说明");
  assert.equal(markup.includes("md-footnote-ref"), true);
  assert.equal(markup.includes("md-footnotes"), true);
  assert.equal(markup.includes("[1]"), true);
  assert.equal(markup.includes("来源说明"), true);
});

test("the body stays a single selectable block, and labels stay outside it", () => {
  const markup = render("第一段\n\n第二段");
  assert.equal(markup.startsWith('<div class="agent-entry-body agent-markdown" data-markdown-root="">'), true);
  assert.equal(markup.endsWith("</div>"), true);
});

test("empty and whitespace-only bodies render an empty container instead of throwing", () => {
  // `data-markdown-root` scopes in-page anchor lookups to the message that owns the link.
  assert.equal(render(""), '<div class="agent-entry-body agent-markdown" data-markdown-root=""></div>');
  assert.equal(render("\n\n   \n"), '<div class="agent-entry-body agent-markdown" data-markdown-root=""></div>');
});
