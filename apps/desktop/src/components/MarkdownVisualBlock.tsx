import { memo, useMemo } from "react";
import { CopyButton } from "./CopyButton.js";
import { ConversationVisual } from "./ConversationVisual.js";
import { MAX_VISUAL_SOURCE, parseVisualDocument } from "../lib/visual-document.js";

export const MarkdownVisualBlock = memo(function MarkdownVisualBlock({ source, streaming = false }: { source: string; streaming?: boolean }) {
  const parsed = useMemo(() => parseVisualDocument(source), [source]);
  if (parsed.status === "ready") return <ConversationVisual document={parsed.document} />;
  const pending = streaming && parsed.reason !== "size";
  return <section className="conversation-visual visual-fallback" aria-label="AI 可视化" aria-busy={pending || undefined}>
    <p role="status">{pending ? "正在准备可视化…" : parsed.reason === "size" ? "绘图内容超出 64 KB 限制，请拆分为多个图。" : "绘图数据不符合格式，原始内容已保留。"}</p>
    <details><summary>查看绘图数据</summary>
      {source.length <= MAX_VISUAL_SOURCE ? <CopyButton text={source} label="复制绘图数据" /> : null}
      <pre><code>{source.slice(0, MAX_VISUAL_SOURCE)}{source.length > MAX_VISUAL_SOURCE ? "\n…内容已截断" : ""}</code></pre>
    </details>
  </section>;
});
