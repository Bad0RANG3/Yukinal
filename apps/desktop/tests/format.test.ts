import assert from "node:assert/strict";
import { test } from "node:test";

import { errorMessage, formatBytes, formatTimestamp } from "../src/lib/format.js";

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

test("a size steps through the binary units", () => {
  assert.equal(formatBytes(0), "0.0 B");
  assert.equal(formatBytes(128), "128 B");
  assert.equal(formatBytes(1024), "1.0 KB");
  assert.equal(formatBytes(1_048_576), "1.0 MB");
  assert.equal(formatBytes(5 * 1024 ** 3), "5.0 GB");
  assert.equal(formatBytes(3 * 1024 ** 4), "3.0 TB");
});

test("a size in a file listing is no longer capped at megabytes", () => {
  // The regression this merge fixed. The file pane's own formatter stopped at MB, so
  // a 5 GB remote image read "5120.0 MB" — arithmetically right and useless, since the
  // one thing a file listing answers is "how big is this".
  assert.equal(formatBytes(5 * 1024 ** 3), "5.0 GB");
  assert.notEqual(formatBytes(5 * 1024 ** 3), "5120.0 MB");
  // ...and past TB it keeps climbing rather than silently mislabelling.
  assert.equal(formatBytes(2 * 1024 ** 5), "2048 TB");
});

test("one decimal below ten, whole numbers above", () => {
  // The two merged implementations disagreed here: one rounded KB to a whole number,
  // so a 1536-byte file rendered as "2 KB" and the file's actual size was lost in the
  // display layer. Keeping the decimal is the deliberate choice.
  assert.equal(formatBytes(1536), "1.5 KB");
  assert.equal(formatBytes(1023), "1023 B");
  assert.equal(formatBytes(10 * 1024), "10 KB");
  assert.equal(formatBytes(99 * 1024), "99 KB");
  // Width stability is the reason for the threshold: below 10 the value carries a
  // decimal, above it never does, so a column of sizes does not jitter. The range
  // stops below 1024 because a larger count would step up to the next unit instead.
  for (const value of [10, 11, 100, 999, 1023]) {
    assert.equal(formatBytes(value * 1024).includes("."), false, `${value} KB should be whole`);
  }
});

test("a missing or unrepresentable size degrades to a dash", () => {
  // Collector output is routinely absent, and these render inside metric tiles where
  // "NaN B" would be far worse than a dash. This is the overview's original behaviour,
  // kept because the file pane's required `number` never hits it.
  assert.equal(formatBytes(undefined), "—");
  assert.equal(formatBytes(Number.NaN), "—");
  assert.equal(formatBytes(Number.POSITIVE_INFINITY), "—");
  assert.equal(formatBytes(Number.NEGATIVE_INFINITY), "—");
});
