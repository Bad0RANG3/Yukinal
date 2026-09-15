import assert from "node:assert/strict";
import test from "node:test";
import { z } from "zod";

import { DEVELOPMENT_POLICY, PRODUCTION_POLICY, RPC_ERROR, type AgentStreamEvent } from "@yukinal/shared";
import type { LLMProvider } from "@yukinal/provider-sdk";

import { createEmptyContextSource } from "../context/empty-source.js";
import { ContextEngine } from "../context/context-engine.js";
import { RpcFailure } from "../errors.js";
import { mcpToolFromCatalog } from "../mcp/tool.js";
import { PermissionEngine } from "../permissions/permission-engine.js";
import { ToolRegistry } from "../tools/registry.js";
import { AgentLoop } from "./agent-loop.js";
import { createRuntime } from "./create-runtime.js";
import type { AgentLogger } from "../config.js";

const noop = (): void => {};
const silent: AgentLogger = { debug: noop, info: noop, warn: noop, error: noop, child: () => silent };

test("without a provider the loop refuses to run instead of faking output", async () => {
  const runtime = createRuntime({ log: silent });
  const loop = new AgentLoop(runtime.loop.deps);
  await assert.rejects(
    loop.start(
      {
        runId: "run_1",
        sessionId: "ses_1",
        prompt: "check the api",
        target: { host: "remote", serverId: "srv_1", environment: "staging" },
      },
      { emit: noop },
      undefined as never,
    ),
    (error: unknown) => error instanceof RpcFailure && error.code === RPC_ERROR.NOT_IMPLEMENTED,
  );
});

test("provider text, reasoning and cumulative usage survive the loop", async () => {
  const registry = new ToolRegistry();
  registry.register({
    name: "test.read",
    description: "Read test data",
    risk: "read",
    timeoutMs: 1_000,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    input: z.strictObject({}),
    execute: async () => ({ ok: true }),
  });
  const loop = new AgentLoop({
    registry,
    permission: new PermissionEngine(),
    context: new ContextEngine(createEmptyContextSource()),
  });

  let calls = 0;
  const provider: LLMProvider = {
    id: "test-provider",
    model: "test-model",
    async listModels() {
      return [];
    },
    async *stream() {
      calls += 1;
      if (calls === 1) {
        yield { type: "usage", inputTokens: 5, outputTokens: 2 };
        yield { type: "tool_call", call: { id: "call_read", name: "test__read", arguments: {} } };
        yield { type: "done", finishReason: "tool_calls" };
        return;
      }
      yield { type: "reasoning_delta", text: "reasoning" };
      yield { type: "text_delta", text: "answer" };
      yield { type: "usage", inputTokens: 7, outputTokens: 3 };
      yield { type: "done", finishReason: "stop" };
    },
  };

  const events: AgentStreamEvent[] = [];
  const result = await loop.start(
    {
      runId: "run_stream_contract",
      sessionId: "ses_stream_contract",
      prompt: "answer after reading",
      target: { host: "local", environment: "local" },
    },
    { emit: (event) => events.push(event) },
    provider,
  );

  assert.equal(result.state, "completed", JSON.stringify(result));
  const textEvents = events.filter(
    (event): event is Extract<AgentStreamEvent, { type: "agent.text" }> => event.type === "agent.text",
  );
  const reasoningEvents = events.filter(
    (event): event is Extract<AgentStreamEvent, { type: "agent.thinking" }> => event.type === "agent.thinking",
  );
  const usageEvents = events.filter(
    (event): event is Extract<AgentStreamEvent, { type: "agent.usage" }> => event.type === "agent.usage",
  );
  assert.equal(textEvents.map((event) => event.textDelta).join(""), "answer");
  assert.equal(reasoningEvents.map((event) => event.textDelta).join(""), "reasoning");
  assert.deepEqual(
    usageEvents.map((event) => event.usage),
    [{ inputTokens: 5, outputTokens: 2 }, { inputTokens: 12, outputTokens: 5 }],
  );
});

test("image-only prompt parts reach the provider as structured image input", async () => {
  const loop = new AgentLoop({
    registry: new ToolRegistry(),
    permission: new PermissionEngine(),
    context: new ContextEngine(createEmptyContextSource()),
  });
  let seenMessages: Parameters<LLMProvider["stream"]>[0]["messages"] = [];
  const provider: LLMProvider = {
    id: "test-provider",
    model: "vision-model",
    async listModels() {
      return [];
    },
    async *stream(request) {
      seenMessages = request.messages;
      yield { type: "text_delta", text: "a screenshot" };
      yield { type: "done", finishReason: "stop" };
    },
  };

  const result = await loop.start(
    {
      runId: "run_image_only",
      sessionId: "ses_image_only",
      prompt: "",
      parts: [
        {
          type: "image",
          mediaType: "image/png",
          data: "aGVsbG8=",
          name: "screen.png",
        },
      ],
      target: { host: "local", environment: "local" },
    },
    { emit: noop },
    provider,
  );

  assert.equal(result.state, "completed");
  assert.equal(seenMessages[0]?.role, "system");
  assert.match(
    seenMessages[0]?.role === "system" ? seenMessages[0].content : "",
    /Image attachment/,
  );
  assert.deepEqual(seenMessages.slice(1), [
    {
      role: "user",
      content: "",
      images: [{ mediaType: "image/png", data: "aGVsbG8=", name: "screen.png" }],
    },
  ]);
});

test("PDF-only prompt parts reach the provider as structured document input", async () => {
  const loop = new AgentLoop({
    registry: new ToolRegistry(),
    permission: new PermissionEngine(),
    context: new ContextEngine(createEmptyContextSource()),
  });
  let seenMessages: Parameters<LLMProvider["stream"]>[0]["messages"] = [];
  const provider: LLMProvider = {
    id: "test-provider",
    model: "document-model",
    async listModels() {
      return [];
    },
    async *stream(request) {
      seenMessages = request.messages;
      yield { type: "text_delta", text: "read the document" };
      yield { type: "done", finishReason: "stop" };
    },
  };

  const result = await loop.start(
    {
      runId: "run_document_only",
      sessionId: "ses_document_only",
      prompt: "",
      parts: [
        {
          type: "document",
          mediaType: "application/pdf",
          data: "JVBERi0xLjcK",
          name: "guide.pdf",
        },
      ],
      target: { host: "local", environment: "local" },
    },
    { emit: noop },
    provider,
  );

  assert.equal(result.state, "completed");
  assert.deepEqual(seenMessages.slice(1), [
    {
      role: "user",
      content: "",
      documents: [
        {
          mediaType: "application/pdf",
          data: "JVBERi0xLjcK",
          name: "guide.pdf",
        },
      ],
    },
  ]);
});

test("text-file-only prompt parts reach the provider as a delimited text block", async () => {
  const loop = new AgentLoop({
    registry: new ToolRegistry(),
    permission: new PermissionEngine(),
    context: new ContextEngine(createEmptyContextSource()),
  });
  let seenMessages: Parameters<LLMProvider["stream"]>[0]["messages"] = [];
  const provider: LLMProvider = {
    id: "test-provider",
    model: "text-model",
    async listModels() {
      return [];
    },
    async *stream(request) {
      seenMessages = request.messages;
      yield { type: "text_delta", text: "read it" };
      yield { type: "done", finishReason: "stop" };
    },
  };

  const result = await loop.start(
    {
      runId: "run_file_only",
      sessionId: "ses_file_only",
      prompt: "",
      parts: [
        {
          type: "file",
          mediaType: "text/plain",
          data: "PORT=8080\n",
          name: "app.env",
        },
      ],
      target: { host: "local", environment: "local" },
    },
    { emit: noop },
    provider,
  );

  assert.equal(result.state, "completed");
  assert.deepEqual(seenMessages.slice(1), [
    {
      role: "user",
      content:
        "--- BEGIN ATTACHED TEXT FILE: app.env ---\nPORT=8080\n\n--- END ATTACHED TEXT FILE ---",
    },
  ]);
});

test("approval responses are bound to the run that displayed them", async () => {
  const registry = new ToolRegistry();
  registry.register({
    name: "danger.test",
    description: "Test action requiring approval",
    risk: "high",
    timeoutMs: 1_000,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    input: z.strictObject({}),
    execute: async () => ({ ok: true }),
  });
  const loop = new AgentLoop({
    registry,
    permission: new PermissionEngine(),
    context: new ContextEngine(createEmptyContextSource()),
  });

  let calls = 0;
  const provider: LLMProvider = {
    id: "test-provider",
    model: "test-model",
    async listModels() {
      return [];
    },
    async *stream() {
      calls += 1;
      if (calls === 1) {
        yield { type: "tool_call", call: { id: "call_approval", name: "danger__test", arguments: {} } };
        yield { type: "done", finishReason: "tool_calls" };
      } else {
        yield { type: "text_delta", text: "approved" };
        yield { type: "done", finishReason: "stop" };
      }
    },
  };

  let resolveApproval: ((approval: Extract<AgentStreamEvent, { type: "agent.waiting_approval" }>['approval']) => void) | undefined;
  const approval = new Promise<Extract<AgentStreamEvent, { type: "agent.waiting_approval" }>['approval']>((resolve) => {
    resolveApproval = resolve;
  });
  const run = loop.start(
    {
      runId: "run_approval",
      sessionId: "ses_approval",
      prompt: "run the protected test action",
      target: { host: "remote", serverId: "srv_approval", environment: "production" },
    },
    {
      emit: (event) => {
        if (event.type === "agent.waiting_approval") resolveApproval?.(event.approval);
      },
    },
    provider,
  );

  const request = await approval;
  const response = { approvalId: request.approvalId, decision: "approve_once" as const, respondedAt: new Date().toISOString() };
  assert.equal(loop.respondApproval({ ...response, runId: "run_other" }), false);
  assert.equal(loop.respondApproval({ ...response, runId: "run_approval" }), true);
  const result = await run;
  assert.equal(result.state, "completed", JSON.stringify(result));
  assert.equal(result.toolCalls, 1);
});

test("auto mode cannot self-approve a dangerous production tool", async () => {
  const registry = new ToolRegistry();
  registry.register({
    name: "danger.test",
    description: "Test action requiring direct approval",
    risk: "high",
    timeoutMs: 1_000,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    input: z.strictObject({}),
    execute: async () => ({ ok: true }),
  });
  const loop = new AgentLoop({
    registry,
    permission: new PermissionEngine(),
    context: new ContextEngine(createEmptyContextSource()),
  });

  let calls = 0;
  const provider: LLMProvider = {
    id: "test-provider",
    model: "test-model",
    async listModels() {
      return [];
    },
    async *stream() {
      calls += 1;
      if (calls === 1) {
        yield { type: "tool_call", call: { id: "call_agent_auto", name: "danger__test", arguments: {} } };
        yield { type: "done", finishReason: "tool_calls" };
      } else {
        yield { type: "text_delta", text: "已完成委托执行" };
        yield { type: "done", finishReason: "stop" };
      }
    },
  };

  const events: AgentStreamEvent[] = [];
  let resolveApproval: ((approval: Extract<AgentStreamEvent, { type: "agent.waiting_approval" }>['approval']) => void) | undefined;
  const approval = new Promise<Extract<AgentStreamEvent, { type: "agent.waiting_approval" }>['approval']>((resolve) => {
    resolveApproval = resolve;
  });
  const run = loop.start(
    {
      runId: "run_agent_auto",
      sessionId: "ses_agent_auto",
      prompt: "run the delegated test action",
      permissionMode: "auto",
      target: { host: "remote", serverId: "srv_agent_auto", environment: "production" },
    },
    {
      emit: (event) => {
        events.push(event);
        if (event.type === "agent.waiting_approval") resolveApproval?.(event.approval);
      },
    },
    provider,
  );

  const request = await approval;
  assert.equal(loop.respondApproval({ approvalId: request.approvalId, runId: "run_agent_auto", decision: "approve_once", respondedAt: new Date().toISOString() }), true);
  const result = await run;
  assert.equal(result.state, "completed", JSON.stringify(result));
  assert.equal(result.toolCalls, 1);
  assert.equal(events.some((event) => event.type === "agent.waiting_approval"), true);
  const toolResult = events.find((event) => event.type === "agent.tool_result");
  assert(toolResult && toolResult.type === "agent.tool_result");
  assert.equal(toolResult.approvedBy, "user");
  assert.equal(toolResult.status, "success");
});

test("approval expiry is streamed and does not leave a live approval", async () => {
  const registry = new ToolRegistry();
  registry.register({
    name: "danger.test",
    description: "Test action requiring approval",
    risk: "high",
    timeoutMs: 1_000,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    input: z.strictObject({}),
    execute: async () => ({ ok: true }),
  });
  const loop = new AgentLoop({
    registry,
    permission: new PermissionEngine(),
    context: new ContextEngine(createEmptyContextSource()),
    approvalTtlMs: 10,
  });

  let calls = 0;
  const provider: LLMProvider = {
    id: "test-provider",
    model: "test-model",
    async listModels() {
      return [];
    },
    async *stream() {
      calls += 1;
      if (calls === 1) {
        yield { type: "tool_call", call: { id: "call_expiry", name: "danger__test", arguments: {} } };
        yield { type: "done", finishReason: "tool_calls" };
      } else {
        yield { type: "text_delta", text: "continued after expiry" };
        yield { type: "done", finishReason: "stop" };
      }
    },
  };

  const events: AgentStreamEvent[] = [];
  const result = await loop.start(
    {
      runId: "run_expiry",
      sessionId: "ses_expiry",
      prompt: "run the protected test action",
      target: { host: "remote", serverId: "srv_expiry", environment: "production" },
    },
    { emit: (event) => events.push(event) },
    provider,
  );

  assert.equal(result.state, "completed", JSON.stringify(result));
  assert.equal(events.some((event) => event.type === "agent.approval_expired" && event.approvalId.length > 0), true);
  const rejected = events.find((event) => event.type === "agent.tool_result");
  assert(rejected && rejected.type === "agent.tool_result");
  assert.equal(rejected.outputSummary, "审批已过期");
  assert.match(result.text, /continued after expiry/);
});

/** One tool-calling turn followed by a text turn; a fresh instance per run. */
function writeThenAnswer(): LLMProvider {
  let turns = 0;
  return {
    id: "test-provider",
    model: "test-model",
    async listModels() {
      return [];
    },
    async *stream() {
      turns += 1;
      if (turns === 1) {
        yield { type: "tool_call", call: { id: "call_write", name: "write__test", arguments: {} } };
        yield { type: "done", finishReason: "tool_calls" };
      } else {
        yield { type: "text_delta", text: "写入已处理" };
        yield { type: "done", finishReason: "stop" };
      }
    },
  };
}

/**
 * `policyId` is honoured per call, and it is visible on the stream.
 *
 * Same tool declaration, same development target, same run mode — the only difference
 * between the two runs below is the policy the caller named. The environment default for
 * `development` auto-approves an ordinary write; `policy.production` asks. Asserting on
 * the emitted `agent.tool_call` is the whole path rather than the engine alone: it is
 * what the UI renders and what the audit row is written from.
 */
test("a requested policy decides the run, and the event stream says which one", async () => {
  const registry = new ToolRegistry();
  registry.register({
    name: "write.test",
    description: "Test write",
    risk: "medium",
    timeoutMs: 1_000,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    input: z.strictObject({}),
    execute: async () => ({ ok: true }),
  });
  const loop = new AgentLoop({
    registry,
    permission: new PermissionEngine(),
    context: new ContextEngine(createEmptyContextSource()),
  });
  const request = {
    sessionId: "ses_policy",
    prompt: "write the config file",
    target: { host: "remote" as const, serverId: "srv_dev", environment: "development" as const },
  };

  // No policy named: the engine's environment default applies, as it always has.
  const defaultEvents: AgentStreamEvent[] = [];
  const defaultRun = await loop.start(
    { ...request, runId: "run_env_default" },
    { emit: (event) => defaultEvents.push(event) },
    writeThenAnswer(),
  );
  assert.equal(defaultRun.state, "completed", JSON.stringify(defaultRun));
  const defaultCall = defaultEvents.find((event) => event.type === "agent.tool_call");
  assert(defaultCall && defaultCall.type === "agent.tool_call");
  assert.equal(defaultCall.policyId, DEVELOPMENT_POLICY.id);
  assert.equal(defaultCall.decision, "auto");

  // The same call, under the policy the caller named instead.
  const overrideEvents: AgentStreamEvent[] = [];
  let resolveApproval: ((approval: Extract<AgentStreamEvent, { type: "agent.waiting_approval" }>["approval"]) => void) | undefined;
  const approval = new Promise<Extract<AgentStreamEvent, { type: "agent.waiting_approval" }>["approval"]>((resolve) => {
    resolveApproval = resolve;
  });
  const override = loop.start(
    { ...request, runId: "run_policy_override", policyId: PRODUCTION_POLICY.id },
    {
      emit: (event) => {
        overrideEvents.push(event);
        if (event.type === "agent.waiting_approval") resolveApproval?.(event.approval);
      },
    },
    writeThenAnswer(),
  );

  const pending = await approval;
  const overrideCall = overrideEvents.find((event) => event.type === "agent.tool_call");
  assert(overrideCall && overrideCall.type === "agent.tool_call");
  assert.equal(overrideCall.policyId, PRODUCTION_POLICY.id);
  // The development default said `auto` for this very call; asking under production must
  // change the decision, not merely the label on it.
  assert.equal(overrideCall.decision, "ask");
  assert.equal(overrideCall.riskLevel, defaultCall.riskLevel);

  assert.equal(
    loop.respondApproval({ approvalId: pending.approvalId, runId: "run_policy_override", decision: "approve_once", respondedAt: new Date().toISOString() }),
    true,
  );
  const overrideRun = await override;
  assert.equal(overrideRun.state, "completed", JSON.stringify(overrideRun));
  const overrideResult = overrideEvents.find((event) => event.type === "agent.tool_result");
  assert(overrideResult && overrideResult.type === "agent.tool_result");
  assert.equal(overrideResult.policyId, PRODUCTION_POLICY.id);
  assert.equal(overrideResult.status, "success");
});

test("an unknown policyId fails the run before it touches the provider", async () => {
  const loop = new AgentLoop({
    registry: new ToolRegistry(),
    permission: new PermissionEngine(),
    context: new ContextEngine(createEmptyContextSource()),
  });
  let streamed = 0;
  const provider: LLMProvider = {
    id: "test-provider",
    model: "test-model",
    async listModels() {
      return [];
    },
    async *stream() {
      streamed += 1;
      yield { type: "done", finishReason: "stop" };
    },
  };
  const events: AgentStreamEvent[] = [];

  await assert.rejects(
    loop.start(
      {
        runId: "run_unknown_policy",
        sessionId: "ses_unknown_policy",
        prompt: "check the api",
        policyId: "policy.terraform",
      },
      { emit: (event) => events.push(event) },
      provider,
    ),
    (error: unknown) => {
      assert.ok(error instanceof RpcFailure, String(error));
      assert.equal(error.code, RPC_ERROR.INVALID_PARAMS);
      assert.match(error.message, /policy\.terraform/);
      return true;
    },
  );

  // "Before any provider call" is the point: no `agent.started`, no request, nothing the
  // caller could mistake for a run that had begun.
  assert.deepEqual(events, []);
  assert.equal(streamed, 0);
  assert.equal(loop.pendingApprovals.length, 0);
});

/**
 * 审计里能分辨 MCP 与内置工具（ADR 0014）。
 *
 * 宿主把 `agent.tool_result` 落成 `tool_executions` 那一行，所以「这次调用是不是某个第三方
 * 服务器声明的工具」必须在**这个事件**里可读。两条证据同时断言：事件里的结构化 `origin`
 * （`{kind: "mcp", serverId}`）与工具名里的 `mcp.` 段 —— 后者是今天就已经写在审计行上的东西
 * （`tool_name` 列），前者是给未来那条「把 origin 也落库」的迁移准备的。只看名称前缀是不够的：
 * 一个内置工具永远有可能被起名叫 `mcp.something`，而注册表已经拒绝这种冒充。
 */
test("an MCP call is emitted with its server as origin", async () => {
  const registry = new ToolRegistry();
  const target = { host: "local" as const, environment: "unknown" as const };
  registry.register(
    mcpToolFromCatalog(
      {
        name: "mcp.mcp-1.echo",
        serverId: "mcp_1",
        tool: "echo",
        description: "Echo text back.",
        inputSchema: { type: "object" },
      },
      {
        executeOnHost: async () => ({
          status: "success",
          output: { serverId: "mcp_1", tool: "echo", isError: false, text: "echo: hi", content: [] },
        }),
      },
    ),
  );

  const loop = new AgentLoop({
    registry,
    permission: new PermissionEngine(),
    context: new ContextEngine(createEmptyContextSource()),
  });

  let calls = 0;
  const provider: LLMProvider = {
    id: "test-provider",
    model: "test-model",
    async listModels() {
      return [];
    },
    async *stream() {
      calls += 1;
      if (calls === 1) {
        yield { type: "tool_call", call: { id: "call_mcp", name: "mcp__mcp-1__echo", arguments: { text: "hi" } } };
        yield { type: "done", finishReason: "tool_calls" };
      } else {
        yield { type: "text_delta", text: "done" };
        yield { type: "done", finishReason: "stop" };
      }
    },
  };

  const events: AgentStreamEvent[] = [];
  let resolveApproval: ((approval: Extract<AgentStreamEvent, { type: "agent.waiting_approval" }>["approval"]) => void) | undefined;
  const approval = new Promise<Extract<AgentStreamEvent, { type: "agent.waiting_approval" }>["approval"]>((resolve) => {
    resolveApproval = resolve;
  });

  const run = loop.start(
    {
      runId: "run_mcp",
      sessionId: "ses_mcp",
      prompt: "echo something",
      target,
    },
    {
      emit: (event) => {
        events.push(event);
        if (event.type === "agent.waiting_approval") resolveApproval?.(event.approval);
      },
    },
    provider,
  );

  // 第三方工具永远要用户批准：这一条在 catalog.test.ts 里从权限引擎那一侧也钉了一次。
  const request = await approval;
  assert.equal(loop.respondApproval({ approvalId: request.approvalId, runId: "run_mcp", decision: "approve_once", respondedAt: new Date().toISOString() }), true);
  const result = await run;
  assert.equal(result.state, "completed", JSON.stringify(result));

  const call = events.find((event) => event.type === "agent.tool_call");
  const toolResult = events.find((event) => event.type === "agent.tool_result");
  assert.ok(call && call.type === "agent.tool_call");
  assert.ok(toolResult && toolResult.type === "agent.tool_result");
  assert.deepEqual(call.origin, { kind: "mcp", serverId: "mcp_1" });
  assert.deepEqual(toolResult.origin, { kind: "mcp", serverId: "mcp_1" });
  assert.equal(call.toolName, "mcp.mcp-1.echo", "审计行上的名字必须带得出服务器段");
  assert.equal(call.toolName.startsWith("mcp."), true);
});

