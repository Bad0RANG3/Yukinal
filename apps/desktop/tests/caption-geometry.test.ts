import assert from "node:assert/strict";
import { test } from "node:test";
import { WindowShellSurfaceSchema } from "@yukinal/shared";
import { captionEdges } from "../src/app/caption-surface.js";

test("fractional adjacent tracks still produce a valid native caption at wide sizes", () => {
  const edges = captionEdges(311.1875, 1608.812515, 1608.8125, 1920);
  assert.ok(edges);
  assert.equal(edges.right, edges.inspectorLeft);
  assert.equal(WindowShellSurfaceSchema.safeParse({ ...edges, radius: 14, background: "#282828", border: "#303030" }).success, true);
});

test("real overlap and invalid caption bounds retain the complete renderer fallback", () => {
  assert.equal(captionEdges(280, 1620, 1608, 1920), null);
  assert.equal(captionEdges(280, 1608, 1922, 1920), null);
  assert.equal(captionEdges(-1, 1160, 1160, 1440), null);
  assert.equal(captionEdges(280, Infinity, 1160, 1440), null);
  assert.deepEqual(captionEdges(280, 1160, 1160, 1440), { left: 280, right: 1160, inspectorLeft: 1160, viewportWidth: 1440 });
});
