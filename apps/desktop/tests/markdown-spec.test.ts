/**
 * The CommonMark 0.31.2 corpus, opt-in.
 *
 * ## Why it is opt-in and why the corpus is not in the repo
 *
 * The spec corpus is CC-BY-SA text, and this repository's gate is offline: vendoring 140 KiB of
 * someone else's prose would add a licence obligation to every checkout and still be a copy that
 * can drift from the published one. So the corpus is downloaded on purpose and pointed at with
 * `YUKINAL_MARKDOWN_SPEC`:
 *
 *     curl -o "$TEMP/commonmark-0.31.2.json" https://spec.commonmark.org/0.31.2/spec.json
 *     $env:YUKINAL_MARKDOWN_SPEC = "$env:TEMP/commonmark-0.31.2.json"
 *     pnpm --filter @yukinal/desktop test
 *
 * Without that variable every test here is skipped, which is the same contract the live provider
 * and MCP tests use.
 *
 * ## What it measures, and what it does not
 *
 * Our parser produces an AST, not HTML, so the comparison is **content preservation**: every
 * character the spec wants on the page must still be on the page, in the same order, with
 * whitespace collapsed on both sides (the spec's `<br />` and our paragraph breaks make exact
 * whitespace a different question). That is the property this repository actually promises —
 * "没认出来的东西一个字都不能丢" — and it is what the gap list in
 * `docs/boundaries/markdown.md` is built from. It is deliberately *not* an HTML-equality test:
 * matching cmark's serialisation byte for byte would measure our serializer as much as our
 * parser.
 */

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { parseMarkdown, type Block, type Inline } from "../src/lib/markdown.js";

interface SpecExample {
  markdown: string;
  html: string;
  example: number;
  section: string;
}

/**
 * The sections `docs/boundaries/markdown.md` claims to handle, marked `(supported)` in the
 * report so the documented surface and the measurement are readable side by side.
 */
const SUPPORTED_SECTIONS = new Set([
  "ATX headings",
  "Backslash escapes",
  "Block quotes",
  "Code spans",
  "Emphasis and strong emphasis",
  "Entities and numeric character references",
  "Fenced code blocks",
  "Indented code blocks",
  "Link reference definitions",
  "Links",
  "Lists",
  "Paragraphs and blank lines",
  "Precedence",
  "Thematic breaks",
]);

/**
 * Sections the corpus cannot measure, because our rendering of them is deliberately different.
 *
 * HTML is shown as text and images are never downloaded, so the spec's HTML for these sections
 * *is* the deviation: counting them would only measure the decision, not the parser. They are
 * excluded from the floors and printed as excluded, so the omission is visible in every run.
 */
const DEVIATION_SECTIONS = new Set(["HTML blocks", "Raw HTML"]);

/**
 * Examples that require a link target our policy refuses.
 *
 * Links open through the system browser and only `http(s)`/`mailto`/fragment targets are turned
 * into links at all (relative targets like `/uri` cannot mean anything outside the document that
 * wrote them, and every other scheme is a navigation vector). The spec uses `/uri` throughout, so
 * those examples are outside the supported range by construction — not a parser gap.
 */
function requiresUnsupportedTarget(html: string): boolean {
  return [...html.matchAll(/(?:href|src)="([^"]*)"/g)].some((match) => {
    const target = match[1] ?? "";
    return !/^(?:https?:\/\/|mailto:|#)/i.test(target);
  });
}

function specPath(): string | null {
  const value = process.env.YUKINAL_MARKDOWN_SPEC?.trim();
  return value ? value : null;
}

function loadCorpus(path: string): SpecExample[] {
  const parsed: unknown = JSON.parse(readFileSync(path, "utf8"));
  assert.ok(Array.isArray(parsed), "the corpus must be an array of examples");
  return parsed as SpecExample[];
}

/** Every piece of text our AST would put on the page, in document order. */
function textOf(blocks: readonly Block[]): string {
  return blocks.map(blockText).join(" ");
}

function blockText(block: Block): string {
  switch (block.kind) {
    case "heading":
    case "paragraph":
      return inlineText(block.content);
    case "code":
      return block.text;
    case "list":
      return block.items
        .map((item) => `${item.checked === null ? "" : item.checked ? "[x]" : "[ ]"} ${textOf(item.blocks)}`)
        .join(" ");
    case "quote":
      return textOf(block.blocks);
    case "table":
      return [
        ...block.head.map(inlineText),
        ...block.rows.map((row) => row.map(inlineText).join(" ")),
      ].join(" ");
    case "footnotes":
      return block.items.map((item) => inlineText(item.content)).join(" ");
    case "rule":
      return "";
  }
}

function inlineText(inlines: readonly Inline[]): string {
  return inlines
    .map((inline) => {
      switch (inline.kind) {
        case "text":
        case "code":
          return inline.text;
        case "strong":
        case "emphasis":
        case "strike":
        case "link":
          return inlineText(inline.children);
        case "image":
          return inline.alt;
        case "footnote":
          return inline.label;
        case "break":
          return " ";
      }
    })
    .join("");
}

const NAMED_ENTITIES: Record<string, string> = {
  amp: "&",
  lt: "<",
  gt: ">",
  quot: '"',
  apos: "'",
};

/**
 * The text the spec's own HTML puts on the page: tags dropped, entities decoded.
 *
 * One pass, because entity decoding is not idempotent: `&amp;#35;` must come out as the literal
 * `&#35;`, and a second pass over the result would turn it into `#`. (The spec has an example for
 * exactly that.) Numeric references that are not a Unicode scalar — the spec also has those —
 * are left as written rather than crashing the run.
 */
function specText(html: string): string {
  return html.replace(/<[^>]*>|&[#a-zA-Z0-9]+;/g, (token: string) => {
    if (token.startsWith("<")) return "";
    const body = token.slice(1, -1);
    if (/^#[xX][0-9a-fA-F]+$/.test(body)) {
      return decodeCodePoint(Number.parseInt(body.slice(2), 16), token);
    }
    if (/^#\d+$/.test(body)) {
      return decodeCodePoint(Number.parseInt(body.slice(1), 10), token);
    }
    return NAMED_ENTITIES[body] ?? token;
  });
}

function decodeCodePoint(value: number, original: string): string {
  const isScalar =
    Number.isInteger(value) &&
    value >= 0 &&
    value <= 0x10ffff &&
    !(value >= 0xd800 && value <= 0xdfff);
  return isScalar ? String.fromCodePoint(value) : original;
}

function comparable(value: string): string {
  return value.replace(/\s+/g, " ").trim();
}

/**
 * Is every character the spec shows present, in order, in ours?
 *
 * This is the invariant this repository actually promises: syntax we do not interpret is shown
 * as literal text (so our text has *extra* characters — the markers we did not consume), but
 * nothing may ever be **missing**. Exact equality is the stricter conformance question, reported
 * next to it as the gap list.
 */
function keepsEverything(theirs: string, ours: string): boolean {
  let cursor = 0;
  for (const character of theirs) {
    cursor = ours.indexOf(character, cursor);
    if (cursor < 0) return false;
    cursor += 1;
  }
  return true;
}

/**
 * The measured state of 2026-09-15, corpus 0.31.2.
 *
 * `total` is how many examples of that section are inside the measurable range (the exclusions
 * above are subtracted); `identical` counts examples whose text matches the spec's own text
 * exactly; `kept` counts the ones where nothing the spec shows is missing (extra characters are
 * allowed — that is what "syntax we do not interpret stays literal text" looks like).
 *
 * All three are pinned: a change in `total` means the measurable range moved (a new exclusion, a
 * new corpus) and has to be re-baselined on purpose, while a change that lowers `identical` or
 * `kept` is a regression. The numbers, the method and the sections where text is genuinely
 * dropped are written down in `docs/boundaries/markdown.md`.
 */
const BASELINE: Record<string, { total: number; identical: number; kept: number }> = {
  "ATX headings": { total: 18, identical: 17, kept: 18 },
  Autolinks: { total: 15, identical: 13, kept: 15 },
  "Backslash escapes": { total: 10, identical: 8, kept: 10 },
  "Blank lines": { total: 1, identical: 1, kept: 1 },
  "Block quotes": { total: 25, identical: 24, kept: 25 },
  "Code spans": { total: 21, identical: 21, kept: 21 },
  "Emphasis and strong emphasis": { total: 123, identical: 123, kept: 123 },
  // 命名引用（`&copy;`）是有意不解码的：那要一张 2231 条的 HTML5 名字表，见
  // `docs/boundaries/markdown.md`。这两个丢字的例子就是它 —— #25 与 #41。
  "Entity and numeric character references": { total: 14, identical: 12, kept: 12 },
  "Fenced code blocks": { total: 29, identical: 29, kept: 29 },
  "Hard line breaks": { total: 13, identical: 13, kept: 13 },
  Images: { total: 2, identical: 1, kept: 2 },
  "Indented code blocks": { total: 12, identical: 12, kept: 12 },
  Inlines: { total: 1, identical: 1, kept: 1 },
  "Link reference definitions": { total: 10, identical: 6, kept: 10 },
  Links: { total: 24, identical: 16, kept: 24 },
  "List items": { total: 48, identical: 47, kept: 48 },
  Lists: { total: 26, identical: 23, kept: 26 },
  Paragraphs: { total: 8, identical: 8, kept: 8 },
  Precedence: { total: 1, identical: 1, kept: 1 },
  "Setext headings": { total: 27, identical: 23, kept: 27 },
  "Soft line breaks": { total: 2, identical: 2, kept: 2 },
  Tabs: { total: 11, identical: 11, kept: 11 },
  "Textual content": { total: 3, identical: 3, kept: 3 },
  "Thematic breaks": { total: 19, identical: 19, kept: 19 },
};

test("every supported CommonMark example keeps its content", () => {
  const path = specPath();
  if (!path) {
    // Same contract as the other live tests: no corpus, no network, no surprise.
    return;
  }
  const corpus = loadCorpus(path);
  const totals = new Map<
    string,
    { total: number; identical: number; kept: number; lost: number[] }
  >();
  let excluded = 0;

  for (const example of corpus) {
    if (DEVIATION_SECTIONS.has(example.section) || requiresUnsupportedTarget(example.html)) {
      excluded += 1;
      continue;
    }
    const bucket = totals.get(example.section) ?? {
      total: 0,
      identical: 0,
      kept: 0,
      lost: [],
    };
    bucket.total += 1;
    const ours = comparable(textOf(parseMarkdown(example.markdown)));
    const theirs = comparable(specText(example.html));
    if (ours === theirs) bucket.identical += 1;
    if (keepsEverything(theirs, ours)) bucket.kept += 1;
    else bucket.lost.push(example.example);
    totals.set(example.section, bucket);
  }

  const rows = [...totals.entries()].sort((left, right) => left[0].localeCompare(right[0]));
  const totalsOf = [...totals.values()].reduce(
    (sum, bucket) => ({
      total: sum.total + bucket.total,
      identical: sum.identical + bucket.identical,
      kept: sum.kept + bucket.kept,
    }),
    { total: 0, identical: 0, kept: 0 },
  );
  console.log("section / text identical to spec / nothing lost / examples — failing ids");
  for (const [section, bucket] of rows) {
    const marker = SUPPORTED_SECTIONS.has(section) ? " (supported)" : "";
    console.log(
      `  ${section}${marker}: ${bucket.identical}/${bucket.total} identical, ${bucket.kept}/${bucket.total} kept${
        bucket.lost.length ? ` — LOST: ${bucket.lost.slice(0, 6).join(", ")}` : ""
      }`,
    );
  }
  console.log(
    `  TOTAL: ${totalsOf.identical}/${totalsOf.total} identical, ${totalsOf.kept}/${totalsOf.total} kept`,
  );
  console.log(
    `  excluded as intentional deviations: ${excluded} (HTML-as-text, images not fetched, non-http(s) link targets)`,
  );

  const regressions: string[] = [];
  for (const [section, bucket] of rows) {
    const floor = BASELINE[section];
    if (!floor) {
      regressions.push(`${section}: not in the recorded baseline (new section in the corpus?)`);
      continue;
    }
    if (bucket.total !== floor.total) {
      regressions.push(
        `${section}: measured ${bucket.total} examples, the baseline covers ${floor.total} — the range moved; re-baseline on purpose`,
      );
      continue;
    }
    if (bucket.identical < floor.identical) {
      regressions.push(
        `${section}: ${bucket.identical} text-identical, was ${floor.identical} — inspect ${bucket.lost.join(", ") || "the report above"}`,
      );
    }
    if (bucket.kept < floor.kept) {
      regressions.push(
        `${section}: ${bucket.kept} kept, was ${floor.kept} — text is now being lost`,
      );
    }
  }
  assert.deepEqual(regressions, [], "the measured gaps may shrink, never grow");
});
