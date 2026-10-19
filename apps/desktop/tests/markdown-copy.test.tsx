import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";

import { CopyButton } from "../src/components/CopyButton.js";
import { MarkdownText } from "../src/components/MarkdownText.js";
import { parseMarkdown, type Block } from "../src/lib/markdown.js";

function codeBlocks(blocks: Block[]): Extract<Block, { kind: "code" }>[] {
  return blocks.flatMap((block) => {
    if (block.kind === "code") return [block];
    if (block.kind === "quote") return codeBlocks(block.blocks);
    if (block.kind === "list") return block.items.flatMap((item) => codeBlocks(item.blocks));
    return [];
  });
}
const copyBlocks = (source: string) => codeBlocks(parseMarkdown(source, { preserveCodeSource: true }));

test("closed fences preserve code tabs, CRLF, trailing spaces and every ending before the closing fence", () => {
  const body = "\tif (ready) {  \r\n\t\tprint('中文');\r\n}\r\n\r\n";
  const source = "before\r\n\r\n```py\r\n" + body + "```\r\nafter";
  const [block] = copyBlocks(source);
  assert.ok(block);
  assert.equal(block.copyText, body);
  assert.equal(block.text, body.replace(/\r\n/g, "\n").replace(/\t/g, "    ").slice(0, -1), "normalized display stays compatible");
  assert.equal(block.language, "py");
  assert.equal(copyBlocks("~~~\rvalue\r~~~")[0]?.copyText, "value\r");
});

test("unterminated streaming fences copy exactly what has arrived without inventing a newline", () => {
  for (const body of ["", "\twork", "\twork\n", "  work  \r\n\r\n", "\n\n"]) {
    assert.equal(copyBlocks("```sh\n" + body)[0]?.copyText, body);
  }
  assert.equal(copyBlocks("```\n```")[0]?.copyText, "");
  assert.equal(copyBlocks("````txt\n```\nraw\n````")[0]?.copyText, "```\nraw\n");
});

test("indented code removes only its four structural columns and retains internal tabs/newlines", () => {
  const source = "    \twork();  \r\n\r\n\t\tlast();\r\n";
  const [block] = copyBlocks(source);
  assert.ok(block);
  assert.equal(block.copyText, "\twork();  \r\n\r\n\tlast();\r\n");
  assert.equal(block.text, "    work();  \n\n    last();");
  assert.equal(copyBlocks("    no final newline")[0]?.copyText, "no final newline");
});

test("quoted fences and quoted indented code copy code content rather than quote markers", () => {
  assert.equal(copyBlocks("> ```py\r\n> \tprint(1)  \r\n> \r\n> ```")[0]?.copyText, "\tprint(1)  \r\n\r\n");
  assert.equal(copyBlocks("> > ```\n> > \tinner\n> > ```")[0]?.copyText, "\tinner\n");
  assert.equal(copyBlocks(">     \tinner\n>     tail\n")[0]?.copyText, "\tinner\ntail\n");
});

test("nested list/quote fences remove container indentation but keep code indentation and blank lines", () => {
  assert.equal(copyBlocks("> - ```py\r\n>   \tprint(1)  \r\n>   \r\n>   ```")[0]?.copyText, "\tprint(1)  \r\n\r\n");
  assert.equal(copyBlocks("- item\n  - ```\n    \tinner\n    ```")[0]?.copyText, "\tinner\n");
  assert.equal(copyBlocks("- ```\n  first\n\n\n  last\n  ```")[0]?.copyText, "first\n\n\nlast\n");
});

test("streaming list fences retain their code indentation before a closing fence has arrived", () => {
  for (const ending of ["", "\n", "\r\n"]) {
    assert.equal(copyBlocks("- ```\n  \tcode  " + ending)[0]?.copyText, "\tcode  " + ending);
    assert.equal(copyBlocks("> - ```\n>   \tcode  " + ending)[0]?.copyText, "\tcode  " + ending);
    assert.equal(copyBlocks("- item\n  - ```\n    \tcode" + ending)[0]?.copyText, "\tcode" + ending);
  }
});

test("preserving copy source is opt-in and never changes the established AST/display contract", () => {
  for (const source of [
    "# Title\n\n```sh\r\n\tcmd\r\n```\n\ntext",
    "> - ```py\n>   \tcode\n>   ```\n\nlast",
    "- one\n\n  ```\n  code\n  ```\n\n- two",
    "[ref]: https://example.test\n\n```\n\tcode\n```\n\nfoot[^a]\n\n[^a]: note",
    "    first\n\n    next\n\nparagraph",
  ]) {
    const preserved = parseMarkdown(source, { preserveCodeSource: true });
    const displayOnly: unknown = JSON.parse(JSON.stringify(preserved, (key, value: unknown) => key === "copyText" ? undefined : value));
    assert.deepEqual(displayOnly, parseMarkdown(source));
  }
  assert.deepEqual(parseMarkdown("```\n\tcode\n```"), [{ kind: "code", language: null, text: "    code" }]);
});

test("rendered code has a named header, copy control and escaped code without navigation or raw HTML", () => {
  const html = renderToStaticMarkup(<MarkdownText text={'```html\n<script>unsafe()</script>\n```'} />);
  assert.match(html, /class="md-code-header"/);
  assert.match(html, /class="md-code-language">html<\/span>/);
  assert.match(html, /class="copy-control md-code-copy" data-copy-state="idle"/);
  assert.match(html, /class="copy-button" aria-label="复制代码"/);
  assert.match(html, /class="copy-feedback" role="status" aria-live="polite"/);
  assert.match(html, /class="md-code-pre"/);
  assert.doesNotMatch(html, /<script>/);
  assert.match(html, /&lt;script&gt;/);
  const plain = renderToStaticMarkup(<MarkdownText text={"```\nraw\n```"} />);
  assert.match(plain, /class="md-code-language">代码<\/span>/);
});

test("reusable reply copy exposes explicit labels and disables only empty or explicitly disabled controls", () => {
  const reply = renderToStaticMarkup(<CopyButton text={"answer\n"} label="复制回答" ariaLabel="复制完整回答" className="agent-entry-copy" />);
  assert.match(reply, /class="copy-control agent-entry-copy"/);
  assert.match(reply, /aria-label="复制完整回答"/);
  assert.doesNotMatch(reply, /disabled=/);
  assert.match(renderToStaticMarkup(<CopyButton text="" />), /disabled=""/);
  assert.match(renderToStaticMarkup(<CopyButton text="code" disabled />), /disabled=""/);
  assert.doesNotMatch(renderToStaticMarkup(<CopyButton text={"\t \n"} />), /disabled=/);
});
