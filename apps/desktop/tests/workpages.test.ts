import assert from "node:assert/strict";
import test from "node:test";
import { artifactFromContent, sandboxedArtifactDocument } from "../src/features/workpages/workpage-model.js";
import { useWorkpagesStore } from "../src/features/workpages/workpages-store.js";
import { centerConversationFrame, fitConversationFrame, moveConversationFrame, resizeConversationFrame } from "../src/features/workpages/conversation-frame.js";

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
  store.openTool("terminal", { serverId: "server-a", dock: "left", additional: true });
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

test("returning to conversation preserves tools and reopening reveals the same terminal", () => {
  useWorkpagesStore.setState({ pages: [], retainedTerminals: [], recent: [], activeId: null, expandedId: null, operationsOpen: false, collapsed: false });
  const store = useWorkpagesStore.getState();
  store.openTool("terminal", { serverId: "server-a" });
  store.openTool("files", { serverId: "server-b" });
  const pages = useWorkpagesStore.getState().pages;
  const terminal = useWorkpagesStore.getState().retainedTerminals[0];
  store.setCollapsed(true);
  assert.equal(useWorkpagesStore.getState().pages, pages);
  assert.equal(useWorkpagesStore.getState().retainedTerminals[0], terminal);
  assert.equal(useWorkpagesStore.getState().recent.length, 0);
  store.openTool("terminal", { serverId: "server-a" });
  assert.equal(useWorkpagesStore.getState().collapsed, false);
  assert.equal(useWorkpagesStore.getState().pages.length, 2);
  assert.equal(useWorkpagesStore.getState().retainedTerminals[0], terminal);
  assert.equal(useWorkpagesStore.getState().activeId, terminal?.id);
  store.setCollapsed(true);
  store.setOperationsOpen(true);
  assert.equal(useWorkpagesStore.getState().collapsed, false);
  assert.equal(useWorkpagesStore.getState().operationsOpen, true);
});

test("permanent tools cannot be closed or displaced by additional tabs", () => {
  useWorkpagesStore.setState({ pages: [], retainedTerminals: [], recent: [], activeId: null, expandedId: null, operationsOpen: false, collapsed: false });
  const store = useWorkpagesStore.getState();
  for (const kind of ["files", "terminal", "logs", "services"] as const) store.openTool(kind, { serverId: "server-a" });
  const fixed = useWorkpagesStore.getState().pages;
  for (const page of fixed) store.close(page.id);
  assert.deepEqual(useWorkpagesStore.getState().pages, fixed);
  for (let i = 0; i < 10; i++) store.openArtifact({ id: `result-${i}`, title: `Result ${i}`, content: { format: "markdown", text: "Done" } });
  const state = useWorkpagesStore.getState();
  assert.equal(state.pages.length, 8);
  assert.deepEqual(state.pages.filter((page) => page.pinned), fixed);
  assert.ok(state.recent.every((page) => !page.pinned));
});

test("new terminal tabs have separate sessions and switching a fixed target retains the old target", () => {
  useWorkpagesStore.setState({ pages: [], retainedTerminals: [], recent: [], activeId: null, expandedId: null, operationsOpen: false, collapsed: false });
  const store = useWorkpagesStore.getState();
  store.openTool("terminal", { serverId: "server-a" });
  const original = useWorkpagesStore.getState().pages[0]!;
  store.openTool("terminal", { serverId: "server-a", additional: true });
  const additional = useWorkpagesStore.getState().pages[1]!;
  assert.notEqual(additional.id, original.id);
  assert.equal(additional.pinned, false);
  store.close(additional.id);
  assert.equal(useWorkpagesStore.getState().retainedTerminals.length, 2);
  store.openTool("terminal", { serverId: "server-b" });
  const state = useWorkpagesStore.getState();
  assert.equal(state.pages.find((page) => page.id === original.id)?.serverId, "server-a");
  assert.equal(state.pages.find((page) => page.id === original.id)?.pinned, false);
  assert.equal(state.pages.find((page) => page.pinned)?.serverId, "server-b");
  store.openTool("terminal", { serverId: "server-a" });
  assert.equal(useWorkpagesStore.getState().activeId, original.id);
  assert.equal(useWorkpagesStore.getState().pages.filter((page) => page.pinned).length, 1);
  assert.equal(useWorkpagesStore.getState().retainedTerminals.length, 3);
});

test("a team board is bound to its scope and opening it preserves existing terminal sessions", () => {
  useWorkpagesStore.setState({ pages: [], retainedTerminals: [], recent: [], activeId: null, expandedId: null });
  const store = useWorkpagesStore.getState();
  store.openTool("terminal", { serverId: "srv_a" });
  const terminal = useWorkpagesStore.getState().retainedTerminals[0];
  store.openTeamBoard("srv_a");
  store.openTeamBoard("srv_a");
  assert.equal(useWorkpagesStore.getState().pages.length, 2);
  assert.equal(useWorkpagesStore.getState().expandedId, "board:srv_a");
  store.openTeamBoard(null);
  assert.equal(useWorkpagesStore.getState().pages.at(-1)?.serverId, null);
  store.close("board:all");
  assert.equal(useWorkpagesStore.getState().retainedTerminals[0], terminal);
  assert.equal(useWorkpagesStore.getState().retainedTerminals.length, 1);
});

test("floating conversation starts centered and dragging stays inside the borrowed area", () => {
  const bounds = { left: 800, top: 16, width: 300, height: 800 };
  const frame = fitConversationFrame(bounds);
  assert.deepEqual(frame, { width: 300, height: 512, top: 160 });
  assert.equal(moveConversationFrame(bounds, frame, -2000).top, 16);
  assert.equal(moveConversationFrame(bounds, frame, 2000).top + frame.height, 816);
  assert.deepEqual(frame, { width: 300, height: 512, top: 160 });
  assert.equal(bounds.width, 300);
});

test("resizing preserves the top edge, respects readable minimums and fits smaller windows", () => {
  const bounds = { left: 800, top: 16, width: 300, height: 800 };
  const frame = fitConversationFrame(bounds);
  assert.deepEqual(resizeConversationFrame(bounds, frame, 2000, 2000), { width: 300, height: 656, top: 160 });
  const reduced = resizeConversationFrame(bounds, frame, -2000, -2000);
  assert.deepEqual(reduced, { width: 220, height: 260, top: 160 });
  assert.deepEqual(centerConversationFrame(bounds, reduced), { width: 220, height: 260, top: 286 });
  assert.deepEqual(fitConversationFrame({ left: 400, top: 16, width: 180, height: 210 }, frame), { width: 180, height: 210, top: 16 });
});
