import assert from "node:assert/strict";
import test from "node:test";
import { fitSecondaryWindow, moveSecondaryWindow, resizeSecondaryWindow } from "../src/components/secondary-window-frame.js";

test("a property window stays reachable after dragging, shrinking or changing viewport", () => {
  const viewport = { width: 1440, height: 900 };
  const initial = fitSecondaryWindow(viewport);
  assert.deepEqual(initial, { left: 400, top: 170, width: 640, height: 560 });
  const moved = moveSecondaryWindow(viewport, initial, 5000, -5000);
  assert.equal(moved.left + moved.width, 1424);
  assert.equal(moved.top, 16);
  const small = fitSecondaryWindow({ width: 360, height: 480 }, moved);
  assert.equal(small.width, 328);
  assert.equal(small.height, 448);
  assert.equal(small.left, 16);
  assert.equal(small.top, 16);
});

test("resizing keeps its anchor and respects readable minimums", () => {
  const viewport = { width: 1440, height: 900 };
  const initial = fitSecondaryWindow(viewport);
  const larger = resizeSecondaryWindow(viewport, initial, 5000, 5000);
  assert.equal(larger.left, initial.left);
  assert.equal(larger.top, initial.top);
  assert.equal(larger.left + larger.width, 1424);
  assert.equal(larger.top + larger.height, 884);
  const smaller = resizeSecondaryWindow(viewport, initial, -5000, -5000);
  assert.equal(smaller.width, 360);
  assert.equal(smaller.height, 300);
  assert.equal(smaller.left, initial.left);
});
