import { memo } from "react";
import { ConversationWidgets } from "./ConversationWidgets.js";
import { parseConversationWidgets, parseStreamingConversationWidgets } from "../lib/conversation-widgets.js";
import { CopyButton } from "./CopyButton.js";

/** Unfinished structured output stays readable without pretending it is a valid card. */
export const MarkdownWidgetBlock = memo(function MarkdownWidgetBlock({ source, streaming = false }: { source: string; streaming?: boolean }) {
  const parsed = parseConversationWidgets(source);
  if (parsed.status === "ready") return <div className="agent-widget-block"><ConversationWidgets document={parsed.document} streaming={streaming} /></div>;
  const partial = streaming && parsed.reason === "json" ? parseStreamingConversationWidgets(source) : null;
  if (partial) return <div className="agent-widget-block"><ConversationWidgets document={partial} streaming /><p className="conversation-widget-note" role="status">正在生成其余内容…</p></div>;
  const pending = streaming && parsed.reason === "json";
  const original = <div className="agent-widget-source"><CopyButton text={source} label="复制原始内容" /><pre><code>{source}</code></pre></div>;
  return <div className={`agent-widget-fallback${pending ? " is-pending" : ""}`}>
    <p role={pending ? "status" : undefined}>{pending ? "正在生成展示内容…" : "展示内容未能解析，原文已保留。"}</p>
    {pending ? <details><summary>查看已生成内容</summary>{original}</details> : original}
  </div>;
});
