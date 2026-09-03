import assert from "node:assert/strict";
import test from "node:test";

import type { ToolDeclaration } from "@yukinal/shared";

import { createProviderNameIndex } from "./name-index.js";

function declaration(name: string): ToolDeclaration {
  return {
    name,
    description: `test tool ${name}`,
    risk: "read",
    timeoutMs: 1_000,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    origin: { kind: "builtin" },
  };
}

test("advertises tools with provider-safe names", () => {
  const index = createProviderNameIndex([declaration("docker.ps"), declaration("server.info")]);
  assert.deepEqual(
    index.specs().map((spec) => spec.function.name),
    ["docker__ps", "server__info"],
  );
});

test("maps a model tool call back to the internal name", () => {
  const index = createProviderNameIndex([declaration("docker.logs")]);
  assert.equal(index.providerFor("docker.logs"), "docker__logs");
  assert.equal(index.internalFor("docker__logs"), "docker.logs");
  assert.equal(index.internalFor("docker__nope"), undefined);
});

// The internal charset (lowercase segments + single dots) makes a dot/underscore
// collision unreachable; the guard stays because MCP servers bring foreign names.
test("refuses duplicate registration of the same tool", () => {
  assert.throws(
    () => createProviderNameIndex([declaration("docker.ps"), declaration("docker.ps")]),
    /collision/i,
  );
});

test("specs are deep-copied so callers cannot mutate the registry view", () => {
  const index = createProviderNameIndex([declaration("docker.ps")]);
  const first = index.specs()[0];
  if (first) first.function.name = "tampered";
  assert.equal(index.specs()[0]?.function.name, "docker__ps");
});
