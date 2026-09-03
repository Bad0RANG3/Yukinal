/** Pins the TS consts to the JSON fixture (the same file the Rust core compiles in). */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { HEALTH_THRESHOLDS, healthClass } from "./health.js";

const fixture = JSON.parse(
  readFileSync(new URL("../../fixtures/health_thresholds.json", import.meta.url), "utf8"),
) as typeof HEALTH_THRESHOLDS;

test("health thresholds match the shared fixture (Rust mirror guard)", () => {
  assert.deepEqual(HEALTH_THRESHOLDS, fixture);
});

test("healthClass maps usage to classes at the thresholds", () => {
  assert.equal(healthClass(0, HEALTH_THRESHOLDS.cpu), "healthy");
  assert.equal(healthClass(69.9, HEALTH_THRESHOLDS.cpu), "healthy");
  assert.equal(healthClass(70, HEALTH_THRESHOLDS.cpu), "warning");
  assert.equal(healthClass(89, HEALTH_THRESHOLDS.cpu), "warning");
  assert.equal(healthClass(90, HEALTH_THRESHOLDS.cpu), "critical");
  assert.equal(healthClass(100, HEALTH_THRESHOLDS.cpu), "critical");
});