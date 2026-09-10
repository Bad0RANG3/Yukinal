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

test("critical actions always require a direct user approval", () => {
  const engine = new PermissionEngine();
  const policyOnly = engine.evaluate({
    declaration: declaration({ name: "ssh.execute", risk: "read" }),
    target: target("development"),
    input: { command: "mkfs.ext4 /dev/sda1" },
    policy: { ...STAGING_POLICY, tiers: { read: "auto", write: "auto", dangerous: "auto" } },
  });
  assert.equal(policyOnly.finalRisk, "critical");
  assert.equal(policyOnly.outcome, "ask");

  const delegated = engine.evaluate({
    declaration: declaration({ name: "ssh.execute", risk: "read" }),
    target: target("development"),
    input: { command: "mkfs.ext4 /dev/sda1" },
    permissionMode: "auto",
    policy: { ...STAGING_POLICY, tiers: { read: "auto", write: "auto", dangerous: "auto" } },
  });
  assert.equal(delegated.outcome, "ask");
  assert.equal(delegated.approvedBy, undefined);
});

test("ask mode pauses before writes while keeping reads automatic", () => {
  const engine = new PermissionEngine();
  const write = engine.evaluate({
    declaration: declaration({ name: "filesystem.write", risk: "medium" }),
    target: target("staging"),
    input: {},
    permissionMode: "ask",
    policy: STAGING_POLICY,
  });
  assert.equal(write.outcome, "ask");
  assert.equal(write.approvedBy, undefined);

  const read = engine.evaluate({
    declaration: declaration({ name: "docker.ps", risk: "read" }),
    target: target("staging"),
    input: {},
    permissionMode: "ask",
    policy: STAGING_POLICY,
  });
  assert.equal(read.outcome, "auto");
  assert.equal(read.approvedBy, "policy");
});

test("auto mode delegates only a write-tier action on development or staging", () => {
  const engine = new PermissionEngine();
  const delegated = engine.evaluate({
    declaration: declaration({ name: "filesystem.write", risk: "medium" }),
    target: target("staging"),
    input: {},
    permissionMode: "auto",
    policy: PRODUCTION_POLICY,
  });
  assert.equal(delegated.outcome, "auto");
  assert.equal(delegated.approvedBy, "agent");

  const production = engine.evaluate({
    declaration: declaration({ name: "filesystem.write", risk: "medium" }),
    target: target("production"),
    input: {},
    permissionMode: "auto",
    policy: PRODUCTION_POLICY,
  });
  assert.equal(production.outcome, "ask");
  assert.equal(production.approvedBy, undefined);

  const denied = engine.evaluate({
    declaration: declaration({ name: "filesystem.write", risk: "medium" }),
    target: target("staging"),
    input: {},
    permissionMode: "auto",
    policy: { ...PRODUCTION_POLICY, tiers: { read: "auto", write: "deny", dangerous: "deny" } },
  });
  assert.equal(denied.outcome, "deny");
  assert.equal(denied.approvedBy, undefined);
});

test("a session grant covers a write-tier action", () => {
  const engine = new PermissionEngine();
  // Staging's floor is "medium", so a medium write stays write-tier here.
  const request = {
    declaration: declaration({ name: "filesystem.write", risk: "medium" }),
    target: target("staging"),
    input: { path: "/srv/app/config.yml" },
    policy: STAGING_POLICY,
    permissionMode: "ask" as const,
  };
  const first = engine.evaluate(request);
  assert.equal(first.outcome, "ask");
  assert.equal(first.tier, "write");

  engine.grantSession(first);
  const second = engine.evaluate(request);
  assert.equal(second.outcome, "auto");
  assert.equal(second.approvedBy, "user");
});

test("a session grant never covers the dangerous tier, however it was reached", () => {
  const engine = new PermissionEngine();

  // (a) intrinsically dangerous: docker.stop is high risk.
  const intrinsic = {
    declaration: declaration({ name: "docker.stop", risk: "high" }),
    target: target("staging"),
    input: {},
    policy: STAGING_POLICY,
  };
  engine.grantSession(engine.evaluate(intrinsic));
  assert.equal(engine.evaluate(intrinsic).outcome, "ask", "an intrinsically dangerous tool re-asks");
  assert.equal(engine.grantCount, 0, "and nothing was recorded as granted");

  // (b) escalated by the environment: a medium write on production, whose floor is
  // "high". This is the case that used to be advertised as auto-approved and then
  // denied at execution; the engine must not promise what checkTicket will refuse.
  const escalated = {
    declaration: declaration({ name: "filesystem.write", risk: "medium" }),
    target: target("production"),
    input: { path: "/etc/app/config.yml" },
    policy: PRODUCTION_POLICY,
  };
  const decision = engine.evaluate(escalated);
  assert.equal(decision.tier, "dangerous", "production escalates this to the dangerous tier");

  engine.grantSession(decision);
  assert.equal(engine.grantCount, 0, "a dangerous-tier decision must not be recorded as granted");
  const second = engine.evaluate(escalated);
  assert.equal(second.outcome, "ask", "so the next identical call still asks");
  assert.equal(second.approvedBy, undefined);
});

test("grants are scoped per server, so one host cannot unlock another", () => {
  const engine = new PermissionEngine();
  const request = {
    declaration: declaration({ name: "filesystem.write", risk: "medium" }),
    target: target("staging", "srv_api01"),
    input: {},
    policy: STAGING_POLICY,
    permissionMode: "ask" as const,
  };
  engine.grantSession(engine.evaluate(request));
  assert.equal(engine.evaluate(request).outcome, "auto");

  const otherServer = { ...request, target: target("staging", "srv_db01") };
  assert.equal(engine.evaluate(otherServer).outcome, "ask");
  assert.ok(grantKey("filesystem.write", target("staging", "srv_db01")).includes("srv_db01"));
});

test("grants are also scoped per workspace on the same server", () => {
  const engine = new PermissionEngine();
  const firstTarget = { ...target("staging", "srv_api01"), workspaceId: "ws_frontend" };
  const request = {
    declaration: declaration({ name: "filesystem.write" }),
    target: firstTarget,
    input: {},
    policy: STAGING_POLICY,
    permissionMode: "ask" as const,
  };
  engine.grantSession(engine.evaluate(request));
  assert.equal(engine.evaluate(request).outcome, "auto");

  const otherWorkspace = { ...firstTarget, workspaceId: "ws_backend" };
  assert.equal(engine.evaluate({ ...request, target: otherWorkspace }).outcome, "ask");
  assert.notEqual(grantKey("filesystem.write", firstTarget), grantKey("filesystem.write", otherWorkspace));
});

test("a grant is scoped to its environment, so staging cannot unlock production", () => {
  const engine = new PermissionEngine();
  const request = {
    declaration: declaration({ name: "filesystem.write", risk: "medium" }),
    target: target("staging", "srv_api01"),
    input: {},
    policy: STAGING_POLICY,
    permissionMode: "ask" as const,
  };
  engine.grantSession(engine.evaluate(request));
  assert.equal(engine.evaluate(request).outcome, "auto");

  // Same tool, same server, same workspace — only the environment differs, and
  // that alone raises the tier to dangerous and must re-ask.
  const production = { ...request, target: target("production", "srv_api01"), policy: PRODUCTION_POLICY };
  assert.equal(engine.evaluate(production).outcome, "ask");
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

// --- run modes: read-only is enforced, not requested -------------------------

test("read-only and plan modes deny every non-read action outright", () => {
  for (const mode of ["readonly", "plan"] as const) {
    const engine = new PermissionEngine();
    const decision = engine.evaluate({
      declaration: declaration({ name: "docker.restart" }),
      target: target("development"),
      input: {},
      policy: STAGING_POLICY,
      mode,
    });
    // A deny, not an ask: the user must not be able to approve their way past
    // the mode they themselves selected for this run.
    assert.equal(decision.outcome, "deny", `${mode} must deny`);
    assert.equal(decision.approvedBy, undefined);
    assert.match(decision.reason, new RegExp(`in ${mode} mode`));
    // A denied call never carries an approval id, so no approval card can appear.
    assert.equal(decision.approvalId, undefined);
  }
});

test("read-only modes still allow reads", () => {
  for (const mode of ["readonly", "plan"] as const) {
    const engine = new PermissionEngine();
    const decision = engine.evaluate({
      declaration: declaration({ name: "docker.ps", risk: "read" }),
      target: target("production"),
      input: {},
      policy: PRODUCTION_POLICY,
      mode,
    });
    assert.equal(decision.outcome, "auto", `${mode} must still allow reads`);
  }
});

test("no delegation, grant or permission mode can widen a read-only run", () => {
  // Each of these would normally turn a write into an automatic or approved
  // execution. In a read-only mode none of them may take effect.
  const engine = new PermissionEngine();
  const base = {
    declaration: declaration({ name: "filesystem.write" }),
    target: target("development"),
    input: {},
    policy: STAGING_POLICY,
    mode: "readonly" as const,
  };

  assert.equal(engine.evaluate({ ...base, permissionMode: "auto" }).outcome, "deny");
  assert.equal(engine.evaluate({ ...base, permissionMode: "ask" }).outcome, "deny");

  // A session grant obtained elsewhere must not reopen the door either.
  const grantEngine = new PermissionEngine();
  const granted = { ...base, mode: "goal" as const };
  grantEngine.grantSession(grantEngine.evaluate(granted));
  assert.equal(grantEngine.evaluate(granted).outcome, "auto", "grant works outside read-only");
  assert.equal(grantEngine.evaluate({ ...granted, mode: "readonly" }).outcome, "deny", "grant must not survive into read-only");
});

test("goal mode is the unconstrained default and changes nothing", () => {
  const explicit = new PermissionEngine().evaluate({
    declaration: declaration({ name: "filesystem.write" }),
    target: target("development"),
    input: {},
    policy: STAGING_POLICY,
    mode: "goal",
  });
  const omitted = new PermissionEngine().evaluate({
    declaration: declaration({ name: "filesystem.write" }),
    target: target("development"),
    input: {},
    policy: STAGING_POLICY,
  });
  assert.equal(explicit.outcome, omitted.outcome);
  assert.notEqual(explicit.outcome, "deny");
});
