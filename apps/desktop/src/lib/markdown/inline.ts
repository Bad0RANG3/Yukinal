/**
 * 行内解析 —— 一行文本变成 `Inline` 节点树。
 *
 * 只处理行内语法：`code`、**粗**、*斜*、~~删除~~、[链接](url)、裸 URL、图片、硬换行。
 * 块的切分在 `./block.js`，它负责把每段文字交给这里。
 *
 * 性能上的那条约束写在 `parseInline` 里：所有子解析先看首字符再进各自的解析，
 * 逐字符无脑调用会让一段长正文变成 O(n²)，而流式输出每次增量都会重排一次。
 */

import type { Inline } from "./types.js";

/** 能解码出链接的 scheme。其余（`javascript:`、`data:`、`file:`…）一律当普通文本。 */
const SAFE_SCHEME = /^(?:https?:\/\/|mailto:|#)/i;

export function parseInline(source: string): Inline[] {
  const nodes: Inline[] = [];
  let pending = "";
  let index = 0;

  const flush = () => {
    if (pending) nodes.push({ kind: "text", text: pending });
    pending = "";
  };

  while (index < source.length) {
    const char = source[index] ?? "";

    if (char === "\\" && isEscapable(source[index + 1] ?? "")) {
      pending += source[index + 1] ?? "";
      index += 2;
      continue;
    }

    if (char === "`") {
      let runLength = 0;
      while (source[index + runLength] === "`") runLength += 1;
      const run = "`".repeat(runLength);
      const close = source.indexOf(run, index + runLength);
      if (close !== -1) {
        flush();
        nodes.push({ kind: "code", text: codeSpanText(source.slice(index + runLength, close)) });
        index = close + runLength;
        continue;
      }
      pending += run;
      index += runLength;
      continue;
    }

    // 下面几项都先看首字符再进各自的解析：它们的入口都要切片，逐字符无脑调用
    // 会让一段长正文变成 O(n²)（流式输出每次增量都会重排一次，代价直接落在打字上）。
    if (char === "[" || (char === "!" && source[index + 1] === "[")) {
      const link = readLink(source, index, char === "!");
      if (link) {
        flush();
        nodes.push(link.node);
        index = link.next;
        continue;
      }
    }

    if (char === "<") {
      const autolink = /^<([a-z][a-z0-9+.-]*:[^\s<>]+)>/i.exec(source.slice(index));
      const href = autolink ? safeHref(autolink[1] ?? "") : null;
      if (autolink && href) {
        flush();
        nodes.push({ kind: "link", href, children: [{ kind: "text", text: autolink[1] ?? "" }] });
        index += autolink[0].length;
        continue;
      }
    }

    if (EMPHASIS_CHARS.test(char)) {
      const emphasis = readEmphasis(source, index);
      if (emphasis) {
        flush();
        nodes.push(emphasis.node);
        index = emphasis.next;
        continue;
      }
    }

    if (char === "h" || char === "H") {
      const url = readBareUrl(source, index);
      if (url) {
        flush();
        nodes.push({ kind: "link", href: url.href, children: [{ kind: "text", text: url.href }] });
        index = url.next;
        continue;
      }
    }

    pending += char;
    index += 1;
  }

  flush();
  return nodes;
}

function isEscapable(char: string): boolean {
  return /[\\`*_{}[\]()#+\-.!>~|"']/.test(char);
}

/** 代码段里的换行按原样保留，但首尾各一个空格是语法（`` ` x ` `` → `x`）。 */
function codeSpanText(raw: string): string {
  const collapsed = raw.replace(/\r?\n/g, " ");
  return /^ .* $/.test(collapsed) ? collapsed.slice(1, -1) : collapsed;
}

function readLink(source: string, start: number, image: boolean): { node: Inline; next: number } | null {
  const pattern = image
    ? /^!\[([^\]\n]*)\]\(\s*<?([^\s<>()]*)>?(?:\s+"[^"\n]*")?\s*\)/
    : /^\[([^\]\n]*)\]\(\s*<?([^\s<>()]*)>?(?:\s+"[^"\n]*")?\s*\)/;
  const match = pattern.exec(source.slice(start));
  if (!match) return null;
  const href = safeHref(match[2] ?? "");
  // scheme 不认识就整段当普通文本：既不生成节点，也不假装它是链接。
  if (!href) return null;
  const label = match[1] ?? "";
  return {
    node: image ? { kind: "image", href, alt: label } : { kind: "link", href, children: parseInline(label) },
    next: start + match[0].length,
  };
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

/** 行内强调只会从这三个字符开始，先按首字符挡一道再看具体标记。 */
const EMPHASIS_CHARS = /[*_~]/;

const DELIMITERS: Array<{ marker: string; kind: "strong" | "emphasis" | "strike"; wordBound: boolean }> = [
  { marker: "***", kind: "strong", wordBound: false },
  { marker: "___", kind: "strong", wordBound: true },
  { marker: "**", kind: "strong", wordBound: false },
  { marker: "__", kind: "strong", wordBound: true },
  { marker: "~~", kind: "strike", wordBound: false },
  { marker: "*", kind: "emphasis", wordBound: false },
  { marker: "_", kind: "emphasis", wordBound: true },
];

function readEmphasis(source: string, start: number): { node: Inline; next: number } | null {
  for (const { marker, kind, wordBound } of DELIMITERS) {
    if (!source.startsWith(marker, start)) continue;
    if (wordBound && isWordChar(source[start - 1] ?? "")) continue;

    const inner = source.slice(start + marker.length);
    // 开标记后面紧跟空白就不是强调（`* 列表` 这种星号必须留在正文里）。
    if (!inner.length || /^\s/.test(inner)) continue;

    const close = findCloser(source, start + marker.length, marker, wordBound);
    if (close === -1) continue;

    const body = source.slice(start + marker.length, close);
    if (!body.trim().length) continue;

    const children = parseInline(body);
    // `***x***` 是「粗 + 斜」叠在一起，不是三个星号里恰好有一个没配对。
    const node: Inline =
      marker.length === 3 ? { kind: "strong", children: [{ kind: "emphasis", children }] } : { kind, children };
    return { node, next: close + marker.length };
  }
  return null;
}

function findCloser(source: string, from: number, marker: string, wordBound: boolean): number {
  let index = source.indexOf(marker, from);
  while (index !== -1) {
    const before = source[index - 1] ?? "";
    const after = source[index + marker.length] ?? "";
    const closingOk = !/\s/.test(before) && (!wordBound || !isWordChar(after));
    if (closingOk) return index;
    index = source.indexOf(marker, index + 1);
  }
  return -1;
}

function isWordChar(char: string): boolean {
  return /[A-Za-z0-9]/.test(char);
}
