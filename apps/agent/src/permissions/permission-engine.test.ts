import assert from "node:assert/strict";
import test from "node:test";

import {
  PRODUCTION_POLICY,
  STAGING_POLICY,
  type CommandRiskFact,
  type ToolDeclaration,
  type ToolTarget,
} from "@yukinal/shared";

import { PermissionEngine, grantKey } from "./permission-engine.js";

const target = (environment: ToolTarget["environment"], serverId = "srv_01abc"): ToolTarget => ({
  host: "remote",
  serverId,
  environment,
});

function declaration(overrides: Partial<ToolDeclaration> = {}): ToolDeclaration {
  return {
    name: "docker.restart",
    description: "Restart a container",
    risk: "medium",
    timeoutMs: 10_000,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    inputSchema: { type: "object" },
    origin: { kind: "builtin" },
    ...overrides,
  };
}

test("layer 1 only: a read tool on staging is automatic", () => {
  const engine = new PermissionEngine();
  const decision = engine.evaluate({
    declaration: declaration({ name: "docker.ps", risk: "read" }),
    target: target("staging"),
    input: {},
    policy: STAGING_POLICY,
  });
  assert.equal(decision.outcome, "auto");
  assert.equal(decision.finalRisk, "read");
  assert.deepEqual(
    decision.facts.map((fact) => fact.source),
    ["tool", "environment"],
  );
});

test("layer 2 raises risk for the concrete command", () => {
  const engine = new PermissionEngine();
  const decision = engine.evaluate({
    declaration: declaration({ name: "ssh.execute", risk: "low" }),
    target: target("development"),
    input: { command: "rm -rf /etc" },
  });
  assert.equal(decision.finalRisk, "critical");
  const commandFact = decision.facts.find((fact): fact is CommandRiskFact => fact.source === "command");
  assert.ok(commandFact, "expected a command-layer fact");
  assert.ok(commandFact.matched.includes("rm-rf"));
});

test("layer 3: production turns a medium write into an approval", () => {
  const engine = new PermissionEngine();
  const onStaging = engine.evaluate({
    declaration: declaration(),
    target: target("staging"),
    input: {},
    policy: STAGING_POLICY,
  });
  const onProduction = engine.evaluate({
    declaration: declaration(),
    target: target("production"),
    input: {},
    policy: PRODUCTION_POLICY,
  });
  assert.equal(onStaging.outcome, "auto");
  assert.equal(onProduction.outcome, "ask");
  assert.equal(typeof onProduction.approvalId, "string", "an ask decision must carry an approval id");
  assert.ok(onProduction.reason.includes("Production") || onProduction.reason.length > 0);
});

test("critical actions can never be auto-approved, even by a permissive policy", () => {
  const engine = new PermissionEngine();
  const decision = engine.evaluate({
    declaration: declaration({ name: "ssh.execute", risk: "read" }),
    target: target("development"),
    input: { command: "mkfs.ext4 /dev/sda1" },
    policy: { ...STAGING_POLICY, tiers: { read: "auto", write: "auto", dangerous: "auto" } },
  });
  assert.equal(decision.finalRisk, "critical");
  assert.equal(decision.outcome, "ask");
});

test("a session grant covers read/write but never the dangerous tier", () => {
  const engine = new PermissionEngine();
  const request = {
    declaration: declaration({ name: "filesystem.write", risk: "medium" }),
    target: target("production"),
    input: { path: "/etc/app/.env" },
    policy: PRODUCTION_POLICY,
  };
  const first = engine.evaluate(request);
  assert.equal(first.outcome, "ask");

  engine.grantSession(first);
  assert.equal(engine.evaluate(request).outcome, "auto");

  const dangerous = {
    declaration: declaration({ name: "docker.stop", risk: "high" }),
    target: target("production"),
    input: {},
    policy: PRODUCTION_POLICY,
  };
  engine.grantSession(engine.evaluate(dangerous));
  assert.equal(engine.evaluate(dangerous).outcome, "ask");
});

test("grants are scoped per server, so staging cannot unlock production", () => {
  const engine = new PermissionEngine();
  const request = {
    declaration: declaration({ name: "filesystem.write", risk: "medium" }),
    target: target("production", "srv_api01"),
    input: {},
    policy: PRODUCTION_POLICY,
  };
  engine.grantSession(engine.evaluate(request));
  assert.equal(engine.evaluate(request).outcome, "auto");

  const otherServer = { ...request, target: target("production", "srv_db01") };
  assert.equal(engine.evaluate(otherServer).outcome, "ask");
  assert.ok(grantKey("filesystem.write", target("production", "srv_db01")).includes("srv_db01"));
});

test("an unknown environment is treated like production", () => {
  const engine = new PermissionEngine();
  const decision = engine.evaluate({
    declaration: declaration({ name: "docker.restart" }),
    target: target("unknown"),
    input: {},
  });
  assert.equal(decision.finalRisk, "high");
  assert.equal(decision.policyId, PRODUCTION_POLICY.id);
});
