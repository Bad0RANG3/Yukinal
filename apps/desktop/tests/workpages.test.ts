import assert from "node:assert/strict";
import test from "node:test";
import { artifactFromContent, sandboxedArtifactDocument } from "../src/features/workpages/workpage-model.js";
import { nextTabIndex } from "../src/features/workpages/tab-navigation.js";
import { useWorkpagesStore } from "../src/features/workpages/workpages-store.js";
import { DEFAULT_UI_PREFERENCES, sanitizePreferences } from "../src/stores/preferences-store.js";

test("artifact output requires an explicit bounded visual envelope", () => {
  const rawHtml = artifactFromContent("a", "Reply", "<script>window.parent.postMessage('x','*')</script>");
  assert.equal(rawHtml.content.format, "markdown");
  const chart = artifactFromContent("b", "Load", { format: "chart", unit: "%", series: [{ label: "CPU", value: 28 }, { label: "Delta", value: -3 }] });
  assert.equal(chart.content.format, "chart");
  assert.equal(artifactFromContent("c", "Invalid", { format: "chart", series: [{ label: "CPU", value: Infinity }] }).content.format, "json");
  assert.equal(artifactFromContent("d", "Too large", { format: "html", html: "x".repeat(200_001) }).content.format, "json");
});

test("flow output rejects ambiguous IDs and unresolved edges", () => {
  const nodes = [{ id: "a", label: "Read logs" }, { id: "b", label: "Review" }];
  assert.equal(artifactFromContent("a", "Flow", { format: "flow", nodes, edges: [{ from: "a", to: "b" }] }).content.format, "flow");
  assert.equal(artifactFromContent("a", "Flow", { format: "flow", nodes, edges: [{ from: "a", to: "missing" }] }).content.format, "json");
  assert.equal(artifactFromContent("a", "Flow", { format: "flow", nodes: [nodes[0], nodes[0]], edges: [] }).content.format, "json");
});

test("interactive document applies a restrictive policy before untrusted content", () => {
  const html = '<meta http-equiv="Content-Security-Policy" content="default-src *"><script>fetch("https://example.com")</script>';
  const document = sandboxedArtifactDocument(html);
  assert.ok(document.indexOf("default-src 'none'") < document.indexOf(html));
  for (const restriction of ["connect-src 'none'", "frame-src 'none'", "worker-src 'none'", "base-uri 'none'", "form-action 'none'"]) assert.ok(document.includes(restriction));
  assert.ok(document.includes("script-src 'unsafe-inline'"));
  assert.ok(document.includes(html));
});

test("workpages lock server targets and closing a terminal preserves its mounted session", () => {
  useWorkpagesStore.setState({ pages: [], retainedTerminals: [], recent: [], activeId: null, expandedId: null });
  const store = useWorkpagesStore.getState();
  store.openTool("terminal", { serverId: "server-a", dock: "left" });
  store.openTool("logs", { serverId: "server-b", dock: "right" });
  const terminal = useWorkpagesStore.getState().pages.find((page) => page.kind === "terminal")!;
  assert.equal(terminal.serverId, "server-a");
  store.close(terminal.id);
  assert.equal(useWorkpagesStore.getState().pages.length, 1);
  assert.equal(useWorkpagesStore.getState().retainedTerminals[0]?.id, terminal.id);
  const recent = useWorkpagesStore.getState().recent[0]!;
  store.reopen(recent);
  assert.equal(useWorkpagesStore.getState().pages.at(-1)?.serverId, "server-a");
  assert.equal(useWorkpagesStore.getState().retainedTerminals.length, 1);
  store.toggleExpanded(terminal.id);
  store.close(terminal.id);
  assert.equal(useWorkpagesStore.getState().expandedId, null);
  assert.equal(useWorkpagesStore.getState().activeId, "tool:logs:server-b");
});

test("the strip keeps at most eight tabs and moves the dropped one to recents", () => {
  useWorkpagesStore.setState({ pages: [], retainedTerminals: [], recent: [], activeId: null, expandedId: null });
  const store = useWorkpagesStore.getState();
  for (let index = 1; index <= 10; index += 1) store.openTool("logs", { serverId: `server-${index}` });
  const state = useWorkpagesStore.getState();
  assert.equal(state.pages.length, 8);
  // 最早的两个被挤到「最近关闭」，且顺序是旧在前，方便直接重开。
  assert.deepEqual(state.recent.slice(0, 2).map((page) => page.serverId), ["server-2", "server-1"]);
  assert.equal(state.activeId, "tool:logs:server-10");
});

test("the orientation axis decides the arrow keys, and never the layout identity", () => {
  // 横向只认左右：上下键必须交还给浏览器（滚标签行 / 滚对话）。
  assert.equal(nextTabIndex("ArrowRight", 0, 3, "horizontal"), 1);
  assert.equal(nextTabIndex("ArrowRight", 2, 3, "horizontal"), 0);
  assert.equal(nextTabIndex("ArrowLeft", 0, 3, "horizontal"), 2);
  assert.equal(nextTabIndex("ArrowDown", 0, 3, "horizontal"), null);
  assert.equal(nextTabIndex("ArrowUp", 0, 3, "horizontal"), null);
  // 纵向反过来。
  assert.equal(nextTabIndex("ArrowDown", 1, 3, "vertical"), 2);
  assert.equal(nextTabIndex("ArrowUp", 0, 3, "vertical"), 2);
  assert.equal(nextTabIndex("ArrowRight", 1, 3, "vertical"), null);
  assert.equal(nextTabIndex("ArrowLeft", 1, 3, "vertical"), null);
  // Home/End 与越界输入。
  assert.equal(nextTabIndex("Home", 2, 5, "horizontal"), 0);
  assert.equal(nextTabIndex("End", 0, 5, "vertical"), 4);
  assert.equal(nextTabIndex("ArrowRight", -1, 3, "horizontal"), null);
  assert.equal(nextTabIndex("ArrowRight", 3, 3, "horizontal"), null);
  assert.equal(nextTabIndex("ArrowRight", 0, 0, "horizontal"), null);
});

test("orientation is persisted and cannot be smuggled in as anything else", () => {
  assert.equal(DEFAULT_UI_PREFERENCES.workpagesOrientation, "horizontal");
  assert.equal(sanitizePreferences({ workpagesOrientation: "vertical" }).workpagesOrientation, "vertical");
  // 老版本存档没有这个键，读出来必须是默认值而不是 undefined。
  assert.equal(sanitizePreferences({ terminalFontSize: 14 }).workpagesOrientation, "horizontal");
  assert.equal(sanitizePreferences({ workpagesOrientation: "diagonal" }).workpagesOrientation, "horizontal");
  assert.equal(sanitizePreferences(null).workpagesOrientation, "horizontal");
  // 方向只是标签栏的性质：它不出现在工作页对象里。
  useWorkpagesStore.setState({ pages: [], retainedTerminals: [], recent: [], activeId: null, expandedId: null });
  useWorkpagesStore.getState().openTool("files", { serverId: "server-a" });
  assert.deepEqual(Object.keys(useWorkpagesStore.getState().pages[0]!).sort(), ["dock", "id", "kind", "serverId", "title"]);
});
