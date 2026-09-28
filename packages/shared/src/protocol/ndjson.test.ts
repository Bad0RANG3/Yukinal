import assert from "node:assert/strict";
import test from "node:test";

import { encodeFrame, FrameTooLargeError, MAX_FRAME_BYTES, MAX_SIDECAR_FRAME_BYTES, NdjsonDecoder } from "./ndjson.js";

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

test("rejects an oversized complete frame in the same chunk", () => {
  const reported: unknown[] = [];
  const decoder = new NdjsonDecoder((_line, error) => reported.push(error), 32);
  const frames = decoder.push(`${JSON.stringify({ payload: "x".repeat(64) })}\n`);
  assert.deepEqual(frames, []);
  assert.equal(reported.length, 1);
  assert.ok(reported[0] instanceof FrameTooLargeError);
  assert.equal(decoder.pendingBytes, 0);
});

test("end() flushes a delimiter-less trailing frame", () => {
  const decoder = new NdjsonDecoder();
  decoder.push('{"partial":');
  decoder.push('"yes"}');
  assert.deepEqual(decoder.end(), [{ partial: "yes" }]);
});

test("encodeFrame refuses a frame larger than the given limit", () => {
  assert.throws(() => encodeFrame({ blob: "x".repeat(200) }, 64), FrameTooLargeError);
  assert.equal(typeof encodeFrame({ ok: true }, MAX_SIDECAR_FRAME_BYTES), "string");
});

test("the decoder takes the caller's limit, so a sidecar frame may exceed the MCP frame", () => {
  const reported: unknown[] = [];
  const decoder = new NdjsonDecoder((_line, error) => reported.push(error), MAX_SIDECAR_FRAME_BYTES);
  // Over the MCP limit but under the sidecar limit: it must stay a valid pending frame.
  decoder.push("x".repeat(MAX_FRAME_BYTES + 1_000));
  assert.equal(reported.length, 0, "the sidecar limit must accept what the MCP limit would not");
  decoder.push("y".repeat(MAX_SIDECAR_FRAME_BYTES));
  assert.equal(reported.length, 1);
  assert.ok(reported[0] instanceof FrameTooLargeError);
});
