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
        case "footnote":
          return `[^${node.label}]`;
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
        case "footnotes":
          return candidate.items.map((item) => inlineText(item.content)).join("\n");
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

test("setext headings are recognized without swallowing a standalone rule", () => {
  assert.deepEqual(parseMarkdown("一级\n===\n\n二级\n---"), [
    { kind: "heading", level: 1, content: [{ kind: "text", text: "一级" }] },
    { kind: "heading", level: 2, content: [{ kind: "text", text: "二级" }] },
  ]);
  assert.deepEqual(parseMarkdown("---"), [{ kind: "rule" }]);
});

test("four-space indented code is preserved as code", () => {
  assert.deepEqual(parseMarkdown("    const x = 1;\n\n    console.log(x);"), [
    { kind: "code", language: null, text: "const x = 1;\n\nconsole.log(x);" },
  ]);
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
  // `***x***` 是「斜里套粗」而不是「粗里套斜」：规范第 14 条明说 `<em><strong>` 优先。
  assert.deepEqual(parseInline("***都有***"), [
    { kind: "emphasis", children: [{ kind: "strong", children: [{ kind: "text", text: "都有" }] }] },
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
      tight: true,
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
      tight: true,
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

test("lists use CommonMark tightness from item gaps and direct block gaps", () => {
  assert.equal(block(parseMarkdown("- 甲\n- 乙"), "list").tight, true);

  const betweenItems = block(parseMarkdown("- 甲\n\n- 乙"), "list");
  assert.equal(betweenItems.tight, false);
  assert.equal(betweenItems.items.length, 2);

  const insideItem = block(parseMarkdown("- 甲\n\n  续段\n- 乙"), "list");
  assert.equal(insideItem.tight, false);
  assert.deepEqual(insideItem.items[0]?.blocks.map((candidate) => candidate.kind), ["paragraph", "paragraph"]);

  const nestedSeparator = block(parseMarkdown("- 甲\n  - 甲一\n\n  - 甲二\n- 乙"), "list");
  assert.equal(nestedSeparator.tight, true);
  const nested = nestedSeparator.items[0]?.blocks.find((candidate) => candidate.kind === "list");
  assert.equal(nested?.kind === "list" ? nested.tight : null, false);

  const changedMarker = parseMarkdown("- 甲\n+ 乙");
  assert.deepEqual(changedMarker.map((candidate) => candidate.kind), ["list", "list"]);
});

test("a blank line between items does not split the list", () => {
  const blocks = parseMarkdown("- 甲\n\n- 乙\n\n正文");
  assert.equal(blocks.length, 2);
  const list = block(blocks, "list");
  assert.equal(list.items.length, 2);
  assert.equal(list.tight, false);
  assert.equal(flatten(blocks.slice(1)), "正文");
});

test("blockquotes keep their own structure", () => {
  const blocks = parseMarkdown("> 引用第一行\n> 引用第二行\n\n后记");
  assert.equal(blocks.length, 2);
  assert.equal(flatten([block(blocks, "quote")]), "引用第一行 引用第二行");
  assert.equal(flatten(blocks.slice(1)), "后记");
});

test("blockquotes accept paragraph lazy continuation but stop before new blocks", () => {
  const lazy = parseMarkdown("> first line\ncontinued lazily\n\noutside");
  assert.equal(lazy.length, 2);
  assert.equal(flatten([block(lazy, "quote")]), "first line continued lazily");
  assert.equal(flatten(lazy.slice(1)), "outside");

  for (const source of ["> # title\noutside", "> - item\noutside"]) {
    const blocks = parseMarkdown(source);
    assert.equal(blocks.length, 2, source);
    assert.equal(blocks[0]?.kind, "quote", source);
    assert.equal(blocks[1]?.kind, "paragraph", source);
    assert.equal(flatten(blocks.slice(1)), "outside");
  }
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

test("reference links resolve case-insensitively, including collapsed and shortcut forms", () => {
  const blocks = parseMarkdown(
    "[docs]: https://example.test/guide\n\n见 [文档][DOCS]、[docs][] 和 [Docs]。",
  );
  const paragraph = block(blocks, "paragraph");
  assert.equal(paragraph.content.filter((node) => node.kind === "link").length, 3);
  assert.deepEqual(
    paragraph.content
      .filter((node): node is Extract<Inline, { kind: "link" }> => node.kind === "link")
      .map((node) => node.href),
    [
      "https://example.test/guide",
      "https://example.test/guide",
      "https://example.test/guide",
    ],
  );

  // A definition using a forbidden scheme is not a link definition. The source stays visible.
  const unsafe = parseMarkdown("[x]: javascript:alert(1)\n\n[x]");
  assert.equal(unsafe.every((candidate) => candidate.kind === "paragraph"), true);
  assert.equal(flatten(unsafe).includes("javascript:alert(1)"), true);
});

test("HTML is text, never markup", () => {
  assert.deepEqual(parseMarkdown("<div>hi</div>"), [
    { kind: "paragraph", content: [{ kind: "text", text: "<div>hi</div>" }] },
  ]);
  assert.deepEqual(parseMarkdown("<script>alert(1)</script>"), [
    { kind: "paragraph", content: [{ kind: "text", text: "<script>alert(1)</script>" }] },
  ]);
});

test("footnotes keep both the reference and the definition without generating HTML", () => {
  assert.deepEqual(parseMarkdown("结论[^1]\n\n[^1]: [来源](https://example.test)"), [
    {
      kind: "paragraph",
      content: [{ kind: "text", text: "结论" }, { kind: "footnote", label: "1" }],
    },
    {
      kind: "footnotes",
      items: [
        {
          label: "1",
          content: [
            {
              kind: "link",
              href: "https://example.test",
              children: [{ kind: "text", text: "来源" }],
            },
          ],
        },
      ],
    },
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
    "# 结论\n\n- 第一点，见 `config.yaml`\n- 第二点  \n  续行\n\n```sh\ndocker ps\n```\n\n> 引用\n> 续行\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n";
  for (let length = 0; length <= answer.length; length += 1) {
    const blocks = parseMarkdown(answer.slice(0, length));
    assert.equal(Array.isArray(blocks), true);
  }
  // 完整文本认得出来：两个列表项、一个代码块、一个引用、一张表。
  const full = parseMarkdown(answer);
  assert.deepEqual(full.map((candidate) => candidate.kind), ["heading", "list", "code", "quote", "table"]);

  // 流式最怕的是「半截语法把后面的字吃掉」：每个前缀里的文字字符都必须还在，
  // 多出没配上的标记字符是允许的（那正是「不认识就按字面显示」的样子）。
  const inline = "前置 *强调* 与 **加粗** 和 `代码` 结尾";
  for (let length = 0; length <= inline.length; length += 1) {
    const prefix = inline.slice(0, length);
    const kept = flatten(parseMarkdown(prefix));
    let cursor = 0;
    for (const char of prefix) {
      if (!/[\p{L}\p{N}]/u.test(char)) continue;
      const found = kept.indexOf(char, cursor);
      assert.notEqual(found, -1, `前缀 ${JSON.stringify(prefix)} 丢了 ${char}（输出 ${JSON.stringify(kept)}）`);
      cursor = found + 1;
    }
  }
});

test("empty input produces no blocks", () => {
  assert.deepEqual(parseMarkdown(""), []);
  assert.deepEqual(parseMarkdown("\n\n  \n"), []);
});

/* ── 与 CommonMark 0.31.2 对齐的几处修正（corpus 是 opt-in，这几条离线钉住它们） ── */

test("a code span closes only on a run of exactly the same length", () => {
  // 规范例子：一个反引号开头、中间是两个反引号、一个反引号收尾 → 内容就是那两个反引号。
  // 用 `indexOf` 找结束标记会把更长的 run 当成命中，于是这里会变成两个空代码段。
  const nodes = parseInline("` `` `");
  assert.deepEqual(nodes, [{ kind: "code", text: "``" }]);
  assert.deepEqual(parseInline("`` ` ``"), [{ kind: "code", text: "`" }]);
});

test("an ordered list interrupts a paragraph only when it starts at one", () => {
  // `14.` 是句子的一部分：这里必须是一段，而不是「段落 + 从 14 开始的列表」（那样 `14.` 会消失）。
  const blocks = parseMarkdown("The number of windows in my house is\n14.  The number of doors is 6.");
  assert.deepEqual(blocks.map((candidate) => candidate.kind), ["paragraph"]);
  // 软换行渲染成一个空格，所以原样留下时是两个空格接 `The number…`（源文本里就是两个）。
  assert.match(inlineText(block(blocks, "paragraph").content), /14\. {2}The number of doors is 6\./);

  // 从 1 开始就是列表。
  assert.deepEqual(
    parseMarkdown("text\n1. item").map((candidate) => candidate.kind),
    ["paragraph", "list"],
  );
  // 空列表项也打断不了段落。这里用 `*` 而不是 `-`：单独一行 `-` 是 setext 标题下划线，
  // 那两条规则在规范里是分开的，用 `-` 测这件事只会测到另一条。
  assert.deepEqual(parseMarkdown("text\n* ").map((candidate) => candidate.kind), ["paragraph"]);
});

test("emphasis needs left/right-flanking delimiters, and symbols count as punctuation", () => {
  // 规范例子：`$`/`£`/`€` 属于 Unicode 符号（S），按规范也算标点，所以这些都不是斜体。
  for (const source of ['a*"foo"*', "*$*alpha.", "*£*bravo.", "*€*charlie."]) {
    const nodes = parseInline(source);
    assert.deepEqual(
      nodes.filter((node) => node.kind === "emphasis"),
      [],
      `${source} 不该被当成斜体`,
    );
    assert.equal(inlineText(nodes), source, "整段必须原样留下");
  }
  // 正常的强调仍然认得出来。
  assert.deepEqual(parseInline("*foo*"), [
    { kind: "emphasis", children: [{ kind: "text", text: "foo" }] },
  ]);
});

test("an emphasis closer cannot come from inside a link label", () => {
  // 规范例子：`*[foo*](/uri)` 里的第一个 `*` 是普通文本，链接必须完整保留。
  // 这里用 https 目标，因为相对目标在本地策略下本来就成不了链接（那条偏差另有测试）。
  const nodes = parseInline("*[foo*](https://example.test/)");
  assert.deepEqual(
    nodes.filter((node) => node.kind === "emphasis"),
    [],
    "标签里的 `*` 不能当作结束标记",
  );
  assert.equal(inlineText(nodes), "*foo*");
  assert.equal(
    nodes.filter((node) => node.kind === "link").length,
   1,
    "链接不能被斜体吞掉",
  );
});

test("emphasis pairs runs the way the spec does", () => {
  // 扫描时先看清整个 delimiter run 贴不贴边；只看单个字符的话，`**"foo"` 会被当成
  // 「一个能开的 `*`」，规范里它一个都开不了（前面是字母、后面是标点）。
  for (const source of ['a**"foo"**', 'a__"foo"__', "**foo bar **", "__foo bar __"]) {
    assert.deepEqual(parseInline(source), [{ kind: "text", text: source }], `${source} 必须原样留下`);
  }

  // 词中间的 `_` 不配对，但两侧的标记是普通文字，不能消失。
  assert.deepEqual(parseInline("foo__bar__"), [{ kind: "text", text: "foo__bar__" }]);
  assert.deepEqual(parseInline("__foo__bar__baz__"), [
    { kind: "strong", children: [{ kind: "text", text: "foo__bar__baz" }] },
  ]);
  // 「词字符」是「不是空白也不是标点」，不是 ASCII —— 西里尔字母同样算词。
  for (const source of ["пристаням_стремятся_", "_пристаням_стремятся", "пристаням__стремятся__"]) {
    assert.deepEqual(parseInline(source), [{ kind: "text", text: source }], `${source} 必须原样留下`);
  }

  // 「3 的倍数」规则：`*foo**bar*` 里的 `**` 两边都贴边，配不上，就是两个字面星号。
  assert.deepEqual(parseInline("*foo**bar*"), [
    { kind: "emphasis", children: [{ kind: "text", text: "foo**bar" }] },
  ]);
});

test("numeric character references decode, and nothing else does", () => {
  // 十进制、十六进制、以及控制码位换成替换字符。
  assert.equal(inlineText(parseInline("&#35; &#1234; &#x22; &#X22;")), "# Ӓ \" \"");
  assert.equal(inlineText(parseInline("&#0;")), "\uFFFD");

  // 位数不对、缺分号、名字不是数字：都不是引用，原样留下。
  for (const source of ["&#87654321;", "&#abcdef0;", "&#;", "&#x;", "&copy;", "&amp"]) {
    assert.equal(inlineText(parseInline(source)), source, `${source} 必须原样留下`);
  }

  // `&#42;` 解出来的是**文字**，不能顶替强调标记（规范里专门有一条）。
  assert.deepEqual(parseInline("&#42;foo&#42;"), [{ kind: "text", text: "*foo*" }]);
});

test("inline syntax spans a soft line break, and the last line keeps its backslash", () => {
  // 段落是整段解析的：代码段跨行之后，行尾的反斜杠是**代码段里的字符**，不是硬换行。
  // （规范例子 `` `code\ `` 换行 `` span` `` → `<code>code\ span</code>`。）
  assert.deepEqual(parseMarkdown("`code\\\nspan`"), [
    { kind: "paragraph", content: [{ kind: "code", text: "code\\ span" }] },
  ]);
  // 强调同样可以跨行。
  assert.deepEqual(block(parseMarkdown("*foo\nbar*"), "paragraph").content, [
    { kind: "emphasis", children: [{ kind: "text", text: "foo" }, { kind: "text", text: " " }, { kind: "text", text: "bar" }] },
  ]);
  // 段落最后一行结尾的反斜杠是普通文字（规范：`foo\` 单独一段显示 `foo\`）。
  assert.equal(flatten(parseMarkdown("foo\\")), "foo\\");
});
