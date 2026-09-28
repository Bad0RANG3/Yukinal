import assert from "node:assert/strict";
import test from "node:test";

import { ENVIRONMENTS, isSessionGrantable, tierOf, type Environment, type RiskLevel } from "./risk.js";

const remote = (environment: Environment) => ({ host: "remote" as const, environment });
const local = (environment: Environment) => ({ host: "local" as const, environment });

test("a session grant is refused for critical anywhere, and for dangerous work outside remote dev/staging", () => {
  const cases: Array<{ label: string; finalRisk: RiskLevel; target: { host: "local" | "remote"; environment: Environment }; expected: boolean }> = [];

  for (const environment of ENVIRONMENTS) {
    // Critical is never remembered, on any host or environment.
    cases.push({ label: `critical on remote ${environment}`, finalRisk: "critical", target: remote(environment), expected: false });
    cases.push({ label: `critical on local ${environment}`, finalRisk: "critical", target: local(environment), expected: false });
    // A medium write is never dangerous, so it stays grantable everywhere.
    cases.push({ label: `medium on ${environment}`, finalRisk: "medium", target: remote(environment), expected: true });
  }

  for (const environment of ENVIRONMENTS) {
    cases.push({
      label: `high on remote ${environment}`,
      finalRisk: "high",
      target: remote(environment),
      expected: environment === "development" || environment === "staging",
    });
    cases.push({ label: `high on local ${environment}`, finalRisk: "high", target: local(environment), expected: false });
  }

  for (const entry of cases) {
    assert.equal(
      isSessionGrantable({ tier: tierOf(entry.finalRisk), finalRisk: entry.finalRisk, target: entry.target }),
      entry.expected,
      entry.label,
    );
  }
});

test("session grantability follows the tier after escalation, not the intrinsic risk", () => {
  // A medium write escalated to the dangerous tier by a production target is not
  // session-grantable, while the same write on staging is.
  assert.equal(isSessionGrantable({ tier: "dangerous", finalRisk: "high", target: remote("production") }), false);
  assert.equal(isSessionGrantable({ tier: "dangerous", finalRisk: "high", target: remote("staging") }), true);
});
