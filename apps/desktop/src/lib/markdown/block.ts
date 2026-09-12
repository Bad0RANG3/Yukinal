/**
 * 块级解析 —— 一份 Markdown 文本变成 `Block` 列表。
 *
 * 支持的语法（其余一律按纯文本处理，绝不猜）：
 *   ATX 标题 `#`…`######`；`-` `*` `+` 与 `1.` `1)` 列表（可嵌套、可 `[x]` 勾选）；
 *   围栏代码块 ``` 与 ~~~（**未闭合时把剩下的都当代码** —— 流式输出里它一定闭不上）；
 *   引用 `>`；表格（含 `:---:` 对齐）；分隔线；段落。
 *
 * 刻意不支持：
 *   - HTML（内联与块级都是普通文字，见 `lib/markdown.js`）。
 *   - setext 标题（`---` 下划线）：`---` 同时是分隔线，猜错会把整段正文变成标题。
 *   - 缩进代码块（4 个空格）：列表项的内容同样缩进，两者无法可靠区分。
 *
 * 每个块的内容交给 `./inline.js` 处理行内语法，这里只负责「哪一段是哪一种块」。
 */

import { parseInline } from "./inline.js";
import type { Block, ColumnAlign, Inline, ListItem } from "./types.js";

/**
 * 嵌套深度上限。列表与引用都会递归重排自己的内容，深度不设上界时一段恶意缩进
 * 就能把栈打穿。超限之后按段落处理：结构会退化成平铺，但内容一个字都不丢。
 */
const MAX_DEPTH = 6;

export function parseMarkdown(source: string): Block[] {
  return parseBlocks(normalize(source), 0);
}

function normalize(source: string): string[] {
  // 制表符先展开：缩进判断按空格数做，留下 `\t` 会让「缩进了几个字符」有两种答案。
  return source.replace(/\r\n?/g, "\n").replace(/\t/g, "    ").split("\n");
}

function parseBlocks(lines: string[], depth: number): Block[] {
  if (depth > MAX_DEPTH) return paragraphsOnly(lines);

  const blocks: Block[] = [];
  let index = 0;

  while (index < lines.length) {
    const line = lines[index] ?? "";
    if (isBlank(line)) {
      index += 1;
      continue;
    }

    const fence = matchFence(line);
    if (fence) {
      const [block, next] = readFence(lines, index, fence);
      blocks.push(block);
      index = next;
      continue;
    }

    const heading = matchHeading(line);
    if (heading) {
      blocks.push(heading);
      index += 1;
      continue;
    }

    if (matchRule(line)) {
      blocks.push({ kind: "rule" });
      index += 1;
      continue;
    }

    if (matchQuoteMarker(line) !== null) {
      const [block, next] = readQuote(lines, index, depth);
      blocks.push(block);
      index = next;
      continue;
    }

    const table = readTable(lines, index);
    if (table) {
      blocks.push(table.block);
      index = table.next;
      continue;
    }

    if (matchItem(line)) {
      const [block, next] = readList(lines, index, depth);
      blocks.push(block);
      index = next;
      continue;
    }

    const [block, next] = readParagraph(lines, index);
    blocks.push(block);
    index = next;
  }

  return blocks;
}

/** 深度超限时的兜底：只认空行分段，别的结构一个都不解析。 */
function paragraphsOnly(lines: string[]): Block[] {
  const blocks: Block[] = [];
  let group: string[] = [];
  const flush = () => {
    if (group.length) blocks.push({ kind: "paragraph", content: paragraphContent(group) });
    group = [];
  };
  for (const line of lines) {
    if (isBlank(line)) flush();
    else group.push(line.trim());
  }
  flush();
  return blocks;
}

function isBlank(line: string): boolean {
  return line.trim().length === 0;
}

function indentOf(line: string): number {
  return line.length - line.trimStart().length;
}

/* ── 块级 ───────────────────────────────────────────────────────────────── */

type FenceMarker = { char: string; count: number; language: string | null };

function matchFence(line: string): FenceMarker | null {
  const match = /^ {0,3}(`{3,}|~{3,})[ \t]*(.*)$/.exec(line);
  if (!match) return null;
  const marker = match[1] ?? "";
  const info = (match[2] ?? "").trim();
  // 反引号围栏的 info string 里不能再出现反引号，否则它是正文里的一串反引号。
  if (marker.startsWith("`") && info.includes("`")) return null;
  return { char: marker[0] ?? "`", count: marker.length, language: info ? (info.split(/\s+/)[0] ?? null) : null };
}

function readFence(lines: string[], start: number, fence: FenceMarker): [Block, number] {
  const text: string[] = [];
  let index = start + 1;
  const closer = new RegExp(`^ {0,3}\\${fence.char}{${fence.count},}[ \\t]*$`);
  while (index < lines.length) {
    const line = lines[index] ?? "";
    if (closer.test(line)) {
      index += 1;
      break;
    }
    text.push(line);
    index += 1;
  }
  // 没有闭合就是流式输出走到这里：到哪儿算哪儿，剩下的全是代码。
  return [{ kind: "code", language: fence.language, text: text.join("\n") }, index];
}

function matchHeading(line: string): Extract<Block, { kind: "heading" }> | null {
  const match = /^ {0,3}(#{1,6})(?:[ \t]+(.*?))?[ \t]*$/.exec(line);
  if (!match) return null;
  const level = (match[1] ?? "#").length;
  // 收尾的一串 `#` 只是装饰（`## 标题 ##`），前面必须有空白才算，否则 `C#` 会被吃掉。
  const content = (match[2] ?? "").replace(/[ \t]+#+[ \t]*$/, "");
  return { kind: "heading", level, content: parseInline(content.trim()) };
}

function matchRule(line: string): boolean {
  return /^ {0,3}(?:(?:\*[ \t]*){3,}|(?:-[ \t]*){3,}|(?:_[ \t]*){3,})$/.test(line);
}

function matchQuoteMarker(line: string): string | null {
  const match = /^ {0,3}>[ \t]?(.*)$/.exec(line);
  return match ? (match[1] ?? "") : null;
}

/**
 * 引用只吃带 `>` 标记的行（以及夹在两个标记行之间的空行）。CommonMark 的
 * lazy continuation（不带标记的续行也算引用）在这里被放弃：它会把紧跟在引用后面、
 * 中间没空行的正文整段吞进引用里，而模型输出里两者都常见。
 */
function readQuote(lines: string[], start: number, depth: number): [Block, number] {
  const inner: string[] = [];
  let index = start;
  while (index < lines.length) {
    const line = lines[index] ?? "";
    const stripped = matchQuoteMarker(line);
    if (stripped !== null) {
      inner.push(stripped);
      index += 1;
      continue;
    }
    if (isBlank(line) && matchQuoteMarker(lines[index + 1] ?? "") !== null) {
      inner.push("");
      index += 1;
      continue;
    }
    break;
  }
  return [{ kind: "quote", blocks: parseBlocks(inner, depth + 1) }, index];
}

/* ── 表格 ───────────────────────────────────────────────────────────────── */

function splitRow(line: string): string[] {
  const trimmed = line.trim().replace(/^\|/, "").replace(/\|$/, "");
  const cells: string[] = [];
  let current = "";
  for (let index = 0; index < trimmed.length; index += 1) {
    const char = trimmed[index] ?? "";
    if (char === "\\" && trimmed[index + 1] === "|") {
      current += "|";
      index += 1;
      continue;
    }
    if (char === "|") {
      cells.push(current.trim());
      current = "";
      continue;
    }
    current += char;
  }
  cells.push(current.trim());
  return cells;
}

function matchAlignRow(line: string): ColumnAlign[] | null {
  if (!line.includes("|")) return null;
  const cells = splitRow(line);
  if (!cells.length) return null;
  const align: ColumnAlign[] = [];
  for (const cell of cells) {
    if (!/^:?-+:?$/.test(cell)) return null;
    const left = cell.startsWith(":");
    const right = cell.endsWith(":");
    align.push(left && right ? "center" : right ? "right" : left ? "left" : null);
  }
  return align;
}

function readTable(lines: string[], start: number): { block: Block; next: number } | null {
  const header = lines[start] ?? "";
  if (!header.includes("|")) return null;
  const align = matchAlignRow(lines[start + 1] ?? "");
  if (!align) return null;

  const head = splitRow(header);
  // 列数不一致时它不是表格，只是正文里刚好有两行带竖线的文字。
  if (head.length !== align.length) return null;

  const rows: Inline[][][] = [];
  let index = start + 2;
  while (index < lines.length) {
    const line = lines[index] ?? "";
    if (isBlank(line) || !line.includes("|") || matchItem(line)) break;
    const cells = splitRow(line);
    // 单元格数与表头不一致时按表头截断/补齐，而不是丢掉整行结果。
    rows.push(align.map((_, column) => parseInline(cells[column] ?? "")));
    index += 1;
  }

  return {
    block: { kind: "table", align, head: head.map((cell) => parseInline(cell)), rows },
    next: index,
  };
}

/* ── 列表 ───────────────────────────────────────────────────────────────── */

type ItemMarker = {
  ordered: boolean;
  number: number;
  indent: number;
  content: string;
  checked: boolean | null;
};

function matchItem(line: string): ItemMarker | null {
  const match = /^([ \t]*)([-*+]|\d{1,9}[.)])(?:[ \t]+(.*))?$/.exec(line);
  if (!match) return null;
  const marker = match[2] ?? "-";
  const ordered = /\d/.test(marker[0] ?? "");
  const body = match[3] ?? "";
  const task = /^\[([ xX])\][ \t]+(.*)$/.exec(body);
  return {
    ordered,
    number: ordered ? Number.parseInt(marker, 10) : 1,
    indent: (match[1] ?? "").length,
    content: task ? (task[2] ?? "") : body,
    checked: task ? (task[1] ?? "").toLowerCase() === "x" : null,
  };
}

/**
 * 列表按缩进递归：每个列表项的续行（缩进更深的那些）去掉公共缩进之后**当成一段
 * 独立的 Markdown** 再解析一次，于是嵌套列表、项内多段、项内代码块都自然成立，
 * 不需要为每种组合写一条规则。
 */
function readList(lines: string[], start: number, depth: number): [Block, number] {
  const first = matchItem(lines[start] ?? "");
  if (!first) return readParagraph(lines, start);

  const baseIndent = first.indent;
  const ordered = first.ordered;
  const startNumber = first.number;
  const items: ListItem[] = [];
  let index = start;

  while (index < lines.length) {
    const marker = matchItem(lines[index] ?? "");
    if (!marker || marker.indent !== baseIndent || marker.ordered !== ordered) break;

    const content: string[] = [marker.content];
    const checked = marker.checked;
    index += 1;

    while (index < lines.length) {
      const line = lines[index] ?? "";
      if (isBlank(line)) {
        // 空行只有在后面还有本项的内容时才属于这一项，否则它就是列表的结束。
        const following = lines[index + 1] ?? "";
        if (!isBlank(following) && indentOf(following) > baseIndent) {
          content.push("");
          index += 1;
          continue;
        }
        break;
      }
      const sibling = matchItem(line);
      if (sibling && sibling.indent <= baseIndent) break;
      if (indentOf(line) > baseIndent) {
        content.push(line);
        index += 1;
        continue;
      }
      // 与标记同列却不是标记的行：CommonMark 会当成 lazy continuation，
      // 这里也照收，否则「- 一行\n下一行」会把后半句甩到列表外面。
      content.push(line);
      index += 1;
    }

    items.push({ checked, blocks: parseBlocks(dedent(content), depth + 1) });
    while (index < lines.length && isBlank(lines[index] ?? "")) {
      // 空行留在原处由外层循环处理，但列表之间的空行不该结束整个列表。
      if (matchItem(lines[index + 1] ?? "")?.indent !== baseIndent) break;
      index += 1;
    }
  }

  return [{ kind: "list", ordered, start: startNumber, items }, index];
}

function dedent(lines: string[]): string[] {
  // 逐个比较而不是 `Math.min(...indents)`：后者在长列表上会把参数铺成上万个实参。
  let common = Number.POSITIVE_INFINITY;
  for (const line of lines.slice(1)) {
    if (isBlank(line)) continue;
    common = Math.min(common, indentOf(line));
  }
  if (!Number.isFinite(common)) common = 0;
  return lines.map((line, position) => (position === 0 || isBlank(line) ? line.trim() : line.slice(common)));
}

/* ── 段落 ───────────────────────────────────────────────────────────────── */

function readParagraph(lines: string[], start: number): [Block, number] {
  const collected: string[] = [];
  let index = start;
  while (index < lines.length) {
    const line = lines[index] ?? "";
    if (isBlank(line)) break;
    if (collected.length && startsBlock(lines, index)) break;
    // 只去左侧缩进：行尾空格是「硬换行」的语法，trim 掉就再也认不出来了。
    collected.push(line.trimStart());
    index += 1;
  }
  return [{ kind: "paragraph", content: paragraphContent(collected) }, Math.max(index, start + 1)];
}

function startsBlock(lines: string[], index: number): boolean {
  const line = lines[index] ?? "";
  return (
    matchFence(line) !== null ||
    matchHeading(line) !== null ||
    matchRule(line) ||
    matchQuoteMarker(line) !== null ||
    matchItem(line) !== null ||
    // 表格的表头行只有在下一行是分隔行时才算块的开头。
    (matchAlignRow(lines[index + 1] ?? "") !== null && line.includes("|"))
  );
}

/**
 * 段落内容：行与行之间按 CommonMark 是**软换行**（渲染成一个空格），只有行尾两个
 * 空格或一个反斜杠才是硬换行。这里不把每个换行都变成 `<br>` —— 模型写回来的段落
 * 通常是拍扁的一长行，逐行断开只会让正文变碎。
 */
function paragraphContent(lines: string[]): Inline[] {
  const content: Inline[] = [];
  lines.forEach((line, position) => {
    if (position > 0) {
      content.push(hardBreak(lines[position - 1] ?? "") ? { kind: "break" } : { kind: "text", text: " " });
    }
    content.push(...parseInline(hardBreak(line) ? line.slice(0, -1).trimEnd() : line.replace(/ {2,}$/, "")));
  });
  return content;
}

function hardBreak(line: string): boolean {
  if (/ {2,}$/.test(line)) return true;
  // 行尾一个反斜杠是硬换行；两个是转义出来的字面反斜杠，不算。
  return line.endsWith("\\") && !line.endsWith("\\\\");
}
