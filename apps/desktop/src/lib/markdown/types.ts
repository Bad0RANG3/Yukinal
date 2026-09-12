/**
 * The shapes `lib/markdown` produces.
 *
 * Types only — no parsing lives here, so the block parser and the inline parser can both
 * depend on this module without depending on each other.
 */

/** 表格列的对齐方式；`null` 表示没写。 */
export type ColumnAlign = "left" | "center" | "right" | null;

export type Inline =
  | { kind: "text"; text: string }
  | { kind: "code"; text: string }
  | { kind: "strong"; children: Inline[] }
  | { kind: "emphasis"; children: Inline[] }
  | { kind: "strike"; children: Inline[] }
  | { kind: "link"; href: string; children: Inline[] }
  | { kind: "image"; href: string; alt: string }
  /** 行尾两个空格或一个反斜杠 —— 段落内唯一的强制换行。 */
  | { kind: "break" };

export type ListItem = {
  /** 任务列表的勾选态；不是任务项时为 `null`。 */
  checked: boolean | null;
  blocks: Block[];
};

export type Block =
  | { kind: "heading"; level: number; content: Inline[] }
  | { kind: "paragraph"; content: Inline[] }
  | { kind: "code"; language: string | null; text: string }
  | { kind: "list"; ordered: boolean; start: number; items: ListItem[] }
  | { kind: "quote"; blocks: Block[] }
  | { kind: "table"; align: ColumnAlign[]; head: Inline[][]; rows: Inline[][][] }
  | { kind: "rule" };
