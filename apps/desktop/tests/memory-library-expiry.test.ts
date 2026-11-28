import assert from "node:assert/strict";
import test from "node:test";
import {
  formatMemoryExpiry,
  resolveExpiryIso,
  toDatetimeLocalValue,
} from "../src/features/agent/memory-expiry.js";

test("resolveExpiryIso handles presets and custom instants", () => {
  const base = Date.parse("2026-11-29T00:00:00.000Z");
  assert.equal(resolveExpiryIso("never", "", base), undefined);
  assert.equal(resolveExpiryIso("7d", "", base), "2026-12-06T00:00:00.000Z");
  assert.equal(resolveExpiryIso("30d", "", base), "2026-12-29T00:00:00.000Z");
  assert.equal(resolveExpiryIso("90d", "", base), "2027-02-27T00:00:00.000Z");
  assert.equal(resolveExpiryIso("custom", "", base), undefined);

  const custom = resolveExpiryIso("custom", "2030-01-01T12:30", base);
  assert.ok(custom && custom.endsWith("Z"), "custom expiry must normalize to ISO-8601 UTC");
  assert.throws(() => resolveExpiryIso("custom", "not-a-date", base), /有效的过期时间/);
});

test("toDatetimeLocalValue and formatMemoryExpiry handle valid and absent timestamps", () => {
  assert.equal(toDatetimeLocalValue(undefined), "");
  assert.equal(toDatetimeLocalValue("invalid"), "");
  assert.match(toDatetimeLocalValue("2030-01-01T00:00:00Z"), /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}$/);
  assert.equal(formatMemoryExpiry(undefined), null);
  assert.ok(formatMemoryExpiry("2030-01-01T00:00:00Z")?.includes("2030"));
});
