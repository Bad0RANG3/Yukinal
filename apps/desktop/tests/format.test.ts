import assert from "node:assert/strict";
import { test } from "node:test";

import { errorMessage, formatTimestamp } from "../src/lib/format.js";

test("a timestamp renders as month-day and time, with no year or seconds", () => {
  const formatted = formatTimestamp("2025-08-14T15:32:07.000Z");
  assert.ok(formatted.length > 0);
  // The year and the seconds are deliberately absent: these sit inside list rows,
  // where they add width without helping anyone judge "what just happened".
  assert.equal(formatted.includes("2025"), false, "the year does not belong in a row timestamp");
  assert.equal(/:\d{2}:\d{2}/.test(formatted), false, "seconds do not belong in a row timestamp");
  // Two calls must agree, which is what pinning the formatter instance buys.
  assert.equal(formatTimestamp("2025-08-14T15:32:07.000Z"), formatted);
});

test("different instants do not collapse to the same label", () => {
  const a = formatTimestamp("2025-08-14T15:32:07.000Z");
  const b = formatTimestamp("2025-08-14T15:33:07.000Z");
  assert.notEqual(a, b, "minute resolution lost");
});

test("an unparsable timestamp degrades to the raw value instead of throwing", () => {
  // One bad row must not take down the list it sits in. Rust is the source of these
  // strings, and a legacy or truncated value has reached the UI before.
  for (const bad of ["", "not a date", "0000-13-45"]) {
    assert.doesNotThrow(() => formatTimestamp(bad));
    assert.equal(formatTimestamp(bad), bad, `"${bad}" should fall through unchanged`);
  }
});

test("an Error is unwrapped to its message, and anything else is stringified", () => {
  assert.equal(errorMessage(new Error("connection refused")), "connection refused");
  assert.equal(errorMessage("plain string"), "plain string");
  assert.equal(errorMessage(undefined), "undefined");
  assert.equal(errorMessage(null), "null");
  // A subclass keeps its message; the point is that the boundary does not care
  // which of the several thrown shapes it is handed.
  class IpcError extends Error {}
  assert.equal(errorMessage(new IpcError("schema rejected the payload")), "schema rejected the payload");
});
