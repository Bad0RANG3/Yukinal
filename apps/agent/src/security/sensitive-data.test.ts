import assert from "node:assert/strict";
import test from "node:test";

import { REDACTED, redactSensitiveText, redactSensitiveValue } from "./sensitive-data.js";

test("redacts common API key, bearer, private-key, and credential assignment forms", () => {
  const input = [
    "api_key=demo123",
    "Authorization: Bearer example1",
    "sk-example1",
    "-----BEGIN PRIVATE KEY-----\nfixture\n-----END PRIVATE KEY-----",
  ].join("\n");
  const output = redactSensitiveText(input);

  assert.equal(output.includes("demo123"), false);
  assert.ok(output.includes(REDACTED));
});

test("redacts nested sensitive keys without mutating unrelated diagnostic fields", () => {
  const output = redactSensitiveValue({ service: "api", nested: { password: "not-for-output", port: 443 } }) as Record<string, unknown>;
  assert.equal((output.nested as Record<string, unknown>).password, REDACTED);
  assert.equal((output.nested as Record<string, unknown>).port, 443);
});
