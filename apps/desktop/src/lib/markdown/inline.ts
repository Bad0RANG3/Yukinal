/**
 * 行内解析 —— 一行文本变成 `Inline` 节点树。
 *
 * 只处理行内语法：`code`、**粗**、*斜*、~~删除~~、[链接](url)、裸 URL、图片、硬换行，
 * 以及数字字符引用（`&#35;`、`&#x22;`）。块的切分在 `./block.js`。
 *
 * 强调按 CommonMark 0.31.2 分两步：扫描时只把 `*`/`_` 的 delimiter run 连同「能不能开、
 * 能不能关」记下来，扫描完再按规范第 9、10 条与第 13–17 条配对。早先的写法在扫描时就用
 * `indexOf` 往回找结束标记，于是 `a**"foo"**`、`*foo**bar*`、`foo__bar__` 这类例子会被
 * 配成规范不认的强调，标记因此从正文里消失 —— 实测丢字的大头就在这里。
 *
 * 实体只解码**数字**引用：命名引用（`&copy;`）要一张两千多条的 HTML5 名字表，那份数据不
 * 适合塞进这个仓库，所以按字面显示（见 `docs/boundaries/markdown.md` 的偏差清单）。
 *
 * 性能上的那条约束写在 `parseInline` 里：所有子解析先看首字符再进各自的解析，
 * 逐字符无脑调用会让一段长正文变成 O(n²)，而流式输出每次增量都会重排一次。
 */

import type { Inline } from "./types.js";

/** 能解码出链接的 scheme。其余（`javascript:`、`data:`、`file:`…）一律当普通文本。 */
const SAFE_SCHEME = /^(?:https?:\/\/|mailto:|#)/i;

export type MarkdownReferences = ReadonlyMap<string, string>;

export function parseInline(source: string, references: MarkdownReferences = new Map()): Inline[] {
  const draft = new Draft();
  let pending = "";
  let index = 0;

  const flush = () => {
    draft.pushText(pending);
    pending = "";
  };

  while (index < source.length) {
    const char = source[index] ?? "";

    if (char === "\\" && isEscapable(source[index + 1] ?? "")) {
      pending += source[index + 1] ?? "";
      index += 2;
      continue;
    }

    if (char === "\n" || char === "\r") {
      // 段落是整段交给这里的，所以换行也是行内语法：行尾两个空格或一个反斜杠是**硬换行**，
      // 其余是软换行（渲染成一个空格）。整段一起解析才能让代码段、强调跨行 ——
      // 逐行解析时 `` `code\ `` 换行 `` span` `` 只是一个没闭合的反引号。
      const hard = lineBreakAt(source, index);
      if (hard === "spaces") pending = pending.replace(/[ \t]+$/, "");
      else if (hard === "backslash") pending = pending.slice(0, -1);
      flush();
      draft.pushNode(hard ? { kind: "break" } : { kind: "text", text: " " });
      index += char === "\r" && source[index + 1] === "\n" ? 2 : 1;
      continue;
    }

    if (char === "&") {
      const reference = readNumericReference(source, index);
      if (reference) {
        pending += reference.text;
        index = reference.next;
        continue;
      }
    }

    if (char === "`") {
      let runLength = 0;
      while (source[index + runLength] === "`") runLength += 1;
      const run = "`".repeat(runLength);
      const close = findBacktickRun(source, index + runLength, runLength);
      if (close !== -1) {
        flush();
        draft.pushNode({ kind: "code", text: codeSpanText(source.slice(index + runLength, close)) });
        index = close + runLength;
        continue;
      }
      pending += run;
      index += runLength;
      continue;
    }

    // 下面几项都先看首字符再进各自的解析：它们的入口都要切片，逐字符无脑调用
    // 会让一段长正文变成 O(n²)（流式输出每次增量都会重排一次，代价直接落在打字上）。
    if (char === "[" && source[index + 1] === "^") {
      const footnote = /^\[\^([^\]\n]+)\]/.exec(source.slice(index));
      if (footnote) {
        flush();
        draft.pushNode({ kind: "footnote", label: footnote[1] ?? "" });
        index += footnote[0].length;
        continue;
      }
    }

    if (char === "[" || (char === "!" && source[index + 1] === "[")) {
      const link = readLink(source, index, char === "!", references);
      if (link) {
        flush();
        draft.pushNode(link.node);
        index = link.next;
        continue;
      }
    }

    if (char === "<") {
      const autolink = /^<([a-z][a-z0-9+.-]*:[^\s<>]+)>/i.exec(source.slice(index));
      const href = autolink ? safeHref(autolink[1] ?? "") : null;
      if (autolink && href) {
        flush();
        draft.pushNode({ kind: "link", href, children: [{ kind: "text", text: autolink[1] ?? "" }] });
        index += autolink[0].length;
        continue;
      }
    }

    if (char === "~") {
      const strike = readStrike(source, index, references);
      if (strike) {
        flush();
        draft.pushNode(strike.node);
        index = strike.next;
        continue;
      }
    }

    if (char === "*" || char === "_") {
      let runLength = 0;
      while (source[index + runLength] === char) runLength += 1;
      const before = source[index - 1] ?? "";
      const after = source[index + runLength] ?? "";
      flush();
      draft.pushDelimiter(
        char,
        runLength,
        delimiterCanOpen(char, before, after),
        delimiterCanClose(char, before, after),
      );
      index += runLength;
      continue;
    }

    if (char === "h" || char === "H") {
      const url = readBareUrl(source, index);
      if (url) {
        flush();
        draft.pushNode({ kind: "link", href: url.href, children: [{ kind: "text", text: url.href }] });
        index = url.next;
        continue;
      }
    }

    pending += char;
    index += 1;
  }

  flush();
  return draft.resolve();
}

function isEscapable(char: string): boolean {
  return /[\\`*_{}[\]()#+\-.!>~|"']/.test(char);
}

/**
 * 这个换行是不是硬换行，以及是哪种标记。
 *
 * 判断必须看**源文本**而不是已经产出的文字：`\\` 转义出来的反斜杠是普通字符，
 * 它不该把下一行变成硬换行（那是「偶数个反斜杠结尾」的情况）。
 */
function lineBreakAt(source: string, index: number): "spaces" | "backslash" | null {
  let backslashes = 0;
  while (source[index - 1 - backslashes] === "\\") backslashes += 1;
  if (backslashes % 2 === 1) return "backslash";
  return /[ \t]{2,}$/.test(source.slice(0, index)) ? "spaces" : null;
}

/* ── 代码段 ───────────────────────────────────────────────────────────── */

/**
 * The closing run of a code span must be **exactly** as long as the opening one.
 *
 * `indexOf` was the wrong tool: it matches a longer run's prefix, so `` ` `` `` ` `` `` ` `` (one
 * backtick, content, one backtick around a two-backtick run) closed on the *first* of the two
 * backticks and produced two empty code spans. A longer run is content, not a closer.
 */
function findBacktickRun(source: string, from: number, length: number): number {
  let index = source.indexOf("`", from);
  while (index !== -1) {
    let run = 0;
    while (source[index + run] === "`") run += 1;
    if (run === length) return index;
    index = source.indexOf("`", index + run);
  }
  return -1;
}

/** 代码段里的换行按原样保留，但首尾各一个空格是语法（`` ` x ` `` → `x`）。 */
function codeSpanText(raw: string): string {
  const collapsed = raw.replace(/\r?\n/g, " ");
  return /^ .* $/.test(collapsed) ? collapsed.slice(1, -1) : collapsed;
}

/* ── 链接与裸 URL ─────────────────────────────────────────────────────── */

function readLink(
  source: string,
  start: number,
  image: boolean,
  references: MarkdownReferences,
): { node: Inline; next: number } | null {
  const opening = image ? "![" : "[";
  if (!source.startsWith(opening, start)) return null;
  const labelStart = start + opening.length;
  const labelEnd = source.indexOf("]", labelStart);
  if (labelEnd === -1 || source.slice(labelStart, labelEnd).includes("\n")) return null;
  const label = source.slice(labelStart, labelEnd);
  let cursor = labelEnd + 1;
  let href: string | null = null;

  if (source[cursor] === "(") {
    const match = /^\(\s*<?([^\s<>()]*)>?(?:\s+(?:"[^"\n]*"|'[^'\n]*'|\([^)\n]*\)))?\s*\)/.exec(
      source.slice(cursor),
    );
    if (!match) return null;
    href = safeHref(match[1] ?? "");
    cursor += match[0].length;
  } else {
    let referenceLabel = label;
    let next = cursor;
    if (source[cursor] === "[") {
      const referenceEnd = source.indexOf("]", cursor + 1);
      if (referenceEnd === -1) return null;
      const explicit = source.slice(cursor + 1, referenceEnd);
      referenceLabel = explicit || label;
      next = referenceEnd + 1;
    }
    href = references.get(normalizeReference(referenceLabel)) ?? null;
    cursor = next;
  }

  // The scheme is unknown or the reference has no definition: keep every byte as text.
  if (!href) return null;
  const parsedChildren = parseInline(label, references);
  return {
    node: image ? { kind: "image", href, alt: label } : { kind: "link", href, children: parsedChildren },
    next: cursor,
  };
}

export function normalizeReference(label: string): string {
  return label.trim().replace(/\s+/g, " ").toLowerCase();
}

function safeHref(href: string): string | null {
  const trimmed = href.trim();
  if (!trimmed || !SAFE_SCHEME.test(trimmed)) return null;
  return trimmed;
}

function readBareUrl(source: string, start: number): { href: string; next: number } | null {
  if (!/^https?:\/\//i.test(source.slice(start, start + 8))) return null;
  // 前面贴着字母数字说明它是某个标识符的一部分（例如 `xhttps://`），不是裸 URL。
  const before = source[start - 1] ?? "";
  if (before && /[A-Za-z0-9]/.test(before)) return null;

  const match = /^https?:\/\/[^\s<>"'`]+/i.exec(source.slice(start));
  if (!match) return null;
  let raw = match[0];
  // 句末的标点属于句子，不属于 URL。
  raw = raw.replace(/[.,;:!?，。；：！？、）】》]+$/, "");
  while (raw.endsWith(")") && (raw.match(/\(/g)?.length ?? 0) < (raw.match(/\)/g)?.length ?? 0)) {
    raw = raw.slice(0, -1);
  }
  if (raw.length <= "https://".length) return null;
  return { href: raw, next: start + raw.length };
}

/* ── 数字字符引用 ─────────────────────────────────────────────────────── */

/**
 * `&#35;`（十进制 1–7 位）与 `&#x22;`（十六进制 1–6 位）。
 *
 * 位数超了、少了分号、或者名字不是数字，都**不是**引用，原样留下。命名引用（`&copy;`）
 * 不解码：那要一张 HTML5 的完整名字表，见文件头的说明。解出来的字符当普通文字，不会
 * 再被当成强调标记（规范：`&#42;` 不能顶替 `*` 当强调标记）。
 */
const NUMERIC_REFERENCE = /^&#(?:[xX]([0-9a-fA-F]{1,6})|([0-9]{1,7}));/;

function readNumericReference(source: string, start: number): { text: string; next: number } | null {
  const match = NUMERIC_REFERENCE.exec(source.slice(start));
  if (!match) return null;
  const hex = match[1];
  const value = Number.parseInt(hex ?? match[2] ?? "", hex === undefined ? 10 : 16);
  return { text: codePointText(value), next: start + match[0].length };
}

/** 规范：非法码位与 `U+0000` 都换成替换字符（`&#87654321;` 那种是位数超了，根本不是引用）。 */
function codePointText(value: number): string {
  const isScalar =
    Number.isInteger(value) && value > 0 && value <= 0x10ffff && !(value >= 0xd800 && value <= 0xdfff);
  return isScalar ? String.fromCodePoint(value) : "\uFFFD";
}

/* ── 删除线 ───────────────────────────────────────────────────────────── */

/**
 * `~~删除~~`。GFM 的删除线不在 CommonMark 里，也不进下面的强调配对：两边都要求正好
 * 两个波浪号（`~~~` 是代码围栏或普通文字），中间有内容才成立，否则 `~~` 按字面留下。
 */
function readStrike(
  source: string,
  start: number,
  references: MarkdownReferences,
): { node: Inline; next: number } | null {
  if (!source.startsWith("~~", start) || source[start - 1] === "~" || source[start + 2] === "~") {
    return null;
  }
  if (!leftFlanking(source[start - 1] ?? "", source[start + 2] ?? "")) return null;

  let index = source.indexOf("~~", start + 2);
  while (index !== -1) {
    const isRun = source[index - 1] !== "~" && source[index + 2] !== "~";
    if (isRun && rightFlanking(source[index - 1] ?? "", source[index + 2] ?? "")) {
      const body = source.slice(start + 2, index);
      if (body.trim().length) {
        return {
          node: { kind: "strike", children: parseInline(body, references) },
          next: index + 2,
        };
      }
    }
    index = source.indexOf("~~", index + 2);
  }
  return null;
}

/* ── 强调的 delimiter run ─────────────────────────────────────────────── */

/**
 * 扫描期间的行内片段链：最终节点、待定的强调标记、普通文字。
 *
 * 用双向链表而不是数组，是因为配对成功后要把开始与结束标记之间的片段整个换成新节点；
 * 数组做这件事得反复 `splice`，链表只要改两个指针。
 */
type Piece = TextPiece | NodePiece | DelimiterPiece;

interface TextPiece {
  kind: "text";
  text: string;
  prev: Piece | null;
  next: Piece | null;
}

interface NodePiece {
  kind: "node";
  node: Inline;
  prev: Piece | null;
  next: Piece | null;
}

interface DelimiterPiece {
  kind: "delimiter";
  marker: "*" | "_";
  /** 还没被吃掉的标记个数：`***x***` 配对两次之后两边都会降到 0。 */
  length: number;
  canOpen: boolean;
  canClose: boolean;
  prev: Piece | null;
  next: Piece | null;
}

class Draft {
  private head: Piece | null = null;
  private tail: Piece | null = null;

  pushText(text: string): void {
    if (!text) return;
    // 相邻文字合并：配对时要逐个片段走一遍，碎片越少越好。
    if (this.tail?.kind === "text") {
      this.tail.text += text;
      return;
    }
    this.append({ kind: "text", text, prev: null, next: null });
  }

  pushNode(node: Inline): void {
    this.append({ kind: "node", node, prev: null, next: null });
  }

  pushDelimiter(marker: "*" | "_", length: number, canOpen: boolean, canClose: boolean): void {
    this.append({ kind: "delimiter", marker, length, canOpen, canClose, prev: null, next: null });
  }

  /**
   * 规范里的「处理强调」：从左往右看每个能当结束标记的 run，向左找最近的能当开始标记的
   * 同字符 run。先配上的先成立（第 15 条），回看最近的那个（第 16 条），一次最多吃两边
   * 各两个标记（第 13/14 条：嵌套越少越好、`<em><strong>` 优先于 `<strong><em>`）。
   * 没配上的标记原样留在正文里，所以「不认识的东西一个字都不能丢」在这里也成立。
   */
  resolve(): Inline[] {
    let closer = nextDelimiter(this.head);
    while (closer !== null) {
      if (closer.canClose) {
        const opener = findOpener(closer);
        if (opener) {
          wrap(opener, closer);
          // 标记还有剩就原地再配一层：`***x***` 先配出 strong，剩下的配 em。
          if (closer.length === 0) closer = nextDelimiter(closer.next);
          continue;
        }
        // 既不能开也不能关的标记从此退出候选，后面的 closer 不必再看它。
        // 注意这里只是让它「不再是候选」，字符本身必须留在正文里 —— 把片段从链表上
        // 摘掉会把 `a*"foo"*` 的两个星号一起吞掉。
        if (!closer.canOpen) closer.canClose = false;
      }
      closer = nextDelimiter(closer.next);
    }
    return materialize(this.head);
  }

  private append(piece: Piece): void {
    piece.prev = this.tail;
    if (this.tail) this.tail.next = piece;
    else this.head = piece;
    this.tail = piece;
  }

}

/** 从 `piece` 起（含它自己）向右找第一个还有标记的 run。往后走时记得传 `piece.next`。 */
function nextDelimiter(piece: Piece | null): DelimiterPiece | null {
  for (let cursor = piece; cursor !== null; cursor = cursor.next) {
    if (cursor.kind === "delimiter" && cursor.length > 0) return cursor;
  }
  return null;
}

function findOpener(closer: DelimiterPiece): DelimiterPiece | null {
  for (let piece = closer.prev; piece !== null; piece = piece.prev) {
    if (piece.kind !== "delimiter") continue;
    if (piece.length === 0 || piece.marker !== closer.marker || !piece.canOpen) continue;
    if (!ruleOfThree(piece, closer)) continue;
    return piece;
  }
  return null;
}

/**
 * 规范第 9、10 条：有一侧既能开又能关时，两个 run 的长度之和不能是 3 的倍数，
 * 除非两边各自都是 3 的倍数。少了这条，`*foo**bar*` 会被配成规范不认的强调。
 */
function ruleOfThree(opener: DelimiterPiece, closer: DelimiterPiece): boolean {
  if (!opener.canClose && !closer.canOpen) return true;
  const sum = opener.length + closer.length;
  return sum % 3 !== 0 || (opener.length % 3 === 0 && closer.length % 3 === 0);
}

/** 把开始与结束标记之间的一切收进一个 strong/emphasis 节点，剩下的标记留在原位。 */
function wrap(opener: DelimiterPiece, closer: DelimiterPiece): void {
  const width = opener.length >= 2 && closer.length >= 2 ? 2 : 1;
  const children = collect(opener.next, closer);
  const node: Inline = width === 2 ? { kind: "strong", children } : { kind: "emphasis", children };
  // 中间那些片段已经收进 children，这里直接把链表的这一段换成新节点。
  const wrapper: NodePiece = { kind: "node", node, prev: opener, next: closer };
  opener.next = wrapper;
  closer.prev = wrapper;
  opener.length -= width;
  closer.length -= width;
}

function materialize(head: Piece | null): Inline[] {
  return collect(head, null);
}

/**
 * 把链表的一段整理成行内节点。
 *
 * 按字面留下来的标记（退出候选的、没配上对的）与相邻文字合并成一个文字节点：不合并的话
 * `snake_case_name` 会变成 5 个 text 节点。节点（代码段、链接、软换行…）则是一道边界，
 * 各自独立 —— 软换行就是一个独立的空格文字节点。
 */
function collect(from: Piece | null, to: Piece | null): Inline[] {
  const nodes: Inline[] = [];
  let openText: Extract<Inline, { kind: "text" }> | null = null;

  for (let piece = from; piece !== null && piece !== to; piece = piece.next) {
    if (piece.kind === "node") {
      nodes.push(piece.node);
      openText = null;
      continue;
    }
    // 没配上对的标记按字面留下 —— 语法不认识它，但它一个字都不能丢。
    const text = piece.kind === "text" ? piece.text : piece.marker.repeat(piece.length);
    if (!text) continue;
    if (openText) {
      openText.text += text;
      continue;
    }
    openText = { kind: "text", text };
    nodes.push(openText);
  }
  return nodes;
}

/**
 * CommonMark 的「Unicode 标点」= general category **P 或 S**。
 *
 * 用一张 ASCII 表是不行的：规范里 `*$*alpha.`、`*£*bravo.`、`*€*charlie.` 三条例子钉的就是
 * `$`/`£`/`€`（Sc，符号）也算标点，否则它们会被解析成斜体。`\p{P}\p{S}` 正好是这两类。
 */
const PUNCTUATION = /[\p{P}\p{S}]/u;

/** 规范里的 Unicode 空白 = `Zs` 加上 tab / LF / FF / CR（比 JS 的 `\s` 窄一点）。 */
const WHITESPACE = /[\t\n\f\r]|\p{Zs}/u;

function isPunctuation(char: string): boolean {
  return char.length > 0 && PUNCTUATION.test(char);
}

/** 行首与行尾按规范当成空白。 */
function isWhitespace(char: string): boolean {
  return char === "" || WHITESPACE.test(char);
}

/** 左贴边：可以开一段强调。 */
function leftFlanking(before: string, after: string): boolean {
  if (isWhitespace(after)) return false;
  return !isPunctuation(after) || isWhitespace(before) || isPunctuation(before);
}

/** 右贴边：可以关一段强调。 */
function rightFlanking(before: string, after: string): boolean {
  if (isWhitespace(before)) return false;
  return !isPunctuation(before) || isWhitespace(after) || isPunctuation(after);
}

/** 规范第 1/5 条与第 2/6 条：`_` 不能用在词中间，`*` 可以。 */
function delimiterCanOpen(marker: string, before: string, after: string): boolean {
  if (!leftFlanking(before, after)) return false;
  return marker === "*" || !rightFlanking(before, after) || isPunctuation(before);
}

/** 规范第 3/7 条与第 4/8 条。 */
function delimiterCanClose(marker: string, before: string, after: string): boolean {
  if (!rightFlanking(before, after)) return false;
  return marker === "*" || !leftFlanking(before, after) || isPunctuation(after);
}
