import assert from "node:assert/strict";
import test from "node:test";
import { transferProgress } from "../src/features/files/transfer-progress.js";

test("unknown and zero byte totals stay indeterminate while scanning", () => {
  assert.equal(transferProgress({ status: "queued", totalBytes: null, transferredBytes: 0 }), undefined);
  assert.equal(transferProgress({ status: "running", totalBytes: 0, transferredBytes: 0 }), undefined);
});

test("a completed empty transfer reports completion, not zero progress", () => {
  assert.equal(transferProgress({ status: "completed", totalBytes: 0, transferredBytes: 0 }), 100);
  assert.equal(transferProgress({ status: "completed", totalBytes: null, transferredBytes: 0 }), 100);
});

test("known totals show bounded progress and a cancelled transfer keeps its partial value", () => {
  assert.equal(transferProgress({ status: "running", totalBytes: 200, transferredBytes: 50 }), 25);
  assert.equal(transferProgress({ status: "running", totalBytes: 100, transferredBytes: 110 }), 100);
  assert.equal(transferProgress({ status: "cancelled", totalBytes: 100, transferredBytes: 30 }), 30);
});
