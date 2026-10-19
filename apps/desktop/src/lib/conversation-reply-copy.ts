import { CONVERSATION_WIDGET_LANGUAGE, parseConversationWidgets, type ConversationWidgetDocument } from "./conversation-widgets.js";

export function conversationWidgetText(document: ConversationWidgetDocument): string {
  const states = { pending: "待处理", active: "进行中", done: "已完成", error: "失败" };
  return document.widgets.map((widget) => {
    switch (widget.type) {
      case "card": case "details": return `${widget.title}\n${widget.body}`;
      case "steps": return `${widget.title}\n${widget.items.map((item) => `[${states[item.status]}] ${item.title}${item.detail ? `\n  ${item.detail}` : ""}`).join("\n")}`;
      case "metrics": return `${widget.title}\n${widget.items.map((item) => `${item.label}：${item.value}${item.unit ? ` ${item.unit}` : ""}${item.trend?.length ? `\n  趋势：${item.trend.join(" → ")}` : ""}`).join("\n")}`;
      case "progress": return `${widget.title}：${widget.value} / ${widget.max}${widget.detail ? `\n${widget.detail}` : ""}`;
    }
  }).join("\n\n");
}

/** Keep ordinary Markdown/code byte-for-byte; only validated display fences become readable text. */
export function conversationReplyCopyText(source: string): string {
  const lines = source.match(/[^\r\n]*(?:\r\n|\r|\n|$)/g) ?? [];
  const content = (line: string) => line.replace(/(?:\r\n|\r|\n)$/, "");
  const result: string[] = [];
  for (let index = 0; index < lines.length;) {
    const opening = /^ {0,3}(`{3,}|~{3,})(.*)$/.exec(content(lines[index]!));
    if (!opening || (opening[1]![0] === "`" && opening[2]!.includes("`"))) { result.push(lines[index++]!); continue; }
    const close = new RegExp(`^ {0,3}${opening[1]![0]}{${opening[1]!.length},} *$`);
    let end = index + 1;
    while (end < lines.length && !close.test(content(lines[end]!))) end++;
    const after = end < lines.length ? end + 1 : end;
    const parsed = opening[2]!.trim().split(/\s+/)[0]?.toLowerCase() === CONVERSATION_WIDGET_LANGUAGE
      ? parseConversationWidgets(lines.slice(index + 1, end).join("")) : null;
    if (parsed?.status === "ready") {
      const ending = /(?:\r\n|\r|\n)$/.exec(lines[Math.max(index, after - 1)]!)?.[0] ?? "";
      result.push(conversationWidgetText(parsed.document) + ending);
    } else result.push(lines.slice(index, after).join(""));
    index = after;
  }
  return result.join("");
}
