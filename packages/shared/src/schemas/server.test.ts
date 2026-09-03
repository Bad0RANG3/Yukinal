import assert from "node:assert/strict";
import test from "node:test";

import { AddServerInputSchema, ServerSchema } from "./server.js";
import { PermissionDecisionSchema } from "./permission.js";

const validServer = {
  id: "srv_01abc",
  name: "Production API",
  connection: { host: "api.example.com", port: 22, username: "deploy" },
  capabilities: { linux: true, docker: true },
  status: "connected",
  metadata: { environment: "production", region: "Singapore" },
  createdAt: "2026-01-01T00:00:00.000Z",
  updatedAt: "2026-01-01T00:00:00.000Z",
};

test("accepts a contract-shaped server", () => {
  assert.equal(ServerSchema.safeParse(validServer).success, true);
});

test("rejects host-derived ids and non-ports", () => {
  assert.equal(
    ServerSchema.safeParse({ ...validServer, id: "api.example.com:22" }).success,
    false,
  );
  assert.equal(
    ServerSchema.safeParse({ ...validServer, connection: { ...validServer.connection, port: 0 } })
      .success,
    false,
  );
});

test("unknown environments must be declared, never guessed", () => {
  assert.equal(
    AddServerInputSchema.safeParse({
      name: "db",
      host: "10.0.0.5",
      username: "root",
      environment: "prod-ish",
      authentication: { method: "identity", identityId: "idn_1" },
    }).success,
    false,
  );
});

test("an add-server payload can carry a secret without the schema leaking it into types", () => {
  const parsed = AddServerInputSchema.safeParse({
    name: "db",
    host: "10.0.0.5",
    username: "root",
    environment: "staging",
    authentication: { method: "password", password: "hunter2" },
  });
  assert.equal(parsed.success, true);
});

test("permission decisions always state which layer spoke", () => {
  const parsed = PermissionDecisionSchema.safeParse({
    outcome: "ask",
    intrinsicRisk: "critical",
    finalRisk: "high",
    tier: "dangerous",
    facts: [
      { source: "tool", level: "medium", toolName: "ssh.execute" },
      { source: "command", level: "critical", command: "rm -rf /", matched: ["rm-rf"] },
      { source: "environment", level: "critical", environment: "production" },
    ],
    policyId: "policy.production",
    toolName: "ssh.execute",
    reason: "Command matches rm -rf rule on a production target",
    target: { host: "remote", serverId: "srv_01abc", environment: "production" },
    approvalId: "apr_1",
    requestedAt: "2026-01-01T00:00:00.000Z",
  });
  assert.equal(parsed.success, true);
});
