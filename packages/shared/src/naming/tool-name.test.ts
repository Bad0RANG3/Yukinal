import assert from "node:assert/strict";
import test from "node:test";

import {
  assertUniqueProviderNames,
  fromProviderToolName,
  InvalidToolNameError,
  isValidInternalToolName,
  toProviderToolName,
  ToolNameCollisionError,
  ToolNameTooLongError,
} from "./tool-name.js";

test("round-trips dot names through the provider boundary", () => {
  assert.equal(toProviderToolName("docker.ps"), "docker__ps");
  assert.equal(fromProviderToolName("docker__ps"), "docker.ps");
  assert.equal(fromProviderToolName(toProviderToolName("server.health")), "server.health");
});

test("accepts realistic internal names", () => {
  for (const name of ["docker.ps", "ssh.execute", "filesystem.read", "git.status", "server.health"]) {
    assert.equal(isValidInternalToolName(name), true, name);
  }
});

test("rejects names that a gateway would silently rewrite", () => {
  for (const name of ["Docker.ps", "docker", ".ps", "docker..ps", "docker.ps ", "docker__ps", "docker.ps.beta_1"]) {
    assert.equal(isValidInternalToolName(name), false, name);
  }
});

test("throws instead of sending an invalid name to the model", () => {
  assert.throws(() => toProviderToolName("Docker.ps"), InvalidToolNameError);
});

test("rejects names that exceed the provider length limit", () => {
  const long = `infrastructure.${"a".repeat(70)}`;
  assert.throws(() => toProviderToolName(long), ToolNameTooLongError);
});

test("the mapping is injective over valid names", () => {
  const mapped = ["docker.ps", "docker.logs", "a.b.c", "server.health"].map(toProviderToolName);
  assert.deepEqual(mapped, ["docker__ps", "docker__logs", "a__b__c", "server__health"]);
  assert.equal(new Set(mapped).size, mapped.length);
});

test("assertUniqueProviderNames tolerates a valid registry, duplicates included", () => {
  assert.doesNotThrow(() => assertUniqueProviderNames(["docker.ps", "docker.logs", "docker.inspect"]));
  assert.doesNotThrow(() => assertUniqueProviderNames(["docker.ps", "docker.ps"]));
});

test("a foreign spelling that would shadow a tool is caught at registration", () => {
  // e.g. an MCP server exposing "docker__get" while we have "docker.get"
  assert.throws(
    () => assertUniqueProviderNames(["docker.get", "docker__get"]),
    ToolNameCollisionError,
  );
});
