/** Local, display-only visualization protocol. Never evaluate model output. */
export const VISUAL_LANGUAGE = "yukinal-visual";
export const MAX_VISUAL_SOURCE = 65_536;
export const VISUAL_COLORS = ["blue", "green", "amber", "purple", "red", "cyan"] as const;
export type VisualColor = typeof VISUAL_COLORS[number];
type Base = { version: 1; title: string; description?: string };
export type ChartDocument = Base & { kind: "chart"; style: "line" | "bar"; labels: string[]; series: { label: string; values: number[]; color?: VisualColor }[]; unit?: string };
export type PlotDocument = Base & { kind: "plot"; series: { label: string; points: [number, number][]; mode: "line" | "points"; color?: VisualColor }[]; xLabel?: string; yLabel?: string };
export type FlowDocument = Base & { kind: "flow"; nodes: { id: string; label: string; x?: number; y?: number; color?: VisualColor }[]; edges: { from: string; to: string; label?: string }[] };
export type SceneDocument = Base & { kind: "scene3d"; nodes: { id: string; label: string; position: [number, number, number]; shape: "sphere" | "box"; size: number; color?: VisualColor }[]; edges: { from: string; to: string; label?: string }[] };
export type VisualDocument = ChartDocument | PlotDocument | FlowDocument | SceneDocument;
export type VisualParse = { status: "ready"; document: VisualDocument } | { status: "invalid"; reason: "json" | "shape" | "size" };
type ObjectValue = Record<string, unknown>;
const object = (value: unknown): value is ObjectValue => value !== null && typeof value === "object" && !Array.isArray(value);
const keys = (value: ObjectValue, allowed: string[]) => Object.keys(value).every((key) => allowed.includes(key));
const text = (value: unknown, max = 160): value is string => typeof value === "string" && value.length > 0 && value.length <= max && value.trim().length > 0;
const optionalText = (value: unknown, max = 160) => value === undefined || text(value, max);
const number = (value: unknown): value is number => typeof value === "number" && Number.isFinite(value) && Math.abs(value) <= 1e9;
const list = (value: unknown, max: number): value is unknown[] => Array.isArray(value) && value.length > 0 && value.length <= max;
const color = (value: unknown) => value === undefined || VISUAL_COLORS.some((entry) => entry === value);
const tuple = (value: unknown, length: number) => Array.isArray(value) && value.length === length && value.every(number);
const common = ["version", "kind", "title", "description"];

function parseEdges(value: unknown, ids: Set<string>, max: number): FlowDocument["edges"] | null {
  if (!Array.isArray(value) || value.length > max) return null;
  const edges: FlowDocument["edges"] = [];
  for (const item of value) {
    if (!object(item) || !keys(item, ["from", "to", "label"]) || !text(item.from) || !text(item.to) || !ids.has(item.from) || !ids.has(item.to) || item.from === item.to || !optionalText(item.label, 80)) return null;
    edges.push({ from: item.from, to: item.to, ...(item.label === undefined ? {} : { label: item.label as string }) });
  }
  return edges;
}

/** Reject invalid documents as a whole; silently deleting points could change their meaning. */
export function parseVisualDocument(source: string): VisualParse {
  if (source.length > MAX_VISUAL_SOURCE) return { status: "invalid", reason: "size" };
  let value: unknown;
  try { value = JSON.parse(source); } catch { return { status: "invalid", reason: "json" }; }
  const bad: VisualParse = { status: "invalid", reason: "shape" };
  if (!object(value) || value.version !== 1 || !text(value.title) || !optionalText(value.description, 2_000)) return bad;
  const base: Base = { version: 1, title: value.title, ...(value.description === undefined ? {} : { description: value.description as string }) };
  switch (value.kind) {
    case "chart": {
      if (!keys(value, [...common, "style", "labels", "series", "unit"]) || !(value.style === undefined || value.style === "line" || value.style === "bar") || !optionalText(value.unit, 32) || !list(value.labels, 120) || !value.labels.every((entry) => text(entry, 80)) || !list(value.series, 6)) return bad;
      const series: ChartDocument["series"] = [];
      for (const item of value.series) {
        if (!object(item) || !keys(item, ["label", "values", "color"]) || !text(item.label) || !color(item.color) || !Array.isArray(item.values) || item.values.length !== value.labels.length || !item.values.every(number)) return bad;
        series.push({ label: item.label, values: [...item.values as number[]], ...(item.color === undefined ? {} : { color: item.color as VisualColor }) });
      }
      return { status: "ready", document: { ...base, kind: "chart", style: value.style ?? "line", labels: [...value.labels as string[]], series, ...(value.unit === undefined ? {} : { unit: value.unit as string }) } };
    }
    case "plot": {
      if (!keys(value, [...common, "series", "xLabel", "yLabel"]) || !list(value.series, 6) || !optionalText(value.xLabel, 80) || !optionalText(value.yLabel, 80)) return bad;
      const series: PlotDocument["series"] = [];
      for (const item of value.series) {
        if (!object(item) || !keys(item, ["label", "points", "mode", "color"]) || !text(item.label) || !color(item.color) || !(item.mode === undefined || item.mode === "line" || item.mode === "points") || !list(item.points, 256) || !item.points.every((point) => tuple(point, 2))) return bad;
        series.push({ label: item.label, points: (item.points as [number, number][]).map(([x, y]) => [x, y]), mode: item.mode ?? "line", ...(item.color === undefined ? {} : { color: item.color as VisualColor }) });
      }
      return { status: "ready", document: { ...base, kind: "plot", series, ...(value.xLabel === undefined ? {} : { xLabel: value.xLabel as string }), ...(value.yLabel === undefined ? {} : { yLabel: value.yLabel as string }) } };
    }
    case "flow": {
      if (!keys(value, [...common, "nodes", "edges"]) || !list(value.nodes, 24)) return bad;
      const nodes: FlowDocument["nodes"] = [];
      const ids = new Set<string>();
      for (const item of value.nodes) {
        if (!object(item) || !keys(item, ["id", "label", "x", "y", "color"]) || !text(item.id, 80) || ids.has(item.id) || !text(item.label, 80) || !color(item.color)) return bad;
        if ((item.x === undefined) !== (item.y === undefined) || (item.x !== undefined && (!number(item.x) || !number(item.y) || item.x < 0 || item.x > 100 || item.y < 0 || item.y > 100))) return bad;
        ids.add(item.id);
        nodes.push({ id: item.id, label: item.label, ...(item.x === undefined ? {} : { x: item.x as number, y: item.y as number }), ...(item.color === undefined ? {} : { color: item.color as VisualColor }) });
      }
      const edges = parseEdges(value.edges, ids, 64);
      return edges ? { status: "ready", document: { ...base, kind: "flow", nodes, edges } } : bad;
    }
    case "scene3d": {
      if (!keys(value, [...common, "nodes", "edges"]) || !list(value.nodes, 48)) return bad;
      const nodes: SceneDocument["nodes"] = [];
      const ids = new Set<string>();
      for (const item of value.nodes) {
        if (!object(item) || !keys(item, ["id", "label", "position", "shape", "size", "color"]) || !text(item.id, 80) || ids.has(item.id) || !text(item.label, 80) || !tuple(item.position, 3) || !color(item.color) || !(item.shape === undefined || item.shape === "sphere" || item.shape === "box") || (item.size !== undefined && (!number(item.size) || item.size < 0.1 || item.size > 10))) return bad;
        ids.add(item.id);
        nodes.push({ id: item.id, label: item.label, position: [...item.position as [number, number, number]], shape: item.shape ?? "sphere", size: item.size === undefined ? 1 : item.size as number, ...(item.color === undefined ? {} : { color: item.color as VisualColor }) });
      }
      const edges = parseEdges(value.edges, ids, 96);
      return edges ? { status: "ready", document: { ...base, kind: "scene3d", nodes, edges } } : bad;
    }
    default: return bad;
  }
}

export function numericDomain(values: readonly number[], includeZero = false): [number, number] {
  let low = includeZero ? 0 : values[0] ?? 0;
  let high = low;
  for (const value of values) { low = Math.min(low, value); high = Math.max(high, value); }
  if (low === high) { const pad = Math.max(1, Math.abs(low) * .05); return [low - pad, high + pad]; }
  return [low, high];
}
export function scaled(value: number, domain: [number, number], start: number, end: number) {
  return start + (value - domain[0]) / (domain[1] - domain[0]) * (end - start);
}

/** Flow coordinates describe layout, so expand spacing without changing graph data. */
export function flowLayout(nodes: FlowDocument["nodes"]) {
  const rows = Math.ceil(nodes.length / 3);
  const grid = (index: number) => ({ x: index % 3 * 224, y: Math.floor(index / 3) * 104 });
  let points = nodes.map((node, index) => node.x === undefined
    ? grid(index) : { x: node.x * 4.6, y: node.y! * Math.max(88, rows * 104 - 48) / 100 });
  let scale = 1;
  for (let index = 0; index < points.length; index++) {
    for (let other = 0; other < index; other++) {
      const a = points[index]!, b = points[other]!;
      const separation = Math.max(Math.abs(a.x - b.x) / 224, Math.abs(a.y - b.y) / 88);
      scale = Math.max(scale, separation ? 1 / separation : Infinity);
    }
  }
  const adjusted = scale > 1;
  // Coincident or extremely dense coordinates use a bounded layout rather than
  // creating an enormous canvas or hiding nodes behind each other.
  if (scale > 4) { points = nodes.map((_node, index) => grid(index)); scale = 1; }
  const minX = Math.min(...points.map((point) => point.x)), minY = Math.min(...points.map((point) => point.y));
  const positions = new Map(nodes.map((node, index) => [node.id, {
    x: (points[index]!.x - minX) * scale + 108,
    y: (points[index]!.y - minY) * scale + 48,
  }]));
  return { positions, adjusted,
    width: Math.max(640, ...Array.from(positions.values(), (point) => point.x + 108)),
    height: Math.max(144, ...Array.from(positions.values(), (point) => point.y + 48)),
  };
}
export type View3D = { yaw: number; pitch: number; zoom: number };
export const DEFAULT_VIEW_3D: View3D = { yaw: -.55, pitch: .35, zoom: 1 };
export function adjustView3D(view: View3D, yaw: number, pitch: number, zoom = view.zoom): View3D {
  return { yaw: (view.yaw + yaw) % (Math.PI * 2), pitch: Math.max(-1.4, Math.min(1.4, view.pitch + pitch)), zoom: Math.max(.45, Math.min(2.5, zoom)) };
}
/** Normalize information coordinates first so even large source values project to finite pixels. */
export function sceneProjector(nodes: SceneDocument["nodes"], view: View3D) {
  const domains = [0, 1, 2].map((axis) => numericDomain(nodes.flatMap((node) => [node.position[axis]! - node.size / 2, node.position[axis]! + node.size / 2])));
  const center = domains.map(([low, high]) => (low + high) / 2);
  const extent = Math.max(4, ...domains.map(([low, high]) => high - low));
  return (position: readonly number[]) => {
    const x = (position[0]! - center[0]!) / extent, y = (position[1]! - center[1]!) / extent, z = (position[2]! - center[2]!) / extent;
    const rx = x * Math.cos(view.yaw) - z * Math.sin(view.yaw);
    const rz = x * Math.sin(view.yaw) + z * Math.cos(view.yaw);
    const ry = y * Math.cos(view.pitch) - rz * Math.sin(view.pitch);
    return { x: 320 + rx * 380 * view.zoom, y: 185 - ry * 280 * view.zoom, depth: y * Math.sin(view.pitch) + rz * Math.cos(view.pitch) };
  };
}
