import assert from "node:assert/strict";
import test from "node:test";

import { networkProxyInput } from "../src/lib/network.js";

test("direct mode never serializes a proxy credential", () => {
  assert.deepEqual(
    networkProxyInput({ mode: "direct", credential: "user:pw", clearCredential: false }),
    { mode: "direct" },
  );
});

test("explicitly clearing a credential does not serialize a replacement", () => {
  assert.deepEqual(
    networkProxyInput({ mode: "system", credential: "user:pw", clearCredential: true }),
    { mode: "system", clearCredential: true },
  );
});
