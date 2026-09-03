import assert from "node:assert/strict";
import test from "node:test";

import { encodeFrame, FrameTooLargeError, MAX_FRAME_BYTES, NdjsonDecoder } from "./ndjson.js";

test("decodes frames split across arbitrary chunk boundaries", () => {
  const decoder = new NdjsonDecoder();
  const wire = encodeFrame({ a: 1 }) + encodeFrame({ b: 2 });
  const first = wire.slice(0, 9);
  const frames = [...decoder.push(first), ...decoder.push(wire.slice(9))];
  assert.deepEqual(frames, [{ a: 1 }, { b: 2 }]);
});

test("a malformed frame is reported without killing the stream", () => {
  const malformed: Array<{ line: string; error: unknown }> = [];
  const decoder = new NdjsonDecoder((line, error) => malformed.push({ line, error }));
  const frames = decoder.push(`{"ok":true}\nnot-json\n{"ok":false}\n`);
  assert.deepEqual(frames, [{ ok: true }, { ok: false }]);
  assert.equal(malformed.length, 1);
  assert.equal(malformed[0]?.line, "not-json");
});

test("refuses to buffer an unbounded frame", () => {
  const reported: unknown[] = [];
  const decoder = new NdjsonDecoder((_line, error) => reported.push(error));
  decoder.push("x".repeat(MAX_FRAME_BYTES + 10));
  assert.equal(reported.length, 1);
  assert.ok(reported[0] instanceof FrameTooLargeError);
});

test("end() flushes a delimiter-less trailing frame", () => {
  const decoder = new NdjsonDecoder();
  decoder.push('{"partial":');
  decoder.push('"yes"}');
  assert.deepEqual(decoder.end(), [{ partial: "yes" }]);
});
