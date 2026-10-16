import assert from "node:assert/strict";
import test from "node:test";
import { artifactFromContent, sandboxedArtifactDocument } from "../src/features/workpages/workpage-model.js";
import { useWorkpagesStore } from "../src/features/workpages/workpages-store.js";

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
