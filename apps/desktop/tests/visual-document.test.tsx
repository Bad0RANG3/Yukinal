import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";
import { MarkdownText } from "../src/components/MarkdownText.js";
import { ConversationVisual } from "../src/components/ConversationVisual.js";
import { adjustView3D, DEFAULT_VIEW_3D, flowLayout, MAX_VISUAL_SOURCE, numericDomain, parseVisualDocument, scaled, sceneProjector, type VisualDocument } from "../src/lib/visual-document.js";

const chart = { version: 1, kind: "chart", title: "趋势", labels: ["A", "B"], series: [{ label: "CPU", values: [10, 20] }] };
const flow = { version: 1, kind: "flow", title: "流程", nodes: [{ id: "a", label: "读取" }, { id: "b", label: "返回" }], edges: [{ from: "a", to: "b", label: "成功" }] };
const scene = { version: 1, kind: "scene3d", title: "架构", nodes: [{ id: "a", label: "API", position: [0, 0, 0], shape: "box" }, { id: "b", label: "DB", position: [2, 1, -1] }], edges: [{ from: "a", to: "b" }] };
const parse = (value: unknown) => parseVisualDocument(JSON.stringify(value));
const ready = (value: unknown): VisualDocument => { const result = parse(value); assert.equal(result.status, "ready"); if (result.status !== "ready") throw new Error("fixture invalid"); return result.document; };
const markdown = (source: string, streaming = false) => renderToStaticMarkup(<MarkdownText text={`\u0060\u0060\u0060yukinal-visual\n${source}\n\u0060\u0060\u0060`} streaming={streaming} />);

test("all four visual kinds normalize explicit defaults without mutating their source", () => {
  const result = ready(chart); assert.equal(result.kind, "chart"); if (result.kind === "chart") assert.equal(result.style, "line");
  const parsedFlow = ready(flow); assert.equal(parsedFlow.kind, "flow");
  const parsedScene = ready(scene); assert.equal(parsedScene.kind, "scene3d"); if (parsedScene.kind === "scene3d") { assert.equal(parsedScene.nodes[1]!.shape, "sphere"); assert.equal(parsedScene.nodes[1]!.size, 1); }
  assert.equal(ready({ version: 1, kind: "plot", title: "坐标", series: [{ label: "f(x)", points: [[-1, 1], [0, 0], [1, 1]] }] }).kind, "plot");
  assert.equal("style" in chart, false);
});

test("strict chart shape rejects missing labels, ragged values, unknown fields and unsafe styles", () => {
  for (const value of [{ ...chart, labels: [] }, { ...chart, series: [{ label: "CPU", values: [1] }] }, { ...chart, html: "<script/>" }, { ...chart, series: [{ label: "CPU", values: [1, 2], color: "url(https://example.test)" }] }, { ...chart, series: [{ label: "CPU", values: [1, 1e10] }] }, { ...chart, unit: "x".repeat(33) }]) assert.deepEqual(parse(value), { status: "invalid", reason: "shape" });
});

test("size and count limits reject a whole document instead of dropping data", () => {
  assert.deepEqual(parseVisualDocument("x".repeat(MAX_VISUAL_SOURCE + 1)), { status: "invalid", reason: "size" });
  assert.deepEqual(parse({ ...chart, labels: Array(121).fill("x"), series: [{ label: "a", values: Array(121).fill(1) }] }), { status: "invalid", reason: "shape" });
  assert.deepEqual(parse({ version: 1, kind: "plot", title: "plot", series: [{ label: "a", points: Array(257).fill([0, 0]) }] }), { status: "invalid", reason: "shape" });
  assert.equal(parse({ ...scene, nodes: Array.from({ length: 49 }, (_, i) => ({ id: String(i), label: "n", position: [0, 0, 0] })) }).status, "invalid");
});

test("graph references, unique ids, coordinates and primitive limits are checked", () => {
  const invalid = [{ ...flow, edges: [{ from: "a", to: "missing" }] }, { ...flow, edges: [{ from: "a", to: "a" }] }, { ...flow, nodes: [{ id: "a", label: "1" }, { id: "a", label: "2" }] }, { ...flow, nodes: [{ id: "a", label: "1", x: 40 }] }, { ...flow, nodes: [{ id: "a", label: "1", x: -1, y: 0 }] }, { ...scene, nodes: [{ id: "a", label: "a", position: [1, 2], size: 0 }] }, { ...scene, nodes: [{ id: "a", label: "a", position: [1, 2, 3], shape: "iframe" }] }];
  for (const value of invalid) assert.equal(parse(value).status, "invalid");
});

test("incomplete stream stays pending, complete stream renders, final invalid feedback preserves escaped source", () => {
  const pending = markdown('{"version":1,"kind":"chart",', true);
  assert.match(pending, /正在准备可视化/); assert.doesNotMatch(pending, /绘图数据不符合/);
  assert.match(markdown(JSON.stringify(chart), true), /<svg/);
  const invalid = markdown('<script src="https://example.test/a.js"></script>');
  assert.match(invalid, /绘图数据不符合格式/); assert.match(invalid, /&lt;script/); assert.doesNotMatch(invalid, /<script/);
  assert.match(markdown("x".repeat(MAX_VISUAL_SOURCE + 1)), /超出 64 KB/);
});

test("Markdown renders visualization alongside existing tables and ordinary code", () => {
  const text = `普通文字\n\n|列|值|\n|---|---|\n|CPU|20|\n\n\u0060\u0060\u0060yukinal-visual\n${JSON.stringify(chart)}\n\u0060\u0060\u0060\n\n\u0060\u0060\u0060sh\nprintf hello\n\u0060\u0060\u0060`;
  const html = renderToStaticMarkup(<MarkdownText text={text} />);
  assert.match(html, /md-table/); assert.match(html, /visual-canvas/); assert.match(html, /md-code-block/); assert.match(html, /查看数据与文字说明/);
});

test("all renderers include accessible source data and never create resource-bearing model elements", () => {
  for (const value of [chart, flow, scene, { version: 1, kind: "plot", title: "plot", series: [{ label: "series", points: [[0, 0]] }] }]) {
    const html = renderToStaticMarkup(<ConversationVisual document={ready(value)} />);
    assert.match(html, /<svg/); assert.match(html, /<table/); assert.match(html, /<summary/); assert.doesNotMatch(html, /\b(?:NaN|Infinity)\b/);
    assert.doesNotMatch(html, /<(?:script|iframe|image|img)\b|\s(?:src|href)="https?:/);
  }
  const hostile = renderToStaticMarkup(<ConversationVisual document={ready({ ...chart, title: '<svg onload="alert(1)">', labels: ['<img src="https://example.test/a">', "B"] })} />);
  assert.match(hostile, /&lt;svg/); assert.doesNotMatch(hostile, /<img/);
});

test("3D scene has keyboard focus, rotation instructions, accessible zoom and reset controls", () => {
  const html = renderToStaticMarkup(<ConversationVisual document={ready({ ...scene, edges: [{ from: "a", to: "b", label: "SQL" }] })} />);
  assert.match(html, /tabindex="0"/); assert.match(html, /方向键调整/); assert.match(html, /缩小三维场景/); assert.match(html, /type="range"/); assert.match(html, /复位视角/); assert.match(html, /visual-scene-box/);
  assert.match(html, /aria-pressed="false">连接说明/);
  assert.match(html, /visual-scene-labels/);
  assert.match(html, /API → DB：SQL/);
  assert.doesNotMatch(html, /<text[^>]*>SQL<\/text>/);
});

test("dense and coincident flow nodes remain distinct, bounded and connected without mutating source coordinates", () => {
  for (const coordinates of [[10, 35, 60, 80], [50, 50, 50, 50], [50, 50.00001, 50.00002, 50.00003]]) {
    const nodes = coordinates.map((x, index) => ({ id: String(index), label: "包含中文及长名称的完整数据库节点", x, y: 50 }));
    const source = JSON.stringify(nodes);
    const layout = flowLayout(nodes);
    const points = [...layout.positions.values()];
    assert.ok(layout.width <= 2400 && layout.height <= 2400);
    for (let a = 0; a < points.length; a++) for (let b = 0; b < a; b++) {
      assert.ok(Math.abs(points[a]!.x - points[b]!.x) >= 223.99 || Math.abs(points[a]!.y - points[b]!.y) >= 87.99);
    }
    assert.equal(JSON.stringify(nodes), source);
    const html = renderToStaticMarkup(<ConversationVisual document={ready({ ...flow, nodes, edges: [{ from: "0", to: "1", label: "HTTP" }] })} />);
    assert.match(html, /已调整节点间距/); assert.match(html, /marker-end=/); assert.match(html, /<tspan/);
    assert.doesNotMatch(html, /NaN|Infinity/);
  }
});

test("numeric scaling preserves negative values and stays finite for constant and extreme domains", () => {
  assert.deepEqual(numericDomain([-4, -2], true), [-4, 0]);
  for (const values of [[0], [1e9, -1e9], [1e9, 1e9], [-.000001, .000001]]) {
    const domain = numericDomain(values); assert.ok(domain[0] < domain[1]);
    for (const value of values) assert.ok(Number.isFinite(scaled(value, domain, 38, 270)));
  }
});

test("projection responds to rotation and clamps view limits without producing invalid geometry", () => {
  const document = ready(scene); assert.equal(document.kind, "scene3d"); if (document.kind !== "scene3d") return;
  const original = sceneProjector(document.nodes, DEFAULT_VIEW_3D)([2, 1, -1]);
  const changed = sceneProjector(document.nodes, adjustView3D(DEFAULT_VIEW_3D, .5, .2, 2))([2, 1, -1]);
  assert.notDeepEqual(original, changed);
  assert.equal(adjustView3D(DEFAULT_VIEW_3D, 1000, 1000, 1000).zoom, 2.5);
  assert.equal(adjustView3D(DEFAULT_VIEW_3D, 0, -1000, -1000).pitch, -1.4);
  const extreme = { ...document.nodes[0]!, position: [1e9, -1e9, 1e9] as [number, number, number] };
  const projected = sceneProjector([extreme], DEFAULT_VIEW_3D)(extreme.position);
  assert.ok(Object.values(projected).every(Number.isFinite));
});
