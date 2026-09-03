import assert from "node:assert/strict";
import test from "node:test";

import { z } from "zod";

import type { PermissionDecision, ToolDeclaration, ToolTarget } from "@yukinal/shared";

import { PermissionEngine } from "../permissions/permission-engine.js";
import { TraceRecorder } from "../trace/trace-recorder.js";
import { ToolRegistry, type ExecutionTicket } from "./registry.js";
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
  assert.match(result.error?.message ?? "", /policy_auto|"ask"/);
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
