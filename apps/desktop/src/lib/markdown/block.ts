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
 *
 * 容器（列表项、引用）会把自己的内容交给下一层重新解析，所以每一层都只看到「去掉自己
 * 那部分缩进之后」的行。为了让「缩进不够、只能当上一段续行」的行不被下一层误当成新的块
 * （`- a` / ` - b` / `  - c` / `   - d` / `    - e` 里的最后一行就是这个形状），
 * 行在容器里带一个 `lazy` 标记：这种行只剩正文，任何块语法都不再适用。
 */

import { normalizeReference, parseInline, type MarkdownReferences } from "./inline.js";
import type { Block, ColumnAlign, Inline, ListItem } from "./types.js";

/**
 * 嵌套深度上限。列表与引用都会递归重排自己的内容，深度不设上界时一段恶意缩进
 * 就能把栈打穿。超限之后按段落处理：结构会退化成平铺，但内容一个字都不丢。
 */
const MAX_DEPTH = 6;

/**
 * 一行输入，以及它在当前容器里是**哪种**行。
 *
 * `lazy` 对应规范里的 lazy continuation line：缩进不到容器要求、只能算上一段的续行。
 * 这种行即便长得像列表标记、标题或围栏，也一个字都不能当语法用 —— 否则标记会从正文里
 * 消失，而「没认出来的东西一个字都不能丢」正是这里最硬的那条约束。
 */
type Line = { text: string; lazy: boolean };

function plainLines(lines: string[]): Line[] {
  return lines.map((text) => ({ text, lazy: false }));
}

export function parseMarkdown(source: string): Block[] {
  const extracted = extractReferences(normalize(source));
  const footnotes = extractFootnoteDefinitions(extracted.lines);
  const blocks = parseBlocks(plainLines(footnotes.lines), 0, extracted.references);
  if (footnotes.items.length > 0) {
    blocks.push({
      kind: "footnotes",
      items: footnotes.items.map((item) => ({
        label: item.label,
        content: parseInline(item.text, extracted.references),
      })),
    });
  }
  return blocks;
}

function normalize(source: string): string[] {
  // 制表符先展开：缩进判断按空格数做，留下 `\t` 会让「缩进了几个字符」有两种答案。
  return source.replace(/\r\n?/g, "\n").replace(/\t/g, "    ").split("\n");
}

function parseBlocks(lines: Line[], depth: number, references: MarkdownReferences): Block[] {
  return parseBlocksDetailed(lines, depth, references).blocks;
}

type ParsedBlocks = {
  blocks: Block[];
  /**
   * 两个直接子块之间是否至少有一次空行。列表据此判定 tight/loose，但空行若已被
   * 嵌套列表、引用或代码块消费，就不能重复算到外层容器头上。
   */
  blankBetweenBlocks: boolean;
};

function parseBlocksDetailed(
  lines: Line[],
  depth: number,
  references: MarkdownReferences,
): ParsedBlocks {
  if (depth > MAX_DEPTH) return paragraphsOnly(lines, references);

  const blocks: Block[] = [];
  let index = 0;
  let pendingBlank = false;
  let blankBetweenBlocks = false;

  while (index < lines.length) {
    const line = lines[index] ?? { text: "", lazy: false };
    if (isBlank(line.text)) {
      if (blocks.length > 0) pendingBlank = true;
      index += 1;
      continue;
    }
    if (pendingBlank) blankBetweenBlocks = true;
    pendingBlank = false;

    // 续行（lazy）只剩正文：它连块的形状都不许有。
    if (!line.lazy) {
      const fence = matchFence(line.text);
      if (fence) {
        const [block, next] = readFence(lines, index, fence);
        blocks.push(block);
        index = next;
        continue;
      }

      const setext = readSetextHeading(lines, index, references);
      if (setext) {
        blocks.push(setext.block);
        index = setext.next;
        continue;
      }

      const heading = matchHeading(line.text, references);
      if (heading) {
        blocks.push(heading);
        index += 1;
        continue;
      }

      if (indentOf(line.text) >= 4) {
        const [block, next] = readIndentedCode(lines, index);
        blocks.push(block);
        index = next;
        continue;
      }

      if (matchRule(line.text)) {
        blocks.push({ kind: "rule" });
        index += 1;
        continue;
      }

      if (matchQuoteMarker(line.text) !== null) {
        const [block, next] = readQuote(lines, index, depth, references);
        blocks.push(block);
        index = next;
        continue;
      }

      const table = readTable(lines, index, references);
      if (table) {
        blocks.push(table.block);
        index = table.next;
        continue;
      }

      if (matchItem(line.text)) {
        const [block, next] = readList(lines, index, depth, references);
        blocks.push(block);
        index = next;
        continue;
      }
    }

    const [block, next] = readParagraph(lines, index, references);
    blocks.push(block);
    index = next;
  }

  return { blocks, blankBetweenBlocks };
}

/** 深度超限时的兜底：只认空行分段，别的结构一个都不解析。 */
function paragraphsOnly(lines: Line[], references: MarkdownReferences): ParsedBlocks {
  const blocks: Block[] = [];
  let group: Line[] = [];
  let blankBetweenBlocks = false;
  const flush = () => {
    if (group.length) blocks.push({ kind: "paragraph", content: paragraphContent(group, references) });
    group = [];
  };
  for (const line of lines) {
    if (isBlank(line.text)) {
      if (group.length > 0) blankBetweenBlocks = true;
      flush();
    } else {
      group.push({ text: line.text.trim(), lazy: line.lazy });
    }
  }
  flush();
  return { blocks, blankBetweenBlocks };
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

function readFence(lines: Line[], start: number, fence: FenceMarker): [Block, number] {
  const text: string[] = [];
  let index = start + 1;
  const closer = new RegExp(`^ {0,3}\\${fence.char}{${fence.count},}[ \\t]*$`);
  while (index < lines.length) {
    const line = lines[index]?.text ?? "";
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

function matchHeading(
  line: string,
  references: MarkdownReferences = new Map(),
): Extract<Block, { kind: "heading" }> | null {
  const match = /^ {0,3}(#{1,6})(?:[ \t]+(.*?))?[ \t]*$/.exec(line);
  if (!match) return null;
  const level = (match[1] ?? "#").length;
  // 收尾的一串 `#` 只是装饰（`## 标题 ##`），前面必须有空白才算，否则 `C#` 会被吃掉。
  const content = (match[2] ?? "").replace(/[ \t]+#+[ \t]*$/, "");
  return { kind: "heading", level, content: parseInline(content.trim(), references) };
}

function matchSetextUnderline(line: string): number | null {
  const match = /^ {0,3}(=+|-+)[ \t]*$/.exec(line);
  if (!match) return null;
  return (match[1] ?? "=").startsWith("=") ? 1 : 2;
}

function readSetextHeading(
  lines: Line[],
  start: number,
  references: MarkdownReferences,
): { block: Block; next: number } | null {
  const content = lines[start]?.text ?? "";
  if (lines[start]?.lazy) return null;
  if (isBlank(content) || matchFence(content) || matchHeading(content) || matchRule(content) || matchItem(content)) {
    return null;
  }
  const underline = lines[start + 1];
  // 下划线自己不能是续行：规范里专门有一条，`> foo\nbar\n===` 的 `===` 就是普通文字。
  if (!underline || underline.lazy) return null;
  const level = matchSetextUnderline(underline.text);
  if (!level) return null;
  return {
    block: { kind: "heading", level, content: parseInline(content.trim(), references) },
    next: start + 2,
  };
}

function readIndentedCode(lines: Line[], start: number): [Block, number] {
  const text: string[] = [];
  let index = start;
  while (index < lines.length) {
    const line = lines[index];
    if (!line || line.lazy) break;
    if (isBlank(line.text)) {
      text.push("");
      index += 1;
      continue;
    }
    if (indentOf(line.text) < 4) break;
    text.push(line.text.slice(4));
    index += 1;
  }
  while (text.at(-1) === "") text.pop();
  return [{ kind: "code", language: null, text: text.join("\n") }, index];
}

function matchRule(line: string): boolean {
  return /^ {0,3}(?:(?:\*[ \t]*){3,}|(?:-[ \t]*){3,}|(?:_[ \t]*){3,})$/.test(line);
}

function matchQuoteMarker(line: string): string | null {
  const match = /^ {0,3}>[ \t]?(.*)$/.exec(line);
  return match ? (match[1] ?? "") : null;
}

/**
 * 引用先吃带 `>` 标记的行（以及夹在两个标记行之间的空行），再按 CommonMark 接受
 * 段落型的 lazy continuation。只有当引用末尾仍是段落、下一行也不是新块起点时才延续；
 * 因此「引用后紧跟标题或列表」不会被误吞进引用。
 */
function readQuote(
  lines: Line[],
  start: number,
  depth: number,
  references: MarkdownReferences,
): [Block, number] {
  const inner: Line[] = [];
  let index = start;
  while (index < lines.length) {
    const line = lines[index] ?? { text: "", lazy: false };
    const stripped = line.lazy ? null : matchQuoteMarker(line.text);
    if (stripped !== null) {
      inner.push({ text: stripped, lazy: false });
      index += 1;
      continue;
    }
    const following = lines[index + 1];
    if (!line.lazy && isBlank(line.text) && following && !following.lazy && matchQuoteMarker(following.text) !== null) {
      inner.push({ text: "", lazy: false });
      index += 1;
      continue;
    }
    if (
      !line.lazy &&
      !isBlank(line.text) &&
      !startsExplicitBlock(line.text) &&
      quoteEndsInParagraph(inner)
    ) {
      // 引用标记可以省略，但省略的那一行只当上一段的续行（规范里的 lazy continuation）。
      inner.push({ text: line.text, lazy: true });
      index += 1;
      continue;
    }
    break;
  }
  return [{ kind: "quote", blocks: parseBlocks(inner, depth + 1, references) }, index];
}

function startsExplicitBlock(line: string): boolean {
  return (
    matchFence(line) !== null ||
    matchHeading(line) !== null ||
    matchRule(line) ||
    matchItem(line) !== null ||
    matchQuoteMarker(line) !== null ||
    indentOf(line) >= 4
  );
}

function quoteEndsInParagraph(lines: Line[]): boolean {
  const last = lines.at(-1);
  return last !== undefined && !isBlank(last.text) && !startsExplicitBlock(last.text);
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

function readTable(
  lines: Line[],
  start: number,
  references: MarkdownReferences,
): { block: Block; next: number } | null {
  const header = lines[start]?.text ?? "";
  if (!header.includes("|")) return null;
  const align = matchAlignRow(lines[start + 1]?.text ?? "");
  if (!align) return null;

  const head = splitRow(header);
  // 列数不一致时它不是表格，只是正文里刚好有两行带竖线的文字。
  if (head.length !== align.length) return null;

  const rows: Inline[][][] = [];
  let index = start + 2;
  while (index < lines.length) {
    const row = lines[index];
    if (!row || row.lazy) break;
    const line = row.text;
    if (isBlank(line) || !line.includes("|") || matchItem(line)) break;
    const cells = splitRow(line);
    // 单元格数与表头不一致时按表头截断/补齐，而不是丢掉整行结果。
    rows.push(align.map((_, column) => parseInline(cells[column] ?? "", references)));
    index += 1;
  }

  return {
    block: { kind: "table", align, head: head.map((cell) => parseInline(cell, references)), rows },
    next: index,
  };
}

/* ── 列表 ───────────────────────────────────────────────────────────────── */

type ItemMarker = {
  ordered: boolean;
  number: number;
  /** 有序列表的 `.`/`)`，或无序列表的 `-`/`*`/`+`；换标记会开始一个新列表。 */
  delimiter: string;
  indent: number;
  /** 项内容从哪一列开始（绝对列）。它是「后面这一行还算不算本项内容」的分界线。 */
  contentColumn: number;
  content: string;
  checked: boolean | null;
};

/**
 * 列表标记；缩进超过 3 个空格的一律不算（规范：列表项前面最多三个空格）。
 *
 * 少了这条，`- a` / ` - b` / `  - c` / `   - d` / `    - e` 的最后一行会变成第五层列表，
 * 那个 `-` 就从正文里消失了 —— 规范说它是上一段的续行文字。
 */
function matchItem(line: string): ItemMarker | null {
  const match = /^([ \t]*)([-*+]|\d{1,9}[.)])(?:([ \t]+)(.*))?$/.exec(line);
  if (!match) return null;
  const indent = (match[1] ?? "").length;
  if (indent > 3) return null;
  const marker = match[2] ?? "-";
  const ordered = /\d/.test(marker[0] ?? "");
  const gap = (match[3] ?? "").length;
  const body = match[4] ?? "";
  const task = /^\[([ xX])\][ \t]+(.*)$/.exec(body);
  return {
    ordered,
    number: ordered ? Number.parseInt(marker, 10) : 1,
    delimiter: ordered ? (marker.at(-1) ?? ".") : marker,
    indent,
    // 内容列 = 标记列 + 标记宽 + 标记后的空白。标记后留了 5 个以上空格时按规范改成
    // 「标记宽 + 1」：那已经是缩进代码块，多出来的空格属于内容。
    contentColumn: indent + marker.length + (gap > 4 ? 1 : Math.max(gap, 1)),
    content: gap > 4 ? line.slice(indent + marker.length + 1) : task ? (task[2] ?? "") : body,
    checked: task ? (task[1] ?? "").toLowerCase() === "x" : null,
  };
}

/**
 * 列表按缩进递归：每个列表项的续行（缩进更深的那些）去掉公共缩进之后**当成一段
 * 独立的 Markdown** 再解析一次，于是嵌套列表、项内多段、项内代码块都自然成立，
 * 不需要为每种组合写一条规则。
 */
function readList(
  lines: Line[],
  start: number,
  depth: number,
  references: MarkdownReferences,
): [Block, number] {
  const first = matchItem(lines[start]?.text ?? "");
  if (!first) return readParagraph(lines, start, references);

  const ordered = first.ordered;
  const delimiter = first.delimiter;
  const startNumber = first.number;
  const items: ListItem[] = [];
  let tight = true;
  let index = start;
  // 同一层列表只要求标记类型一致：规范允许各项缩进不对齐（`- a` 下面接 ` - b` 仍是同层）。
  const sameType = (marker: ItemMarker | null): boolean =>
    marker !== null && marker.ordered === ordered && marker.delimiter === delimiter;

  while (index < lines.length) {
    const marker = matchItem(lines[index]?.text ?? "");
    if (marker === null || lines[index]?.lazy || !sameType(marker)) break;
    const contentColumn = marker.contentColumn;

    const content: Line[] = [{ text: marker.content, lazy: false }];
    const checked = marker.checked;
    index += 1;

    while (index < lines.length) {
      const line = lines[index] ?? { text: "", lazy: false };
      if (isBlank(line.text)) {
        // 连续空行只有在后面还有本项的内容时才属于这一项，否则它就是列表的结束。
        let followingIndex = index + 1;
        while (followingIndex < lines.length && isBlank(lines[followingIndex]?.text ?? "")) {
          followingIndex += 1;
        }
        const following = lines[followingIndex];
        // 空行之后要缩进到内容列才算本项的内容。`1. a\n\n  2. b\n\n    3. c` 的最后一行
        // 因此落到列表外面，被上一层的「四空格缩进代码」接住 —— 规范就是这么分的。
        if (following && !following.lazy && indentOf(following.text) >= contentColumn) {
          content.push({ text: "", lazy: false });
          index = followingIndex;
          continue;
        }
        break;
      }
      if (!line.lazy && indentOf(line.text) >= contentColumn) {
        // 缩进到内容列：这是本项的内容，下一层可以照常认块（嵌套列表、代码块、引用…）。
        content.push({ text: line.text, lazy: false });
        index += 1;
        continue;
      }
      // 缩进不够：是同级的下一个列表项（类型不同会在外层结束这个列表），否则就是
      // lazy continuation —— 只当上一段的续行，标记/围栏在下一层都只是文字。
      if (line.lazy) break;
      if (matchItem(line.text) !== null) break;
      // 规范里的 laziness 只适用于「段落续行文字」：`- foo\n***\n- bar` 中间那条分隔线
      // 是新块，不能被上一项吞掉（吞掉就变成正文里的三个星号）。
      if (startsExplicitBlock(line.text)) break;
      content.push({ text: line.text, lazy: true });
      index += 1;
    }

    const parsed = parseBlocksDetailed(dedent(content), depth + 1, references);
    if (parsed.blankBetweenBlocks) tight = false;
    items.push({ checked, blocks: parsed.blocks });

    // 同一列表的下一项若隔着空行，整个列表按 CommonMark 变成宽松列表。
    if (isBlank(lines[index]?.text ?? "")) {
      let next = index;
      while (next < lines.length && isBlank(lines[next]?.text ?? "")) next += 1;
      if (!lines[next]?.lazy && sameType(matchItem(lines[next]?.text ?? ""))) {
        tight = false;
        index = next;
      }
    }
  }

  return [{ kind: "list", ordered, start: startNumber, tight, items }, index];
}

function dedent(lines: Line[]): Line[] {
  // 逐个比较而不是 `Math.min(...indents)`：后者在长列表上会把参数铺成上万个实参。
  let common = Number.POSITIVE_INFINITY;
  for (const line of lines.slice(1)) {
    if (isBlank(line.text)) continue;
    common = Math.min(common, indentOf(line.text));
  }
  if (!Number.isFinite(common)) common = 0;
  return lines.map((line, position) => ({
    text: position === 0 || isBlank(line.text) ? line.text.trim() : line.text.slice(common),
    // 续行的身份跟着行走：去掉缩进不能让它变回一个「合法的块开头」。
    lazy: line.lazy,
  }));
}

/* ── 段落 ───────────────────────────────────────────────────────────────── */

function readParagraph(
  lines: Line[],
  start: number,
  references: MarkdownReferences,
): [Block, number] {
  const collected: Line[] = [];
  let index = start;
  while (index < lines.length) {
    const line = lines[index] ?? { text: "", lazy: false };
    if (isBlank(line.text)) break;
    // 续行不可能是块的开始：它只是上一段的文字。
    if (collected.length && !line.lazy && startsBlock(lines, index)) break;
    // 只去左侧缩进：行尾空格是「硬换行」的语法，trim 掉就再也认不出来了。
    collected.push({ text: line.text.trimStart(), lazy: line.lazy });
    index += 1;
  }
  return [{ kind: "paragraph", content: paragraphContent(collected, references) }, Math.max(index, start + 1)];
}

function startsBlock(lines: Line[], index: number): boolean {
  const line = lines[index];
  if (!line || line.lazy) return false;
  return (
    matchFence(line.text) !== null ||
    readSetextHeading(lines, index, new Map()) !== null ||
    matchHeading(line.text) !== null ||
    matchRule(line.text) ||
    matchQuoteMarker(line.text) !== null ||
    interruptsParagraph(line.text) ||
    // 表格的表头行只有在下一行是分隔行时才算块的开头。
    (matchAlignRow(lines[index + 1]?.text ?? "") !== null && line.text.includes("|"))
  );
}

/**
 * Can this line interrupt a paragraph that is already open?
 *
 * CommonMark's exceptions, and both of them are visible in real prose: an **ordered** list only
 * interrupts when it starts at 1 (`14. The number of doors is 6.` is a sentence, not a list), and
 * a list item with no content cannot interrupt anything (`- ` on its own is not an item start).
 * Without this a sentence that happens to begin with a number split into two blocks and the
 * number itself disappeared into the marker.
 */
function interruptsParagraph(line: string): boolean {
  const item = matchItem(line);
  if (!item) return false;
  if (item.content.trim().length === 0) return false;
  return !item.ordered || item.number === 1;
}

/**
 * 段落内容交给行内解析，整段一次。
 *
 * 行与行之间按 CommonMark 是**软换行**（渲染成一个空格），只有行尾两个空格或一个反斜杠
 * 才是硬换行 —— 这两条都在 `parseInline` 里，因为换行也是行内语法：整段一起解析，代码段、
 * 强调、链接才能跨行（逐行看，`` `code\ `` 换行 `` span` `` 只是一个没闭合的反引号）。
 * 这里不把每个换行都变成 `<br>`：模型写回来的段落通常是拍扁的一长行，逐行断开只会让正文变碎。
 */
function paragraphContent(lines: Line[], references: MarkdownReferences): Inline[] {
  const joined = lines
    .map((line, position) =>
      // 段落最后一行结尾的反斜杠是普通文字（后面没有行可换了），行尾空白也一律丢掉。
      position === lines.length - 1 ? line.text.trimEnd() : line.text,
    )
    .join("\n");
  return parseInline(joined, references);
}

function extractReferences(lines: string[]): { lines: string[]; references: MarkdownReferences } {
  const references = new Map<string, string>();
  const remaining = [...lines];
  remaining.forEach((line, index) => {
    const match = /^ {0,3}\[([^\]\n]+)\]:\s*<?([^\s<>]+)>?(?:\s+(?:"[^"\n]*"|'[^'\n]*'|\([^)\n]*\)))?\s*$/.exec(line);
    if (!match) return;
    const label = match[1] ?? "";
    if (label.startsWith("^")) return;
    const href = (match[2] ?? "").trim();
    if (!/^(?:https?:\/\/|mailto:|#)/i.test(href)) return;
    if (!references.has(normalizeReference(label))) {
      references.set(normalizeReference(label), href);
    }
    remaining[index] = "";
  });
  return { lines: remaining, references };
}

function extractFootnoteDefinitions(
  lines: string[],
): { lines: string[]; items: Array<{ label: string; text: string }> } {
  const items: Array<{ label: string; text: string }> = [];
  const remaining = [...lines];
  let index = 0;
  while (index < remaining.length) {
    const match = /^ {0,3}\[\^([^\]\n]+)\]:[ \t]*(.*)$/.exec(remaining[index] ?? "");
    if (!match) {
      index += 1;
      continue;
    }
    const label = match[1] ?? "";
    const content = [match[2] ?? ""];
    remaining[index] = "";
    let next = index + 1;
    while (next < remaining.length) {
      const line = remaining[next] ?? "";
      if (isBlank(line)) {
        const following = remaining[next + 1] ?? "";
        if (indentOf(following) < 2) break;
        content.push("");
        remaining[next] = "";
        next += 1;
        continue;
      }
      if (indentOf(line) < 2) break;
      content.push(line.slice(2));
      remaining[next] = "";
      next += 1;
    }
    items.push({ label, text: content.join("\n").trim() });
    index = next;
  }
  return { lines: remaining, items };
}
