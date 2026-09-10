import assert from "node:assert/strict";
import test from "node:test";

import { z } from "zod";

import type { PermissionDecision, ToolDeclaration, ToolTarget } from "@yukinal/shared";

import { PermissionEngine } from "../permissions/permission-engine.js";
import { TraceRecorder } from "../trace/trace-recorder.js";
import { ToolRegistry, checkTicket, type ExecutionTicket } from "./registry.js";
import { ToolFailure, type Tool } from "./tool.js";

const local: ToolTarget = { host: "local", environment: "development" };

function echoTool(overrides: Partial<Tool<{ text: string }, { text: string }>> = {}): Tool<{ text: string }, { text: string }> {
  const schema = z.object({ text: z.string() });
  return {
    name: "test.echo",
    description: "Echo text back",
    risk: "read",
    timeoutMs: 500,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    input: schema,
    async execute(input) {
      return { text: input.text };
    },
    ...overrides,
  };
}

function autoTicket(registry: ToolRegistry, name: string, input: unknown, target = local): ExecutionTicket {
  const engine = new PermissionEngine();
  const declaration = registry.declaration(name);
  assert.ok(declaration);
  const decision = engine.evaluate({ declaration, target, input });
  return { kind: "policy_auto", decision };
}

test("a registered tool exposes a JSON Schema derived from its zod input", () => {
  const registry = new ToolRegistry();
  const declaration = registry.register(echoTool());
  assert.equal(declaration.name, "test.echo");
  assert.equal(declaration.inputSchema.type, "object");
  assert.notEqual((declaration.inputSchema.properties as { text?: unknown }).text, undefined);
});

test("registration rejects names that would be rewritten at the provider boundary", () => {
  const registry = new ToolRegistry();
  assert.throws(() => registry.register(echoTool({ name: "Test.Echo" })), /dot-namespaced/);
  assert.throws(() => registry.register(echoTool({ name: "echo" })), /dot-namespaced/);
  assert.throws(() => registry.register(echoTool({ timeoutMs: 0 })), /timeoutMs/);
  assert.throws(() => registry.register(echoTool({ description: "  " })), /description/);
  registry.register(echoTool());
  assert.throws(() => registry.register(echoTool()), /already registered/);
});

/**
 * `checkTicket` is the single chokepoint every tool call passes through, and its
 * branches are individually load-bearing. Before this suite the only tickets ever
 * constructed in tests were `policy_auto`, so `session_auto` — the branch a real
 * code path needs — was never executed, which is how the broken session grant
 * shipped. Each test below drives one branch by hand.
 */
test("checkTicket rejects a decision made for a different tool", () => {
  const registry = new ToolRegistry();
  const declaration = registry.register(echoTool({ name: "test.echo" }));
  const other = { ...declaration, name: "test.other" };
  const decision = new PermissionEngine().evaluate({ declaration, target: local, input: { text: "x" } });

  const error = checkTicket(other, { callId: "c", traceId: "t", toolName: "test.other", input: { text: "x" }, target: local }, { kind: "policy_auto", decision });
  assert.equal(error?.code, "denied_by_policy");
  assert.match(error?.message ?? "", /test\.echo.*test\.other/);
});

test("checkTicket rejects a decision replayed onto a different target", () => {
  const registry = new ToolRegistry();
  const declaration = registry.register(echoTool({ name: "test.echo" }));
  const engine = new PermissionEngine();
  const staging: ToolTarget = { host: "remote", serverId: "srv_stg01", environment: "staging" };
  const production: ToolTarget = { host: "remote", serverId: "srv_prd01", environment: "production" };
  const decision = engine.evaluate({ declaration, target: staging, input: { text: "x" } });

  const error = checkTicket(declaration, { callId: "c", traceId: "t", toolName: "test.echo", input: { text: "x" }, target: production }, { kind: "policy_auto", decision });
  assert.equal(error?.code, "denied_by_policy");
  assert.match(error?.message ?? "", /srv_stg01.*srv_prd01/);
});

test("checkTicket refuses a dangerous tier without a direct user approval", () => {
  const registry = new ToolRegistry();
  const declaration = registry.register(echoTool({ name: "test.danger", risk: "critical" }));
  const request = { callId: "c", traceId: "t", toolName: "test.danger", input: { text: "x" }, target: local };
  const decision = new PermissionEngine().evaluate({ declaration, target: local, input: { text: "x" } });
  assert.equal(decision.tier, "dangerous");

  // Every auto-shaped ticket is refused, whatever provenance is claimed.
  for (const kind of ["policy_auto", "agent_auto", "session_auto"] as const) {
    const error = checkTicket(declaration, request, { kind, decision: { ...decision, outcome: "auto", approvedBy: kind === "agent_auto" ? "agent" : kind === "session_auto" ? "user" : "policy" } });
    assert.equal(error?.code, "denied_by_policy", `${kind} must not authorise a dangerous call`);
    assert.match(error?.message ?? "", /explicit user approval/);
  }
});

test("checkTicket refuses an auto ticket carrying a non-auto decision", () => {
  const registry = new ToolRegistry();
  const declaration = registry.register(echoTool({ name: "test.write", risk: "medium" }));
  const staging: ToolTarget = { host: "remote", serverId: "srv_stg01", environment: "staging" };
  // Ask mode pauses before every state-changing call, so this decision is "ask".
  const ask = new PermissionEngine().evaluate({ declaration, target: staging, input: { text: "x" }, permissionMode: "ask" });
  assert.equal(ask.outcome, "ask");
  const request = { callId: "c", traceId: "t", toolName: "test.write", input: { text: "x" }, target: staging };

  for (const kind of ["policy_auto", "agent_auto", "session_auto"] as const) {
    const error = checkTicket(declaration, request, { kind, decision: ask });
    assert.equal(error?.code, "denied_by_policy", `${kind} must not smuggle an "ask" through`);
    assert.match(error?.message ?? "", /arrived with an auto ticket/);
  }
});

test("checkTicket refuses an auto ticket whose provenance does not match its kind", () => {
  const registry = new ToolRegistry();
  const declaration = registry.register(echoTool({ name: "test.echo" }));
  const request = { callId: "c", traceId: "t", toolName: "test.echo", input: { text: "x" }, target: local };
  const engine = new PermissionEngine();

  const policyDecision = { ...engine.evaluate({ declaration, target: local, input: { text: "x" } }), outcome: "auto" as const, approvedBy: "policy" as const };
  assert.equal(checkTicket(declaration, request, { kind: "agent_auto", decision: policyDecision })?.message, "Agent auto ticket has no Agent delegation");
  assert.equal(checkTicket(declaration, request, { kind: "session_auto", decision: policyDecision })?.message, "Session auto ticket has no user session approval");

  const userDecision = { ...policyDecision, approvedBy: "user" as const };
  assert.equal(checkTicket(declaration, request, { kind: "policy_auto", decision: userDecision })?.message, "Policy auto ticket has no policy authorization");
  assert.equal(checkTicket(declaration, request, { kind: "agent_auto", decision: userDecision })?.message, "Agent auto ticket has no Agent delegation");
});

test("checkTicket limits Agent delegation to write-tier development and staging", () => {
  const registry = new ToolRegistry();
  const declaration = registry.register(echoTool({ name: "test.write", risk: "medium" }));
  const request = { callId: "c", traceId: "t", toolName: "test.write", input: { text: "x" }, target: { host: "remote", serverId: "srv_prd01", environment: "production" } as ToolTarget };
  const decision = { ...new PermissionEngine().evaluate({ declaration, target: request.target, input: { text: "x" } }), outcome: "auto" as const, approvedBy: "agent" as const };

  // A production target escalates this to the dangerous tier, so it is refused
  // there first; the envelope check is what protects a development target whose
  // tier drifted away from "write".
  const error = checkTicket(declaration, request, { kind: "agent_auto", decision });
  assert.equal(error?.code, "denied_by_policy");
  assert.match(error?.message ?? "", /explicit user approval|limited to write-tier/);
});

test("checkTicket binds a user_approved ticket to its own approval id", () => {
  const registry = new ToolRegistry();
  const declaration = registry.register(echoTool({ name: "test.write", risk: "medium" }));
  const staging: ToolTarget = { host: "remote", serverId: "srv_stg01", environment: "staging" };
  const request = { callId: "c", traceId: "t", toolName: "test.write", input: { text: "x" }, target: staging };
  const decision = new PermissionEngine().evaluate({ declaration, target: staging, input: { text: "x" }, permissionMode: "ask" });
  assert.ok(decision.approvalId);

  const matched = checkTicket(declaration, request, { kind: "user_approved", decision, approvalId: decision.approvalId ?? "", respondedAt: new Date().toISOString() });
  assert.equal(matched, undefined, "the ticket that carries the real approval id must be accepted");

  const mismatched = checkTicket(declaration, request, { kind: "user_approved", decision, approvalId: "apr_someone_elses", respondedAt: new Date().toISOString() });
  assert.equal(mismatched?.code, "denied_by_policy");
  assert.match(mismatched?.message ?? "", /does not match/);
});

test("checkTicket rejects a deny decision regardless of ticket kind", () => {
  const registry = new ToolRegistry();
  const declaration = registry.register(echoTool({ name: "test.write", risk: "medium" }));
  const request = { callId: "c", traceId: "t", toolName: "test.write", input: { text: "x" }, target: local };
  // Read-only runs deny every non-read tier before policy or delegation is consulted.
  const decision = new PermissionEngine().evaluate({ declaration, target: local, input: { text: "x" }, mode: "readonly" });
  assert.equal(decision.outcome, "deny");

  for (const kind of ["policy_auto", "agent_auto", "session_auto"] as const) {
    const error = checkTicket(declaration, request, { kind, decision });
    assert.equal(error?.code, "denied_by_policy", `${kind} must not override a deny`);
    assert.equal(error?.message, decision.reason);
  }
});

test("executes through the permission ticket and records a trace step", async () => {
  const registry = new ToolRegistry();
  registry.register(echoTool());
  const trace = new TraceRecorder("run_1", "smoke test");
  const seen: string[] = [];
  trace.subscribe((event) => seen.push(event.type));

  const result = await registry.execute(
    { callId: "call_1", traceId: trace.traceId, toolName: "test.echo", input: { text: "hi" }, target: local },
    autoTicket(registry, "test.echo", { text: "hi" }),
    { trace },
  );

  assert.equal(result.status, "success");
  assert.deepEqual(result.output, { text: "hi" });
  assert.equal(result.outputSummary?.includes("hi"), true);
  assert.ok(seen.includes("trace.started"));
  assert.ok(seen.includes("step.started"));
  assert.ok(seen.includes("step.updated"));
  assert.ok(trace.steps[0]);
  assert.equal(trace.steps[0]?.status, "done");
});

test("invalid input never reaches the tool", async () => {
  const registry = new ToolRegistry();
  let executed = false;
  registry.register(echoTool({ execute: async () => { executed = true; return { text: "no" }; } }));

  const result = await registry.execute(
    { callId: "c", traceId: "t", toolName: "test.echo", input: { text: 42 }, target: local },
    autoTicket(registry, "test.echo", { text: 42 }),
  );

  assert.equal(result.status, "failed");
  assert.equal(result.error?.code, "invalid_input");
  assert.equal(executed, false);
});

test("an unknown tool is reported with the registry contents, not a stack trace", async () => {
  const registry = new ToolRegistry();
  const decision: PermissionDecision = {
    outcome: "auto",
    intrinsicRisk: "read",
    finalRisk: "read",
    tier: "read",
    facts: [],
    policyId: "p",
    toolName: "test.missing",
    reason: "n/a",
    target: local,
    requestedAt: new Date().toISOString(),
  };
  const result = await registry.execute(
    { callId: "c", traceId: "t", toolName: "test.missing", input: {}, target: local },
    { kind: "policy_auto", decision },
  );
  assert.equal(result.error?.code, "not_found");
});

test("a decision made for another tool cannot unlock this one", async () => {
  const registry = new ToolRegistry();
  registry.register(echoTool());
  const ticket = autoTicket(registry, "test.echo", { text: "hi" });
  const result = await registry.execute(
    { callId: "c", traceId: "t", toolName: "test.echo", input: { text: "hi" }, target: local },
    { ...ticket, decision: { ...ticket.decision, toolName: "docker.restart" } },
  );
  assert.equal(result.error?.code, "denied_by_policy");
});

test("an ask decision cannot be smuggled through as an auto ticket", async () => {
  const registry = new ToolRegistry();
  registry.register(echoTool({ name: "test.write", risk: "medium" }));
  const production: ToolTarget = { host: "remote", serverId: "srv_1", environment: "production" };
  const engine = new PermissionEngine();
  const declaration = registry.declaration("test.write");
  assert.ok(declaration);
  const decision = engine.evaluate({ declaration, target: production, input: { text: "x" } });
  assert.equal(decision.outcome, "ask");

  const result = await registry.execute(
    { callId: "c", traceId: "t", toolName: "test.write", input: { text: "x" }, target: production },
    { kind: "policy_auto", decision },
  );
  assert.equal(result.error?.code, "denied_by_policy");
  assert.match(result.error?.message ?? "", /approval|policy_auto|"ask"/i);
});

test("a critical call rejects an Agent delegation ticket and needs user approval", async () => {
  const registry = new ToolRegistry();
  registry.register(echoTool({ name: "test.critical", risk: "critical" }));
  const production: ToolTarget = { host: "remote", serverId: "srv_critical", environment: "production" };
  const declaration = registry.declaration("test.critical");
  assert.ok(declaration);
  const engine = new PermissionEngine();
  const delegated = engine.evaluate({ declaration, target: production, input: { text: "x" }, permissionMode: "auto" });
  assert.equal(delegated.outcome, "ask");
  assert.equal(delegated.approvedBy, undefined);

  const result = await registry.execute(
    { callId: "c", traceId: "t", toolName: "test.critical", input: { text: "x" }, target: production },
    { kind: "agent_auto", decision: { ...delegated, outcome: "auto", approvedBy: "agent" } },
  );
  assert.equal(result.error?.code, "denied_by_policy");

  const userApproved = await registry.execute(
    { callId: "c_user", traceId: "t", toolName: "test.critical", input: { text: "x" }, target: production },
    { kind: "user_approved", decision: delegated, approvalId: delegated.approvalId ?? "", respondedAt: new Date().toISOString() },
  );
  assert.equal(userApproved.status, "success");
});

test("Agent auto tickets cannot be forged for a production write", async () => {
  const registry = new ToolRegistry();
  registry.register(echoTool({ name: "test.write", risk: "medium" }));
  const production: ToolTarget = { host: "remote", serverId: "srv_production", environment: "production" };
  const declaration = registry.declaration("test.write");
  assert.ok(declaration);
  const decision = new PermissionEngine().evaluate({ declaration, target: production, input: { text: "x" }, permissionMode: "auto" });
  assert.equal(decision.outcome, "ask");

  const result = await registry.execute(
    { callId: "c", traceId: "t", toolName: "test.write", input: { text: "x" }, target: production },
    { kind: "agent_auto", decision: { ...decision, outcome: "auto", approvedBy: "agent" } },
  );
  assert.equal(result.error?.code, "denied_by_policy");
});

test("a ticket for one server cannot be replayed on another", async () => {
  const registry = new ToolRegistry();
  registry.register(echoTool({ name: "test.write", risk: "medium" }));
  const staging: ToolTarget = { host: "remote", serverId: "srv_staging", environment: "staging" };
  const production: ToolTarget = { host: "remote", serverId: "srv_production", environment: "production" };
  const ticket = autoTicket(registry, "test.write", { text: "x" }, staging);

  const result = await registry.execute(
    { callId: "c", traceId: "t", toolName: "test.write", input: { text: "x" }, target: production },
    ticket,
  );
  assert.equal(result.error?.code, "denied_by_policy");
  assert.match(result.error?.message ?? "", /srv_staging.*srv_production|targets/);
});

/**
 * Regression: "approve for this run" used to auto-DENY the next identical call.
 *
 * The engine flipped the outcome to `auto` when a grant matched but left
 * `approvedBy` undefined, so the loop built a `policy_auto` ticket and the
 * registry rejected it as unauthorised. The engine's own tests only asserted
 * `outcome`, never provenance, so the broken pairing shipped. This test walks
 * the whole path — evaluate, grant, re-evaluate, ticket, execute — which is the
 * path the product actually takes.
 */
test("a session grant authorises the next identical call end to end", async () => {
  const registry = new ToolRegistry();
  registry.register(echoTool({ name: "test.write", risk: "medium" }));
  const staging: ToolTarget = { host: "remote", serverId: "srv_stg01", environment: "staging" };
  const declaration = registry.declaration("test.write");
  assert.ok(declaration);

  const engine = new PermissionEngine();
  // "ask" mode pauses before every state-changing call, even for a target the
  // environment policy would auto-approve, so this is the case a session grant
  // exists to relieve.
  const request = { declaration, target: staging, input: { text: "x" }, permissionMode: "ask" as const };

  const first = engine.evaluate(request);
  assert.equal(first.outcome, "ask", "an unapproved write must wait for the user");
  engine.grantSession(first);

  const second = engine.evaluate(request);
  assert.equal(second.outcome, "auto");
  assert.equal(second.approvedBy, "user", "the grant must be attributed to the user, not left unset");

  // Reproduce the runtime's ticket selection exactly.
  const ticket: ExecutionTicket = { kind: "session_auto", decision: second };
  const result = await registry.execute(
    { callId: "c", traceId: "t", toolName: "test.write", input: { text: "x" }, target: staging },
    ticket,
  );
  assert.equal(result.status, "success");
  assert.equal(result.error, undefined);
});

/**
 * The engine and the execution chokepoint must agree about a dangerous-tier call.
 *
 * These used to disagree: `evaluate` honoured a session grant for a routine write
 * on a production target (escalated to the dangerous tier) and reported
 * `auto`/`user`, while `checkTicket` refuses every dangerous-tier call that is not
 * a per-call `user_approved` ticket. The engine advertised an approval that
 * execution always denied. The engine now judges the grant on the final tier, so
 * this walks the whole path and asserts both halves say the same thing.
 */
test("a session grant on a dangerous-tier target asks, and the engine says so too", async () => {
  const registry = new ToolRegistry();
  // medium intrinsic risk, escalated to the dangerous tier by the environment.
  registry.register(echoTool({ name: "test.write", risk: "medium" }));
  const production: ToolTarget = { host: "remote", serverId: "srv_live01", environment: "production" };
  const declaration = registry.declaration("test.write");
  assert.ok(declaration);

  const engine = new PermissionEngine();
  const request = { declaration, target: production, input: { text: "x" } };

  const first = engine.evaluate(request);
  assert.equal(first.tier, "dangerous", "production escalation is what makes this dangerous");

  engine.grantSession(first);
  assert.equal(engine.grantCount, 0, "a dangerous-tier decision is never recorded as granted");

  const second = engine.evaluate(request);
  assert.equal(second.outcome, "ask", "the engine must not advertise an approval execution would refuse");
  assert.equal(second.approvedBy, undefined);

  // And if a caller forged a session_auto ticket anyway, the chokepoint still refuses.
  const forged = await registry.execute(
    { callId: "c", traceId: "t", toolName: "test.write", input: { text: "x" }, target: production },
    { kind: "session_auto", decision: { ...second, outcome: "auto", approvedBy: "user" } },
  );
  assert.equal(forged.status, "failed");
  assert.equal(forged.error?.code, "denied_by_policy");
  assert.match(forged.error?.message ?? "", /explicit user approval/);
});

test("a session grant never covers an intrinsically dangerous tool", () => {
  const registry = new ToolRegistry();
  registry.register(echoTool({ name: "test.danger", risk: "high" }));
  const local: ToolTarget = { host: "local", environment: "development" };
  const declaration = registry.declaration("test.danger");
  assert.ok(declaration);

  const engine = new PermissionEngine();
  const request = { declaration, target: local, input: { text: "x" } };

  engine.grantSession(engine.evaluate(request));
  const second = engine.evaluate(request);
  assert.equal(second.outcome, "ask", "a dangerous tool must re-ask every time");
  assert.equal(second.approvedBy, undefined);
});

test("clearing grants makes the next call ask again", () => {
  const registry = new ToolRegistry();
  registry.register(echoTool({ name: "test.write", risk: "medium" }));
  const staging: ToolTarget = { host: "remote", serverId: "srv_stg01", environment: "staging" };
  const declaration = registry.declaration("test.write");
  assert.ok(declaration);

  const engine = new PermissionEngine();
  const request = { declaration, target: staging, input: { text: "x" }, permissionMode: "ask" as const };

  engine.grantSession(engine.evaluate(request));
  assert.equal(engine.evaluate(request).outcome, "auto");
  assert.equal(engine.grantCount, 1);

  engine.clearGrants();
  assert.equal(engine.grantCount, 0);
  assert.equal(engine.evaluate(request).outcome, "ask", "a cleared grant must not survive into a later run");
});

test("a tool that overruns its timeout is failed with code timeout", async () => {
  const registry = new ToolRegistry();
  const schema = z.object({});
  registry.register({
    name: "test.hang",
    description: "Hangs forever",
    risk: "read",
    timeoutMs: 40,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    input: schema,
    execute: (_input, context) =>
      new Promise((resolve) => {
        const timer = setTimeout(() => resolve({}), 5_000);
        context.signal.addEventListener("abort", () => clearTimeout(timer));
      }),
  });

  const result = await registry.execute(
    { callId: "c", traceId: "t", toolName: "test.hang", input: {}, target: local },
    autoTicket(registry, "test.hang", {}),
  );
  assert.equal(result.status, "failed");
  assert.equal(result.error?.code, "timeout");
});

test("user cancellation stops the call and is reported as cancelled", async () => {
  const registry = new ToolRegistry();
  const controller = new AbortController();
  const schema = z.object({});
  registry.register({
    name: "test.slow",
    description: "Cancellable work",
    risk: "read",
    timeoutMs: 5_000,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    input: schema,
    execute: (_input, context) =>
      new Promise((resolve, reject) => {
        const timer = setTimeout(() => resolve({}), 5_000);
        context.signal.addEventListener("abort", () => {
          clearTimeout(timer);
          reject(new ToolFailure("aborted", "cancelled", false));
        });
      }),
  });

  const pending = registry.execute(
    { callId: "c", traceId: "t", toolName: "test.slow", input: {}, target: local },
    autoTicket(registry, "test.slow", {}),
    { signal: controller.signal },
  );
  setTimeout(() => controller.abort(), 20);

  const result = await pending;
  assert.equal(result.status, "cancelled");
  assert.equal(result.error?.code, "cancelled");
});

test("retryable failures are retried up to the declared budget", async () => {
  const registry = new ToolRegistry();
  let attempts = 0;
  registry.register(
    echoTool({
      name: "test.flaky",
      timeoutMs: 2_000,
      retry: { maxAttempts: 3, backoffMs: 1 },
      async execute() {
        attempts += 1;
        if (attempts < 3) throw new ToolFailure("transient", "transport", true);
        return { text: "finally" };
      },
    }),
  );

  const result = await registry.execute(
    { callId: "c", traceId: "t", toolName: "test.flaky", input: { text: "x" }, target: local },
    autoTicket(registry, "test.flaky", { text: "x" }),
  );
  assert.equal(attempts, 3);
  assert.equal(result.status, "success");
});

test("the declaration type export stays in sync with the registry list", () => {
  const registry = new ToolRegistry();
  registry.register(echoTool());
  const [declaration]: ToolDeclaration[] = registry.list();
  assert.equal(declaration?.origin.kind, "builtin");
});
