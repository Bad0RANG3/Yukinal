import { useId, useRef, useState, type ReactNode, type KeyboardEvent } from "react";
import { adjustView3D, DEFAULT_VIEW_3D, flowLayout, numericDomain, scaled, sceneProjector, VISUAL_COLORS, type ChartDocument, type FlowDocument, type PlotDocument, type SceneDocument, type VisualColor, type VisualDocument } from "../lib/visual-document.js";

const format = (value: number) => new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 3, notation: Math.abs(value) >= 1e6 ? "compact" : "standard" }).format(value);
const short = (value: string, length = 22) => value.length > length ? `${value.slice(0, length)}…` : value;
const palette = (color: VisualColor | undefined, index: number) => color ?? VISUAL_COLORS[index % VISUAL_COLORS.length]!;
const kindNames = { chart: "数据图表", flow: "流程图", plot: "坐标图", scene3d: "交互 3D" };

/** Every primitive is created here; model data cannot create tags, styles, URLs or event handlers. */
export function ConversationVisual({ document }: { document: VisualDocument }) {
  return <figure className="conversation-visual" aria-label={document.title}>
    <figcaption><strong>{document.title}</strong><span>{kindNames[document.kind]}</span></figcaption>
    {document.description ? <p className="visual-description">{document.description}</p> : null}
    {document.kind === "chart" ? <Chart document={document} /> : document.kind === "plot" ? <Plot document={document} /> : document.kind === "flow" ? <Flow document={document} /> : <Scene document={document} />}
    <details className="visual-data"><summary>查看数据与文字说明</summary><VisualData document={document} /></details>
  </figure>;
}

function Legend({ entries }: { entries: { label: string; color?: VisualColor }[] }) {
  return <ul className="visual-legend">{entries.map((entry, index) => <li key={index} data-color={palette(entry.color, index)}><i aria-hidden="true" /><span>{entry.label}</span></li>)}</ul>;
}

function Grid({ domain, unit = "" }: { domain: [number, number]; unit?: string }) {
  return <g className="visual-grid">{[0, 1, 2, 3, 4].map((tick) => {
    const y = 270 - tick * 58;
    const value = domain[0] + (domain[1] - domain[0]) * tick / 4;
    return <g key={tick}><line x1="58" y1={y} x2="612" y2={y} /><text x="50" y={y + 4} textAnchor="end">{format(value)}{unit}</text></g>;
  })}</g>;
}

function Chart({ document }: { document: ChartDocument }) {
  const domain = numericDomain(document.series.flatMap((series) => series.values), document.style === "bar");
  const count = document.labels.length;
  const step = 540 / count;
  const x = (index: number) => document.style === "bar" ? 66 + (index + .5) * step : scaled(index, [0, Math.max(1, count - 1)], 66, 606);
  const y = (value: number) => scaled(value, domain, 270, 38);
  const zero = y(0);
  return <><svg className="visual-canvas" viewBox="0 0 640 314" role="img" aria-label={`${document.title}，${document.series.length} 个系列，${count} 个类别。下方可展开完整数据。`}>
    <title>{document.title}</title><Grid domain={domain} unit={document.unit} />
    {document.series.map((series, seriesIndex) => <g key={seriesIndex} data-color={palette(series.color, seriesIndex)} className="visual-series">
      {document.style === "line" ? <>
        <polyline points={series.values.map((value, index) => `${x(index)},${y(value)}`).join(" ")} />
        {series.values.map((value, index) => <circle key={index} cx={x(index)} cy={y(value)} r={count > 50 ? 1.8 : 3}><title>{`${document.labels[index]} · ${series.label}：${value}${document.unit ?? ""}`}</title></circle>)}
      </> : series.values.map((value, index) => {
        const width = Math.max(.3, step * .76 / document.series.length);
        return <rect key={index} x={x(index) - step * .38 + width * seriesIndex} y={Math.min(zero, y(value))} width={width * .9} height={Math.max(.5, Math.abs(y(value) - zero))}><title>{`${document.labels[index]} · ${series.label}：${value}${document.unit ?? ""}`}</title></rect>;
      })}
    </g>)}
    <g className="visual-axis-labels">{document.labels.map((label, index) => index % Math.max(1, Math.ceil(count / 7)) === 0 || index === count - 1 ? <text key={index} x={x(index)} y="294" textAnchor="middle"><title>{label}</title>{short(label, 8)}</text> : null)}</g>
  </svg><Legend entries={document.series} /></>;
}

function Plot({ document }: { document: PlotDocument }) {
  const points = document.series.flatMap((series) => series.points);
  const xDomain = numericDomain(points.map(([x]) => x));
  const yDomain = numericDomain(points.map(([, y]) => y));
  const x = (value: number) => scaled(value, xDomain, 66, 606);
  const y = (value: number) => scaled(value, yDomain, 270, 38);
  return <><svg className="visual-canvas" viewBox="0 0 640 330" role="img" aria-label={`${document.title}，坐标图。下方可展开全部坐标。`}>
    <title>{document.title}</title><Grid domain={yDomain} />
    <g className="visual-grid">{[0, 1, 2, 3, 4].map((tick) => { const value = xDomain[0] + tick * (xDomain[1] - xDomain[0]) / 4; return <g key={tick}><line x1={x(value)} x2={x(value)} y1="38" y2="270" /><text x={x(value)} y="292" textAnchor="middle">{format(value)}</text></g>; })}</g>
    <g className="visual-zero-axes">
      {xDomain[0] <= 0 && xDomain[1] >= 0 ? <line x1={x(0)} x2={x(0)} y1="38" y2="270" /> : null}
      {yDomain[0] <= 0 && yDomain[1] >= 0 ? <line x1="66" x2="606" y1={y(0)} y2={y(0)} /> : null}
    </g>
    {document.series.map((series, index) => <g className="visual-series" key={index} data-color={palette(series.color, index)}>
      {series.mode === "line" ? <polyline points={series.points.map(([px, py]) => `${x(px)},${y(py)}`).join(" ")} /> : null}
      {series.points.map(([px, py], pointIndex) => <circle key={pointIndex} cx={x(px)} cy={y(py)} r="2.7"><title>{`${series.label} (${px}, ${py})`}</title></circle>)}
    </g>)}
    <text className="visual-axis-label" x="340" y="320" textAnchor="middle">{document.xLabel ?? "X"}</text><text className="visual-axis-label" x="20" y="22">{document.yLabel ?? "Y"}</text>
  </svg><Legend entries={document.series} /></>;
}

function Flow({ document }: { document: FlowDocument }) {
  const markerId = useId().replace(/:/g, "");
  const { positions, width, height, adjusted } = flowLayout(document.nodes);
  return <>{adjusted ? <p className="visual-interaction-hint">已调整节点间距，可滚动查看完整图形。</p> : null}<div className="visual-flow-scroll"><svg className="visual-flow-canvas" style={{ minWidth: width }} viewBox={`0 0 ${width} ${height}`} role="img" aria-label={`${document.title}，${document.nodes.length} 个节点，${document.edges.length} 条连接。下方可展开完整关系。`}>
    <title>{document.title}</title><defs><marker id={markerId} viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse"><path d="M 0 0 L 10 5 L 0 10 z" className="visual-arrow" /></marker></defs>
    {document.edges.map((edge, index) => {
      const a = positions.get(edge.from)!, b = positions.get(edge.to)!;
      const dx = b.x - a.x, dy = b.y - a.y;
      const length = Math.hypot(dx, dy) || 1;
      const offset = Math.min(1, 1 / Math.max(Math.abs(dx) / 89, Math.abs(dy) / 27 || .001));
      const start = { x: a.x + dx * offset, y: a.y + dy * offset };
      const end = { x: b.x - dx * offset, y: b.y - dy * offset };
      const labelY = Math.abs(dy) < 52 ? Math.min(a.y, b.y) - 36 : (a.y + b.y) / 2 - 7;
      return <g key={index} className="visual-edge"><line x1={start.x} y1={start.y} x2={end.x - dx / length * 3} y2={end.y - dy / length * 3} markerEnd={`url(#${markerId})`} /><title>{`${edge.from} → ${edge.to}${edge.label ? `：${edge.label}` : ""}`}</title>{edge.label ? <text x={(a.x + b.x) / 2} y={labelY} textAnchor="middle">{short(edge.label, 16)}</text> : null}</g>;
    })}
    {document.nodes.map((node, index) => { const point = positions.get(node.id)!; const label = Array.from(short(node.label, 27)); const lines = label.length > 14 ? [label.slice(0, 14).join(""), label.slice(14).join("")] : [node.label]; return <g key={node.id} data-color={palette(node.color, index)} className="visual-node"><rect x={point.x - 88} y={point.y - 26} width="176" height="52" rx="8" /><text x={point.x} y={point.y + (lines.length === 1 ? 4 : -4)} textAnchor="middle"><title>{node.label}</title>{lines.map((line, lineIndex) => <tspan key={lineIndex} x={point.x} dy={lineIndex ? 16 : 0}>{line}</tspan>)}</text></g>; })}
  </svg></div></>;
}

function Scene({ document }: { document: SceneDocument }) {
  const [view, setView] = useState(DEFAULT_VIEW_3D);
  const [showConnections, setShowConnections] = useState(false);
  const drag = useRef<{ id: number; x: number; y: number } | null>(null);
  const project = sceneProjector(document.nodes, view);
  const ordered = document.nodes.map((node) => ({ node, point: project(node.position) })).sort((a, b) => a.point.depth - b.point.depth);
  const positions = new Map(ordered.map(({ node, point }) => [node.id, point]));
  const keyDown = (event: KeyboardEvent<SVGSVGElement>) => {
    const angle = event.shiftKey ? .3 : .12;
    if (["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "+", "=", "-", "Home", "0"].includes(event.key)) event.preventDefault();
    else return;
    setView((current) => event.key === "Home" || event.key === "0" ? DEFAULT_VIEW_3D : adjustView3D(current, event.key === "ArrowLeft" ? -angle : event.key === "ArrowRight" ? angle : 0, event.key === "ArrowUp" ? angle : event.key === "ArrowDown" ? -angle : 0, current.zoom + (["+", "="].includes(event.key) ? .15 : event.key === "-" ? -.15 : 0)));
  };
  return <>
    <p className="visual-interaction-hint">拖动旋转 · 方向键调整 · + / − 缩放 · Home 复位</p>
    <svg className="visual-canvas visual-scene" viewBox="0 0 640 370" role="group" aria-label={`${document.title}，可旋转的三维信息场景`} tabIndex={0}
      onKeyDown={keyDown}
      onPointerDown={(event) => { if (event.button !== 0) return; event.currentTarget.setPointerCapture(event.pointerId); drag.current = { id: event.pointerId, x: event.clientX, y: event.clientY }; }}
      onPointerMove={(event) => { const previous = drag.current; if (!previous || previous.id !== event.pointerId) return; setView((current) => adjustView3D(current, (event.clientX - previous.x) * .008, (event.clientY - previous.y) * .008)); drag.current = { id: event.pointerId, x: event.clientX, y: event.clientY }; }}
      onPointerUp={(event) => { drag.current = null; if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId); }}
      onPointerCancel={() => { drag.current = null; }} onLostPointerCapture={() => { drag.current = null; }}>
      <title>{document.title}</title>
      <g className="visual-scene-axes">{([[1, 0, 0], [0, 1, 0], [0, 0, 1]] as const).map((axis, index) => {
        // A fixed orientation guide stays readable regardless of source coordinate scale.
        const end = sceneProjector([{ id: "axis", label: "axis", position: [0, 0, 0], size: 1, shape: "sphere" }], { ...view, zoom: 1 })(axis);
        return <g key={index}><line x1="45" y1="327" x2={45 + (end.x - 320) * .3} y2={327 + (end.y - 185) * .3} /><text x={45 + (end.x - 320) * .4} y={327 + (end.y - 185) * .4}>{["X", "Y", "Z"][index]}</text></g>;
      })}</g>
      {document.edges.map((edge, index) => { const a = positions.get(edge.from)!, b = positions.get(edge.to)!; return <g className="visual-scene-edge" key={index}><line x1={a.x} y1={a.y} x2={b.x} y2={b.y} /><title>{`${edge.from} → ${edge.to}${edge.label ? `：${edge.label}` : ""}`}</title>{showConnections && edge.label ? <text x={(a.x + b.x) / 2} y={(a.y + b.y) / 2 - 5} textAnchor="middle">{short(edge.label, 18)}</text> : null}</g>; })}
      {ordered.map(({ node, point }) => <g className="visual-scene-node" data-color={palette(node.color, document.nodes.indexOf(node))} key={node.id}>
        {node.shape === "box" ? <Box node={node} project={project} /> : <circle cx={point.x} cy={point.y} r={Math.max(5, Math.min(25, node.size * 11)) * view.zoom} />}
        <title>{`${node.label} (${node.position.join(", ")})`}</title>
      </g>)}
      <g className="visual-scene-labels">{ordered.map(({ node, point }) => <text key={node.id} x={point.x} y={point.y - Math.max(18, Math.min(32, node.size * 14)) * view.zoom} textAnchor="middle"><title>{node.label}</title>{short(node.label, 24)}</text>)}</g>
    </svg>
    <div className="visual-controls" role="group" aria-label="三维视角">
      <button type="button" aria-label="缩小三维场景" disabled={view.zoom <= .45} onClick={() => setView((current) => adjustView3D(current, 0, 0, current.zoom - .2))}>−</button>
      <label>缩放 <input type="range" min="45" max="250" step="5" value={Math.round(view.zoom * 100)} onChange={(event) => setView((current) => adjustView3D(current, 0, 0, Number(event.currentTarget.value) / 100))} /></label>
      <button type="button" aria-label="放大三维场景" disabled={view.zoom >= 2.5} onClick={() => setView((current) => adjustView3D(current, 0, 0, current.zoom + .2))}>+</button>
      <button type="button" onClick={() => setView(DEFAULT_VIEW_3D)}>复位视角</button>
      {document.edges.some((edge) => edge.label) ? <button type="button" aria-pressed={showConnections} onClick={() => setShowConnections((current) => !current)}>连接说明</button> : null}
    </div>
  </>;
}

function Box({ node, project }: { node: SceneDocument["nodes"][number]; project: ReturnType<typeof sceneProjector> }) {
  const vertices = [[-1,-1,-1],[1,-1,-1],[1,1,-1],[-1,1,-1],[-1,-1,1],[1,-1,1],[1,1,1],[-1,1,1]].map((offset) => project(node.position.map((value, axis) => value + offset[axis]! * node.size / 2)));
  const faces = [[0,1,2,3],[4,5,6,7],[0,1,5,4],[2,3,7,6],[0,3,7,4],[1,2,6,5]].map((indices) => ({ indices, depth: indices.reduce((sum, index) => sum + vertices[index]!.depth, 0) / 4 })).sort((a, b) => a.depth - b.depth);
  return <g className="visual-scene-box">{faces.map(({ indices }, index) => <polygon key={index} points={indices.map((vertex) => `${vertices[vertex]!.x},${vertices[vertex]!.y}`).join(" ")} />)}</g>;
}

function DataTable({ headers, children }: { headers: string[]; children: ReactNode }) {
  return <div className="visual-data-scroll"><table><thead><tr>{headers.map((header, index) => <th key={index} scope="col">{header}</th>)}</tr></thead><tbody>{children}</tbody></table></div>;
}
function VisualData({ document }: { document: VisualDocument }) {
  if (document.kind === "chart") return <DataTable headers={["类别", ...document.series.map((series) => `${series.label}${document.unit ? ` (${document.unit})` : ""}`)]}>{document.labels.map((label, index) => <tr key={index}><th scope="row">{label}</th>{document.series.map((series, seriesIndex) => <td key={seriesIndex}>{series.values[index]}</td>)}</tr>)}</DataTable>;
  if (document.kind === "plot") return <DataTable headers={["系列", document.xLabel ?? "X", document.yLabel ?? "Y"]}>{document.series.flatMap((series, index) => series.points.map(([x, y], point) => <tr key={`${index}:${point}`}><th scope="row">{series.label}</th><td>{x}</td><td>{y}</td></tr>))}</DataTable>;
  return <><DataTable headers={document.kind === "scene3d" ? ["节点", "X", "Y", "Z", "形状", "大小"] : ["节点", "标识"]}>{document.nodes.map((node) => <tr key={node.id}><th scope="row">{node.label}</th>{"position" in node ? <>{node.position.map((value, index) => <td key={index}>{value}</td>)}<td>{node.shape === "box" ? "方体" : "球体"}</td><td>{node.size}</td></> : <td>{node.id}</td>}</tr>)}</DataTable>
    {document.edges.length > 0 ? <ul className="visual-relations">{document.edges.map((edge, index) => <li key={index}>{document.nodes.find((node) => node.id === edge.from)!.label} → {document.nodes.find((node) => node.id === edge.to)!.label}{edge.label ? `：${edge.label}` : ""}</li>)}</ul> : <p>未定义连接关系。</p>}
  </>;
}
