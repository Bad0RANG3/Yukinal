import assert from "node:assert/strict";
import test from "node:test";
import { anchoredPopoverPosition } from "../src/hooks/useAnchoredPopover.js";

test("usage surface opens above the trigger without exceeding the right edge", () => {
  const position = anchoredPopoverPosition({ left: 330, right: 378, top: 600, bottom: 628 }, { width: 360, height: 300 }, { width: 390, height: 844 }, "end");
  assert.equal(position.left, 18);
  assert.equal(position.top, 292);
  assert.equal(position.width, 360);
  assert.ok(position.left + position.width <= 378);
});

test("a trigger near the top opens below, and narrow viewports constrain width", () => {
  const position = anchoredPopoverPosition({ left: 10, right: 70, top: 20, bottom: 48 }, { width: 360, height: 300 }, { width: 320, height: 800 });
  assert.equal(position.left, 12);
  assert.equal(position.top, 56);
  assert.equal(position.width, 296);
});

test("short windows limit the surface to the larger available side", () => {
  const position = anchoredPopoverPosition({ left: 100, right: 160, top: 100, bottom: 128 }, { width: 340, height: 500 }, { width: 640, height: 240 });
  assert.equal(position.top, 136);
  assert.equal(position.maxHeight, 92);
  assert.ok(position.top + position.maxHeight <= 228);
});
