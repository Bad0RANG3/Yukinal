/**
 * Markdown 解析器的契约。
 *
 * 这些用例存在的理由不是「覆盖率高」，而是把三件不可能靠看界面发现的事钉住：
 *   1. 模型写回来的结构确实被认出来了（而不是整段按纯文本铺开）；
 *   2. **没认出来的东西一个字都没丢** —— 解析失败的代价必须是「不好看」，
 *      不能是「内容消失」；
 *   3. 流式输出里的半截文本（未闭合的代码围栏）不会抛，也不会把结果吞掉。
 */

import assert from "node:assert/strict";
import test from "node:test";

import { parseInline, parseMarkdown, type Block, type Inline } from "../src/lib/markdown.js";

type Kind = Block["kind"];
type Of<K extends Kind> = Extract<Block, { kind: K }>;

/** 取第一个指定类型的块；取不到就让断言在这里失败，而不是抛一个类型错误。 */
function block<K extends Kind>(blocks: Block[], kind: K): Of<K> {
  const found = blocks.find((candidate): candidate is Of<K> => candidate.kind === kind);
  assert.ok(found, `期望一个 ${kind} 块，实际只有 ${blocks.map((candidate) => candidate.kind).join(", ") || "空"}`);
  return found;
}

/** 行内节点拍平成一句话：断言用，不参与渲染。 */
function inlineText(nodes: Inline[]): string {
  return nodes
    .map((node) => {
      switch (node.kind) {
        case "text":
        case "code":
          return node.text;
        case "break":
          return "\n";
        case "strong":
        case "emphasis":
        case "strike":
        case "link":
          return inlineText(node.children);
        case "image":
          return node.alt;
      }
    })
    .join("");
}

/** 块拍平成一句话，用来问「这段东西还在不在」。 */
function flatten(blocks: Block[]): string {
  return blocks
    .map((candidate) => {
      switch (candidate.kind) {
        case "heading":
        case "paragraph":
          return inlineText(candidate.content);
        case "code":
          return candidate.text;
        case "list":
          return candidate.items.map((item) => flatten(item.blocks)).join("\n");
        case "quote":
          return flatten(candidate.blocks);
        case "table":
          return [candidate.head, ...candidate.rows].map((row) => row.map(inlineText).join(" ")).join("\n");
        case "rule":
          return "---";
      }
    })
    .join("\n");
}

test("headings: level, closing hashes, and no-space is not a heading", () => {
  const blocks = parseMarkdown("# 标题\n\n### 三级 ###\n\n#没空格\n\n####### 七个");
  assert.deepEqual(blocks[0], { kind: "heading", level: 1, content: [{ kind: "text", text: "标题" }] });
  assert.deepEqual(blocks[1], { kind: "heading", level: 3, content: [{ kind: "text", text: "三级" }] });
  // 两个反例：`#没空格` 与七个 `#` 都不是标题，它们是正文。
  assert.equal(inlineText(block(blocks, "paragraph").content), "#没空格");
  assert.equal(blocks.length, 4);
  assert.equal(blocks[3]?.kind, "paragraph");
});

test("paragraphs join soft breaks with a space and keep trailing spaces as hard breaks", () => {
  assert.deepEqual(parseMarkdown("第一行\n第二行"), [
    {
      kind: "paragraph",
      content: [
        { kind: "text", text: "第一行" },
        { kind: "text", text: " " },
        { kind: "text", text: "第二行" },
      ],
    },
  ]);

  assert.deepEqual(block(parseMarkdown("第一行  \n第二行"), "paragraph").content, [
    { kind: "text", text: "第一行" },
    { kind: "break" },
    { kind: "text", text: "第二行" },
  ]);

  const backslash = block(parseMarkdown("第一行\\\n第二行"), "paragraph");
  assert.deepEqual(backslash.content, [
    { kind: "text", text: "第一行" },
    { kind: "break" },
    { kind: "text", text: "第二行" },
  ]);
});

test("fenced code keeps its language, its text, and survives an unterminated fence", () => {
  const closed = parseMarkdown("前言\n\n```sh\nls -la\n```\n\n后记");
  assert.deepEqual(closed[1], { kind: "code", language: "sh", text: "ls -la" });
  assert.equal(flatten(closed.slice(2)), "后记");

  assert.deepEqual(parseMarkdown("~~~json\n{}\n~~~"), [{ kind: "code", language: "json", text: "{}" }]);

  // 流式输出里围栏一定闭不上：到这里为止的都得算代码，一个字都不能丢。
  assert.deepEqual(parseMarkdown("```sh\nls -la\nrm -rf /tmp/x"), [
    { kind: "code", language: "sh", text: "ls -la\nrm -rf /tmp/x" },
  ]);
});

test("inline code spans use the matching run of backticks", () => {
  assert.deepEqual(parseInline("`a`"), [{ kind: "code", text: "a" }]);
  assert.deepEqual(parseInline("``b ` c``"), [{ kind: "code", text: "b ` c" }]);
  // 没闭合的反引号是普通文字，不是「吞掉剩下全部的代码段」。
  assert.equal(inlineText(parseInline("`未闭合")), "`未闭合");
});

test("emphasis: strong, emphasis, strike, and the triple marker", () => {
  assert.deepEqual(parseInline("**粗**"), [{ kind: "strong", children: [{ kind: "text", text: "粗" }] }]);
  assert.deepEqual(parseInline("*斜*"), [{ kind: "emphasis", children: [{ kind: "text", text: "斜" }] }]);
  assert.deepEqual(parseInline("~~删~~"), [{ kind: "strike", children: [{ kind: "text", text: "删" }] }]);
  assert.deepEqual(parseInline("***都有***"), [
    { kind: "strong", children: [{ kind: "emphasis", children: [{ kind: "text", text: "都有" }] }] },
  ]);
});

test("underscores inside words are not emphasis, and a list star is not either", () => {
  // `snake_case_name` 变成「snake<em>case</em>name」是这类解析器最常见的丑事。
  assert.deepEqual(parseInline("snake_case_name"), [{ kind: "text", text: "snake_case_name" }]);
  assert.deepEqual(parseMarkdown("* 项"), [
    {
      kind: "list",
      ordered: false,
      start: 1,
      items: [{ checked: null, blocks: [{ kind: "paragraph", content: [{ kind: "text", text: "项" }] }] }],
    },
  ]);
});

test("backslash escapes turn markers back into literal characters", () => {
  assert.equal(inlineText(parseInline("\\*不是斜体\\*")), "*不是斜体*");
  assert.equal(inlineText(parseInline("下划线 \\_x\\_")), "下划线 _x_");
});

test("lists: ordered start, nesting, and task checkboxes", () => {
  assert.deepEqual(parseMarkdown("- 甲\n- 乙"), [
    {
      kind: "list",
      ordered: false,
      start: 1,
      items: [
        { checked: null, blocks: [{ kind: "paragraph", content: [{ kind: "text", text: "甲" }] }] },
        { checked: null, blocks: [{ kind: "paragraph", content: [{ kind: "text", text: "乙" }] }] },
      ],
    },
  ]);

  const ordered = block(parseMarkdown("3. 甲\n4. 乙"), "list");
  assert.equal(ordered.ordered, true);
  assert.equal(ordered.start, 3);

  const nested = block(parseMarkdown("- 甲\n  - 甲一\n- 乙"), "list");
  assert.equal(nested.items.length, 2);
  assert.equal(nested.items[0]?.blocks.length, 2);
  assert.equal(nested.items[0]?.blocks[1]?.kind, "list");
  assert.equal(flatten(nested.items[0]?.blocks ?? []), "甲\n甲一");

  const tasks = block(parseMarkdown("- [x] 完成\n- [ ] 未完成"), "list");
  assert.deepEqual(tasks.items.map((item) => item.checked), [true, false]);
  assert.equal(tasks.items.map((item) => flatten(item.blocks)).join("\n"), "完成\n未完成");
});

test("a blank line between items does not split the list", () => {
  const blocks = parseMarkdown("- 甲\n\n- 乙\n\n正文");
  assert.equal(blocks.length, 2);
  assert.equal(block(blocks, "list").items.length, 2);
  assert.equal(flatten(blocks.slice(1)), "正文");
});

test("blockquotes keep their own structure", () => {
  const blocks = parseMarkdown("> 引用第一行\n> 引用第二行\n\n后记");
  assert.equal(blocks.length, 2);
  assert.equal(flatten([block(blocks, "quote")]), "引用第一行 引用第二行");
  assert.equal(flatten(blocks.slice(1)), "后记");
});

test("tables: alignment row, header and body cells", () => {
  assert.deepEqual(parseMarkdown("| 名称 | 状态 |\n| :--- | ---: |\n| ssh | ok |"), [
    {
      kind: "table",
      align: ["left", "right"],
      head: [[{ kind: "text", text: "名称" }], [{ kind: "text", text: "状态" }]],
      rows: [[[{ kind: "text", text: "ssh" }], [{ kind: "text", text: "ok" }]]],
    },
  ]);
  // 列数对不上就不是表格：它只是正文里两行带竖线的字。
  assert.equal(parseMarkdown("| a | b |\n| --- |")[0]?.kind, "paragraph");
});

test("rules are recognized, including the spaced and star forms", () => {
  assert.deepEqual(parseMarkdown("---"), [{ kind: "rule" }]);
  assert.deepEqual(parseMarkdown("***"), [{ kind: "rule" }]);
  assert.deepEqual(parseMarkdown("- - -"), [{ kind: "rule" }]);
});

test("links and images: only known schemes become nodes", () => {
  assert.deepEqual(parseInline("[文档](https://example.test/a)"), [
    { kind: "link", href: "https://example.test/a", children: [{ kind: "text", text: "文档" }] },
  ]);
  assert.deepEqual(parseInline("<https://example.test/a>"), [
    { kind: "link", href: "https://example.test/a", children: [{ kind: "text", text: "https://example.test/a" }] },
  ]);
  assert.deepEqual(parseInline("![替代文字](https://example.test/a.png)"), [
    { kind: "image", href: "https://example.test/a.png", alt: "替代文字" },
  ]);

  // 句末标点属于句子，不属于地址。
  assert.deepEqual(parseInline("见 https://example.test/a。"), [
    { kind: "text", text: "见 " },
    { kind: "link", href: "https://example.test/a", children: [{ kind: "text", text: "https://example.test/a" }] },
    { kind: "text", text: "。" },
  ]);

  // 不认识的 scheme 一律当普通文本：不生成节点，也不假装它是链接。
  assert.equal(parseInline("[x](javascript:alert(1))").every((node) => node.kind === "text"), true);
  assert.equal(parseInline("![x](data:text/html,<b>)").every((node) => node.kind === "text"), true);
  assert.equal(parseInline("xhttps://example.test").every((node) => node.kind === "text"), true);
});

test("HTML is text, never markup", () => {
  assert.deepEqual(parseMarkdown("<div>hi</div>"), [
    { kind: "paragraph", content: [{ kind: "text", text: "<div>hi</div>" }] },
  ]);
  assert.deepEqual(parseMarkdown("<script>alert(1)</script>"), [
    { kind: "paragraph", content: [{ kind: "text", text: "<script>alert(1)</script>" }] },
  ]);
});

test("deep indentation stops at the depth limit instead of overflowing the stack", () => {
  const deep = Array.from({ length: 12 }, (_, level) => `${"  ".repeat(level)}- 第${level}层`).join("\n");
  const text = flatten(parseMarkdown(deep));
  for (let level = 0; level < 12; level += 1) {
    assert.equal(text.includes(`第${level}层`), true, `第${level}层 被丢掉了`);
  }
});

test("every prefix of a streaming answer parses", () => {
  const answer =
    "# 结论\n\n- 第一点，见 `config.yaml`\n- 第二点  \n  续行\n\n```sh\ndocker ps\n```\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n";
  for (let length = 0; length <= answer.length; length += 1) {
    const blocks = parseMarkdown(answer.slice(0, length));
    assert.equal(Array.isArray(blocks), true);
  }
  // 完整文本认得出来：两个列表项、一个代码块、一张表。
  const full = parseMarkdown(answer);
  assert.deepEqual(full.map((candidate) => candidate.kind), ["heading", "list", "code", "table"]);
});

test("empty input produces no blocks", () => {
  assert.deepEqual(parseMarkdown(""), []);
  assert.deepEqual(parseMarkdown("\n\n  \n"), []);
});
