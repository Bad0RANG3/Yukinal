import { useId, useState } from "react";
import { MarkdownText } from "../../components/MarkdownText.js";
import { Icon } from "../../components/Icon.js";
import { isNativeDesktopShell } from "../../lib/ipc.js";
import { sandboxedArtifactDocument, type ArtifactContent, type WorkpageArtifact } from "./workpage-model.js";

export function WorkpageArtifactView({ artifact }: { artifact: WorkpageArtifact }) {
  const content = artifact.content;
  if (content.format === "markdown") return <MarkdownText className="workpage-markdown" text={content.text} />;
  if (content.format === "chart") return <ArtifactChart content={content} title={artifact.title} />;
  if (content.format === "flow") return <ArtifactFlow content={content} title={artifact.title} />;
  if (content.format === "html") return <InteractiveArtifact key={artifact.id} html={content.html} title={artifact.title} />;
  return <pre className="workpage-json"><code>{content.text}</code></pre>;
}

function InteractiveArtifact({ html, title }: { html: string; title: string }) {
  const [running, setRunning] = useState(false);
  const [unavailable, setUnavailable] = useState(false);
  const toggle = () => {
    // webview_bind can be installed in child frames. Do not run untrusted scripts
    // until the host can enforce frame provenance on its raw native binding.
    if (isNativeDesktopShell()) { setUnavailable(true); return; }
    setRunning((value) => !value);
  };
  return <div className="workpage-interactive">
    <div className="workpage-artifact-toolbar"><span>交互页面</span><span className="workpage-isolation-label">隔离预览</span><button type="button" className="button-secondary" onClick={toggle}><Icon name={running ? "stop" : "connect"} size="sm" />{running ? "停止" : "运行"}</button></div>
    {unavailable ? <div className="workpage-inline-error" role="status">桌面交互隔离尚未就绪</div> : null}
    {running ? <iframe title={title} sandbox="allow-scripts" referrerPolicy="no-referrer" srcDoc={sandboxedArtifactDocument(html)} /> : <pre className="workpage-json"><code>{html}</code></pre>}
  </div>;
}

function ArtifactChart({ content, title }: { content: Extract<ArtifactContent, { format: "chart" }>; title: string }) {
  const values = content.series.map((item) => item.value);
  const min = Math.min(0, ...values), max = Math.max(0, ...values);
  const span = max - min || 1;
  const zero = 140 + (-min / span) * 360;
  const height = Math.max(120, content.series.length * 34 + 28);
  return <figure className="workpage-chart"><svg role="img" aria-label={title} viewBox={`0 0 600 ${height}`} style={{ minHeight: height }}>
    <line x1={zero} x2={zero} y1={8} y2={height - 6} className="workpage-chart-axis" />
    {content.series.map((item, index) => {
      const valueX = 140 + ((item.value - min) / span) * 360, y = index * 34 + 14;
      return <g key={index}><text x={130} y={y + 16} textAnchor="end" className="workpage-chart-label">{shortLabel(item.label, 17)}</text><rect x={Math.min(zero, valueX)} y={y} width={Math.max(1, Math.abs(valueX - zero))} height={22} rx={3} className="workpage-chart-bar"><title>{`${item.label}: ${item.value}${content.unit ?? ""}`}</title></rect><text x={510} y={y + 16} className="workpage-chart-value">{item.value}{content.unit ?? ""}</text></g>;
    })}
  </svg><figcaption>{title}</figcaption></figure>;
}

function ArtifactFlow({ content, title }: { content: Extract<ArtifactContent, { format: "flow" }>; title: string }) {
  const markerId = `flow-arrow-${useId().replace(/:/g, "")}`;
  const columns = content.nodes.length < 4 ? 1 : 2;
  const width = columns === 1 ? 440 : 680;
  const positions = new Map(content.nodes.map((node, index) => [node.id, { x: columns === 1 ? 100 : 45 + (index % columns) * 350, y: 24 + Math.floor(index / columns) * 110 }]));
  const height = Math.ceil(content.nodes.length / columns) * 110 + 20;
  return <figure className="workpage-flow"><svg role="img" aria-label={title} viewBox={`0 0 ${width} ${height}`} style={{ minHeight: Math.min(height, 600) }}>
    <defs><marker id={markerId} markerWidth={8} markerHeight={8} refX={7} refY={4} orient="auto"><path d="M0 0 8 4 0 8" fill="currentColor" /></marker></defs>
    {content.edges.map((edge, index) => {
      const from = positions.get(edge.from), to = positions.get(edge.to);
      if (!from || !to) return null;
      const sameRow = from.y === to.y;
      const x1 = from.x + (sameRow ? 240 : 120), y1 = from.y + (sameRow ? 28 : 56);
      const x2 = to.x + (sameRow ? 0 : 120), y2 = to.y + (sameRow ? 28 : 0);
      const middle = (y1 + y2) / 2;
      return <g key={index}><path className="workpage-flow-edge" markerEnd={`url(#${markerId})`} d={sameRow ? `M${x1} ${y1} L${x2 - 5} ${y2}` : `M${x1} ${y1} C${x1} ${middle},${x2} ${middle},${x2} ${y2 - 5}`} />{edge.label ? <text x={(x1 + x2) / 2 + 5} y={(y1 + y2) / 2 - 6} className="workpage-flow-edge-label">{shortLabel(edge.label, 20)}</text> : null}</g>;
    })}
    {content.nodes.map((node) => {
      const position = positions.get(node.id)!;
      return <g key={node.id}><rect x={position.x} y={position.y} width={240} height={56} rx={8} className="workpage-flow-node" /><text x={position.x + 120} y={position.y + 33} textAnchor="middle" className="workpage-flow-label">{shortLabel(node.label, 25)}<title>{node.label}</title></text></g>;
    })}
  </svg><figcaption>{title}</figcaption></figure>;
}

function shortLabel(text: string, length: number): string { return text.length > length ? `${text.slice(0, length - 1)}…` : text; }
