import assert from "node:assert/strict";
import test from "node:test";

import { SafeCustomHeadersSchema } from "./provider.js";

test("provider custom headers accept metadata but reject credential channels", () => {
  assert.deepEqual(SafeCustomHeadersSchema.parse({ "HTTP-Referer": "https://desktop.example" }), {
    "HTTP-Referer": "https://desktop.example",
  });
  assert.equal(SafeCustomHeadersSchema.safeParse({ Authorization: "Bearer not-for-storage" }).success, false);
  assert.equal(SafeCustomHeadersSchema.safeParse({ "X-Api-Key": "not-for-storage" }).success, false);
});
