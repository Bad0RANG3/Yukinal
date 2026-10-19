import assert from "node:assert/strict";
import test from "node:test";
import { conversationScrollGeometry as geometry, conversationScrollTarget as target } from "../src/features/agent/conversation-scroll.js";

test("a short or unmeasurable conversation has no interactive scroll rail", () => {
  for (const [viewport, content, track] of [[500, 300, 468], [500, 500, 468], [0, 500, 0], [500, 5000, 0], [NaN, 1000, 400]]) {
    const rail = geometry(viewport!, content!, 0, track!);
    assert.equal(rail.enabled, false);
    assert.equal(target(200, 10, rail), 0);
    assert.ok(Number.isFinite(rail.thumbTop));
  }
});

test("ticks mark the visible proportion and reach both ends of the conversation", () => {
  const top = geometry(600, 2400, 0, 560);
  assert.equal(top.thumbHeight, 140);
  assert.equal(top.thumbTop, 0);
  const middle = geometry(600, 2400, 900, 560);
  assert.equal(middle.progress, .5);
  assert.equal(middle.thumbTop, 210);
  const bottom = geometry(600, 2400, 1800, 560);
  assert.equal(bottom.thumbTop + bottom.thumbHeight, bottom.trackHeight);
  assert.equal(target(560, bottom.thumbHeight, bottom), 1800);
});

test("grabbing any point inside the reading marker never jumps the conversation", () => {
  const rail = geometry(600, 2400, 900, 560);
  for (const grab of [0, 20, rail.thumbHeight / 2, rail.thumbHeight]) {
    assert.equal(target(rail.thumbTop + grab, grab, rail), 900);
    assert.equal(target(-200, grab, rail), 0);
    assert.equal(target(2000, grab, rail), rail.maxScroll);
  }
});

test("very long conversations keep a usable marker after resize and content growth", () => {
  const long = geometry(500, 100000, 50000, 468);
  assert.equal(long.thumbHeight, 36);
  const resized = geometry(300, 100000, 50000, 268);
  assert.equal(resized.maxScroll, 99700);
  assert.ok(resized.thumbTop < long.thumbTop);
  const grown = geometry(300, 120000, 50000, 268);
  assert.ok(grown.progress < resized.progress);
  assert.equal(geometry(300, 120000, -50, 268).progress, 0);
  assert.equal(geometry(300, 120000, 200000, 268).progress, 1);
  const compact = geometry(64, 1000, 936, 32);
  assert.equal(compact.thumbHeight, 16);
  assert.equal(target(32, compact.thumbHeight, compact), 936);
});
