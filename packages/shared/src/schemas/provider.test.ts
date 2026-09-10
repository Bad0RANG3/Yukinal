import assert from "node:assert/strict";
import test from "node:test";

import { ProviderSaveInputSchema, SafeCustomHeadersSchema } from "./provider.js";

test("provider custom headers accept metadata but reject credential channels", () => {
  assert.deepEqual(SafeCustomHeadersSchema.parse({ "HTTP-Referer": "https://desktop.example" }), {
    "HTTP-Referer": "https://desktop.example",
  });
  assert.equal(SafeCustomHeadersSchema.safeParse({ Authorization: "Bearer not-for-storage" }).success, false);
  assert.equal(SafeCustomHeadersSchema.safeParse({ "X-Api-Key": "not-for-storage" }).success, false);
});

test("custom provider IDs follow OpenCode's lowercase identifier format", () => {
  const valid = ProviderSaveInputSchema.safeParse({
    providerId: "my-provider_01",
    label: "My Provider",
    baseUrl: "https://api.example.com/v1",
    model: "model-id",
  });
  assert.equal(valid.success, true);
  assert.equal(ProviderSaveInputSchema.safeParse({
    providerId: "My Provider",
    label: "My Provider",
    baseUrl: "https://api.example.com/v1",
    model: "model-id",
  }).success, false);
});
