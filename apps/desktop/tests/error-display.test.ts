import assert from "node:assert/strict";
import test from "node:test";

import { describeInvestigationFailure } from "../src/features/investigations/error-display.js";

test("unknown remote outcomes direct operators to inspect before continuing", () => {
  const display = describeInvestigationFailure({ code: "outcome_unknown" });
  assert.equal(display.label, "远端结果未知");
  assert.match(display.nextStep, /只读证据/);
  assert.match(display.nextStep, /不要直接重试/);
});
