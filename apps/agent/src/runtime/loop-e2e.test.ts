/**
 * Vertical slice E2E: a real OpenAI-compatible HTTP endpoint (local mock server,
 * same wire format as OpenAI/OpenRouter/Ollama) driving the whole loop.
 *
 * The mock replaces only the *external* LLM; everything else — provider client,
 * name mapping (ADR 0004), permission engine, registry timeout/trace, loop
 * turn-taking — is the production code path. This is the "UI → agent → tool →
 * result" chain proven in CI without a paid key.
 */

import assert from "node:assert/strict";
import { createServer, type Server } from "node:http";
import test from "node:test";

import type { AgentRunRequest, AgentStreamEvent } from "@yukinal/shared";

import type { AgentLogger } from "../config.js";
import { OpenAiCompatibleProvider } from "../providers/openai-compatible.js";
import { TraceRecorder } from "../trace/trace-recorder.js";
import { HostRpcClient } from "../transport/host-client.js";
import { createRuntime } from "./create-runtime.js";

const noop = (): void => {};
const silent: AgentLogger = { debug: noop, info: noop, warn: noop, error: noop, child: () => silent };

/** 按请求顺序回放脚本的 SSE 服务器。测试结束后必须 close，否则进程不退出。 */
function mockLlm(script: Array<Array<object> | "hang">, seenRequests: string[] = []): Promise<{ port: number; close(): void }> {
  return new Promise((resolve) => {
    let index = 0;
    const server: Server = createServer(async (request, res) => {
      let body = "";
      for await (const chunk of request) body += chunk.toString();
      seenRequests.push(body);
      const step = script[index] ?? [];
      index += 1;
      if (step === "hang") {
        // 永不响应：连接挂住，用来验证 Stop 真的掐断了在途请求。
        res.writeHead(200, { "content-type": "text/event-stream" });
        return;
      }
      res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
      for (const chunk of step) {
        res.write(`data: ${JSON.stringify(chunk)}\n\n`);
      }
      res.write("data: [DONE]\n\n");
      res.end();
    });
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      const port = typeof address === "object" && address ? address.port : 0;
      resolve({ port, close: () => server.close() });
    });
  });
}

function mockProviderFailure(body: string, status = 401): Promise<{ port: number; close(): void }> {
  return new Promise((resolve) => {
    const server: Server = createServer((_req, res) => {
      res.writeHead(status, { "content-type": "application/json" });
      res.end(body);
    });
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      const port = typeof address === "object" && address ? address.port : 0;
      resolve({ port, close: () => server.close() });
    });
  });
}

function sseText(text: string): object {
  return { choices: [{ delta: { content: text }, finish_reason: null }] };
}

function sseToolCall(delta: object): object {
  return { choices: [{ delta: { tool_calls: [delta] }, finish_reason: null }] };
}

function runRequest(overrides: Partial<AgentRunRequest> = {}): AgentRunRequest {
  return {
    runId: "run_e2e",
    sessionId: "ses_e2e",
    prompt: "检查 staging API（echo 你好）",
    target: { host: "remote", serverId: "srv_e2e", environment: "staging" },
    ...overrides,
  };
}

test("E2E: prompt -> tool call -> permission -> execute -> report", async (t) => {
  const { port, close } = await mockLlm([
    [
      sseToolCall({ index: 0, type: "function", id: "call_1", function: { name: "system__echo", arguments: '{"message":"hello from mock"}' } }),
    ],
    [sseText("mock answer to echo")],
  ]);

  t.after(() => close());
  const runtime = createRuntime({ log: silent });
  const events: AgentStreamEvent[] = [];

  const result = await runtime.loop.start(
    runRequest(),
    { emit: (event) => events.push(event) },
    new OpenAiCompatibleProvider({ baseUrl: `http://127.0.0.1:${port}`, model: "mock-model" }),
  );

  assert.equal(result.state, "completed", JSON.stringify(result));
  assert.equal(result.toolCalls, 1);
  assert.equal(result.steps, 1); // steps = 已完整跑完的轮次（回答轮是终止轮，不计数）
  assert.match(result.text, /mock answer to echo/);

  const types = events.map((event) => event.type);
  assert(types.includes("agent.started"), `${types.join(",")}`);
  const toolCall = events.find((event) => event.type === "agent.tool_call");
  assert(toolCall && toolCall.type === "agent.tool_call", "a tool call must be visible");
  assert.equal(toolCall.toolName, "system.echo"); // 内部 dot 名回填（ADR 0004）
  assert.equal(toolCall.callId, "call_1");
  assert.deepEqual(toolCall.target, runRequest().target);
  assert.equal(toolCall.riskLevel, "read");
  assert.equal(toolCall.decision, "auto");
  const toolResult = events.find((event) => event.type === "agent.tool_result");
  assert(toolResult && toolResult.type === "agent.tool_result");
  assert.equal(toolResult.callId, "call_1");
  assert.equal(toolResult.approvedBy, "policy");
  assert.equal(toolResult.durationMs >= 0, true);
  assert.equal(toolResult.startedAt <= toolResult.endedAt, true);
  assert.equal(toolResult.status, "success");
  assert.match(toolResult.outputSummary, /hello from mock/);
  assert(types.includes("agent.completed"), JSON.stringify(types));
});

test("E2E: durable task budget caps a run below the process default", async (t) => {
  const { port, close } = await mockLlm([
    [sseToolCall({ index: 0, type: "function", id: "call_budget_1", function: { name: "system__echo", arguments: '{"message":"one"}' } })],
    [sseToolCall({ index: 0, type: "function", id: "call_budget_2", function: { name: "system__echo", arguments: '{"message":"two"}' } })],
  ]);
  t.after(() => close());

  const runtime = createRuntime({ log: silent, maxRunMs: 10_000 });
  const result = await runtime.loop.start(
    runRequest({
      runId: "run_budget_override",
      taskId: "task_budget_override",
      taskBudget: { maxSteps: 1, maxRunMs: 10_000, maxAttempts: 3 },
    }),
    { emit: noop },
    new OpenAiCompatibleProvider({ baseUrl: `http://127.0.0.1:${port}`, model: "mock-model" }),
  );

  assert.equal(result.state, "failed", JSON.stringify(result));
  assert.match(result.error ?? "", /maxSteps=1/);
  assert.equal(result.steps, 1);
});

test("E2E: a durable task tool call outside its plan is reported as a deviation", async (t) => {
  const { port, close } = await mockLlm([
    [sseToolCall({ index: 0, type: "function", id: "call_plan_deviation", function: { name: "system__echo", arguments: '{"message":"must not run"}' } })],
    [sseText("我需要先重新规划")],
  ]);
  t.after(() => close());

  const hostClient = new HostRpcClient((frame) => {
    const sent = JSON.parse(frame) as { id: number; method: string };
    if (sent.method === "host.context.fetch") {
      hostClient.handleIncoming({ jsonrpc: "2.0", id: sent.id, result: { status: "not_found" } });
    } else if (sent.method === "host.investigation.plan.check") {
      hostClient.handleIncoming({
        jsonrpc: "2.0",
        id: sent.id,
        result: {
          status: "deviation",
          deviation: {
            code: "missing_plan",
            action: "replan",
            message: "durable task has no active plan",
            toolName: "system.echo",
            at: "2026-09-19T00:00:00.000Z",
          },
        },
      });
    }
  });
  const runtime = createRuntime({ log: silent, hostToolClient: hostClient });
  const events: AgentStreamEvent[] = [];
  const result = await runtime.loop.start(
    runRequest({ runId: "run_plan_deviation", taskId: "task_plan_deviation" }),
    { emit: (event) => events.push(event) },
    new OpenAiCompatibleProvider({ baseUrl: `http://127.0.0.1:${port}`, model: "mock-model" }),
  );

  assert.equal(result.state, "completed", JSON.stringify(result));
  const toolResult = events.find((event) => event.type === "agent.tool_result");
  assert(toolResult && toolResult.type === "agent.tool_result");
  assert.equal(toolResult.errorCode, "plan_deviation");
  assert.equal(toolResult.status, "failed");
});

test("E2E: a durable task can retrieve persisted evidence without consuming the remote plan", async (t) => {
  const { port, close } = await mockLlm([
    [sseToolCall({ index: 0, type: "function", id: "call_evidence_search", function: { name: "investigation__evidence__search", arguments: '{"sourceTool":"server.info","limit":4}' } })],
    [sseToolCall({ index: 0, type: "function", id: "call_evidence_lookup", function: { name: "investigation__evidence", arguments: '{"evidenceId":"ev_existing"}' } })],
    [sseText("已取回原始证据")],
  ]);
  t.after(() => close());

  const methods: string[] = [];
  const evidence = {
    id: "ev_existing",
    taskId: "task_evidence_lookup",
    scope: runRequest().target,
    kind: "snapshot" as const,
    sourceTool: "server.info",
    collectedAt: "2026-09-19T00:00:00.000Z",
    inputSummary: "{}",
    contentType: "json" as const,
    content: { health: "healthy" },
    contentHash: "a".repeat(64),
    truncated: false,
    redactionStatus: "clean" as const,
  };
  const hostClient = new HostRpcClient((frame) => {
    const sent = JSON.parse(frame) as { id: number; method: string; params?: Record<string, unknown> };
    methods.push(sent.method);
    if (sent.method === "host.context.fetch") {
      hostClient.handleIncoming({ jsonrpc: "2.0", id: sent.id, result: { status: "not_found" } });
    } else if (sent.method === "host.investigation.evidence.search") {
      hostClient.handleIncoming({
        jsonrpc: "2.0",
        id: sent.id,
        result: {
          status: "success",
          evidence: [{
            id: evidence.id,
            taskId: evidence.taskId,
            scope: evidence.scope,
            kind: evidence.kind,
            sourceTool: evidence.sourceTool,
            collectedAt: evidence.collectedAt,
            inputSummary: evidence.inputSummary,
            contentType: evidence.contentType,
            contentHash: evidence.contentHash,
            truncated: evidence.truncated,
            redactionStatus: evidence.redactionStatus,
          }],
        },
      });
    } else if (sent.method === "host.investigation.evidence.fetch") {
      hostClient.handleIncoming({ jsonrpc: "2.0", id: sent.id, result: { status: "success", evidence } });
    }
  });
  const runtime = createRuntime({ log: silent, hostToolClient: hostClient });
  const events: AgentStreamEvent[] = [];
  const result = await runtime.loop.start(
    runRequest({ runId: "run_evidence_lookup", taskId: "task_evidence_lookup" }),
    { emit: (event) => events.push(event) },
    new OpenAiCompatibleProvider({ baseUrl: `http://127.0.0.1:${port}`, model: "mock-model" }),
  );

  assert.equal(result.state, "completed", JSON.stringify(result));
  assert.equal(methods.filter((method) => method === "host.investigation.evidence.search").length, 1);
  assert.equal(methods.filter((method) => method === "host.investigation.evidence.fetch").length, 1);
  assert.equal(methods.includes("host.investigation.plan.check"), false);
  assert.equal(methods.includes("host.investigation.plan.step_result"), false);
  assert.equal(methods.includes("host.investigation.evidence.record"), false);
  const toolResult = events.find((event) => event.type === "agent.tool_result");
  assert(toolResult && toolResult.type === "agent.tool_result");
  assert.equal(toolResult.status, "success");
});

test("E2E: local evidence correlation and triage remain available while a plan is active", async (t) => {
  const { port, close } = await mockLlm([
    [sseToolCall({ index: 0, type: "function", id: "call_local_plan", function: { name: "investigation__playbook", arguments: '{"template":"readonly_health"}' } })],
    [sseToolCall({ index: 0, type: "function", id: "call_correlate", function: { name: "investigation__evidence__correlate", arguments: '{"anchorEvidenceId":"ev_local"}' } })],
    [sseToolCall({ index: 0, type: "function", id: "call_triage", function: { name: "investigation__evidence__triage", arguments: '{"evidenceIds":["ev_local"]}' } })],
    [sseText("已在活动计划内完成本地证据对齐与候选整理")],
  ]);
  t.after(() => close());

  const methods: string[] = [];
  const target = runRequest().target!;
  const evidence = {
    id: "ev_local",
    taskId: "task_local_evidence",
    runId: "run_local_evidence",
    scope: target,
    kind: "log" as const,
    sourceTool: "server.logs",
    collectedAt: "2026-09-19T00:00:00.000Z",
    inputSummary: "{}",
    contentType: "json" as const,
    content: { lines: [{ text: "fixture timeout", level: "error" }] },
    contentHash: "b".repeat(64),
    truncated: false,
    redactionStatus: "clean" as const,
  };
  const summary = {
    id: evidence.id,
    taskId: evidence.taskId,
    runId: evidence.runId,
    scope: evidence.scope,
    kind: evidence.kind,
    sourceTool: evidence.sourceTool,
    collectedAt: evidence.collectedAt,
    inputSummary: evidence.inputSummary,
    contentType: evidence.contentType,
    contentHash: evidence.contentHash,
    truncated: evidence.truncated,
    redactionStatus: evidence.redactionStatus,
  };
  const hostClient = new HostRpcClient((frame) => {
    const sent = JSON.parse(frame) as { id: number; method: string; params?: Record<string, unknown> };
    methods.push(sent.method);
    if (sent.method === "host.context.fetch") {
      hostClient.handleIncoming({ jsonrpc: "2.0", id: sent.id, result: { status: "not_found" } });
    } else if (sent.method === "host.investigation.plan.record") {
      hostClient.handleIncoming({ jsonrpc: "2.0", id: sent.id, result: { recorded: true, plan: sent.params?.plan } });
    } else if (sent.method === "host.investigation.evidence.correlate") {
      hostClient.handleIncoming({
        jsonrpc: "2.0",
        id: sent.id,
        result: {
          status: "success",
          correlation: {
            anchor: summary,
            evidence: [summary],
            matchedBy: "same_run",
            windowSeconds: 300,
            sourceTools: ["server.logs"],
            warnings: [],
          },
        },
      });
    } else if (sent.method === "host.investigation.evidence.fetch") {
      hostClient.handleIncoming({ jsonrpc: "2.0", id: sent.id, result: { status: "success", evidence } });
    }
  });

  const runtime = createRuntime({ log: silent, hostToolClient: hostClient });
  const result = await runtime.loop.start(
    runRequest({ runId: "run_local_evidence", taskId: "task_local_evidence" }),
    { emit: () => {} },
    new OpenAiCompatibleProvider({ baseUrl: `http://127.0.0.1:${port}`, model: "mock-model" }),
  );

  assert.equal(result.state, "completed", JSON.stringify(result));
  assert.equal(methods.filter((method) => method === "host.investigation.plan.record").length, 1);
  assert.equal(methods.filter((method) => method === "host.investigation.evidence.correlate").length, 1);
  assert.equal(methods.filter((method) => method === "host.investigation.evidence.fetch").length, 1);
  assert.equal(methods.includes("host.investigation.plan.check"), false);
  assert.equal(methods.includes("host.investigation.plan.step_result"), false);
});

test("E2E: an observation window replays only the final read until the host closes it", async (t) => {
  const { port, close } = await mockLlm([
    [sseToolCall({ index: 0, type: "function", id: "call_observation_initial", function: { name: "server__info", arguments: "{}" } })],
    [sseText("窗口还没有结束，不能提前报告完成")],
    [sseText("观察窗口已完成，验证结果稳定")],
  ]);
  t.after(() => close());

  const taskId = "task_observation_replay";
  const planId = "plan_observation_replay";
  const stepId = "step_observation_verify";
  const methods: string[] = [];
  let stepResultCount = 0;
  const planFor = (windowStatus: "running" | "succeeded", sampleCount: number) => ({
    id: planId,
    taskId,
    revision: 1,
    status: windowStatus === "succeeded" ? "completed" : "active",
    createdAt: "2026-09-20T00:00:00.000Z",
    updatedAt: "2026-09-20T00:00:00.000Z",
    ...(windowStatus === "succeeded" ? {} : { currentStepId: stepId }),
    observationWindow: {
      durationSeconds: 1,
      intervalSeconds: 1,
      allowedTools: ["server.info"],
      successCriteria: ["server.info remains healthy"],
      status: windowStatus,
      sampleCount,
      startedAt: "2026-09-20T00:00:00.000Z",
      deadlineAt: "2026-09-20T00:00:01.000Z",
      deadlineEpochSeconds: 1_000,
      lastSampleAt: "2026-09-20T00:00:00.000Z",
      lastSampleEpochSeconds: 999,
    },
    steps: [{
      id: stepId,
      ordinal: 0,
      kind: "verification",
      title: "观察服务健康",
      purpose: "在受限窗口内重复同一个健康快照",
      allowedTools: ["server.info"],
      evidenceIds: [],
      successCriteria: ["server.info remains healthy"],
      requiresApproval: false,
      maxAttempts: 8,
      attempts: sampleCount,
      status: windowStatus === "succeeded" ? "succeeded" : "running",
    }],
  });
  const hostClient = new HostRpcClient((frame) => {
    const sent = JSON.parse(frame) as { id: number; method: string; params?: Record<string, unknown> };
    methods.push(sent.method);
    if (sent.method === "host.context.fetch") {
      hostClient.handleIncoming({ jsonrpc: "2.0", id: sent.id, result: { status: "not_found" } });
    } else if (sent.method === "host.investigation.plan.check") {
      hostClient.handleIncoming({
        jsonrpc: "2.0",
        id: sent.id,
        result: {
          status: "allowed",
          planId,
          stepId,
          stepKind: "verification",
          evidenceIds: [],
          requiresApproval: false,
        },
      });
    } else if (sent.method === "host.tool.execute") {
      hostClient.handleIncoming({
        jsonrpc: "2.0",
        id: sent.id,
        result: {
          status: "success",
          output: {
            id: `snapshot_${stepResultCount + 1}`,
            serverId: "srv_e2e",
            collectedAt: "2026-09-20T00:00:00.000Z",
            health: "healthy",
            capabilities: { linux: true },
          },
        },
      });
    } else if (sent.method === "host.investigation.evidence.record") {
      hostClient.handleIncoming({ jsonrpc: "2.0", id: sent.id, result: { recorded: true, evidenceId: `ev_observation_${stepResultCount + 1}` } });
    } else if (sent.method === "host.investigation.artifact.record") {
      hostClient.handleIncoming({ jsonrpc: "2.0", id: sent.id, result: { recorded: true, artifact: sent.params?.artifact } });
    } else if (sent.method === "host.investigation.plan.step_result") {
      stepResultCount += 1;
      const running = stepResultCount === 1;
      hostClient.handleIncoming({
        jsonrpc: "2.0",
        id: sent.id,
        result: running
          ? {
              recorded: true,
              observation: "running",
              sampleAccepted: true,
              nextSampleAt: new Date(Date.now() + 8).toISOString(),
              plan: planFor("running", 1),
            }
          : { recorded: true, plan: planFor("succeeded", 2) },
      });
    }
  });

  const runtime = createRuntime({ log: silent, hostToolClient: hostClient, maxRunMs: 2_000 });
  const events: AgentStreamEvent[] = [];
  const result = await runtime.loop.start(
    runRequest({ runId: "run_observation_replay", taskId }),
    { emit: (event) => events.push(event) },
    new OpenAiCompatibleProvider({ baseUrl: `http://127.0.0.1:${port}`, model: "mock-model" }),
  );

  assert.equal(result.state, "completed", JSON.stringify(result));
  assert.equal(result.toolCalls, 2);
  assert.equal(stepResultCount, 2);
  assert.equal(methods.filter((method) => method === "host.tool.execute").length, 2);
  assert.equal(methods.filter((method) => method === "host.investigation.plan.step_result").length, 2);
  const toolCalls = events.filter((event): event is Extract<AgentStreamEvent, { type: "agent.tool_call" }> => event.type === "agent.tool_call");
  assert.equal(toolCalls.length, 2);
  assert.equal(toolCalls[0]?.toolName, "server.info");
  assert.match(toolCalls[1]?.callId ?? "", /^observation_/);
  assert.equal(toolCalls.every((event) => event.toolName === "server.info"), true);
});

test("E2E: the bounded playbook creates a host plan before the first task tool call", async (t) => {
  const { port, close } = await mockLlm([
    [sseToolCall({ index: 0, type: "function", id: "call_playbook", function: { name: "investigation__playbook", arguments: '{"template":"readonly_health"}' } })],
    [sseText("已生成只读健康排查计划")],
  ]);
  t.after(() => close());

  const methods: string[] = [];
  const hostClient = new HostRpcClient((frame) => {
    const sent = JSON.parse(frame) as { id: number; method: string; params?: Record<string, unknown> };
    methods.push(sent.method);
    if (sent.method === "host.context.fetch") {
      hostClient.handleIncoming({ jsonrpc: "2.0", id: sent.id, result: { status: "not_found" } });
    } else if (sent.method === "host.investigation.plan.record") {
      hostClient.handleIncoming({
        jsonrpc: "2.0",
        id: sent.id,
        result: { recorded: true, plan: sent.params?.plan },
      });
    } else if (sent.method === "host.investigation.evidence.record") {
      hostClient.handleIncoming({ jsonrpc: "2.0", id: sent.id, result: { recorded: true } });
    }
  });
  const runtime = createRuntime({ log: silent, hostToolClient: hostClient });
  const result = await runtime.loop.start(
    runRequest({ runId: "run_playbook", taskId: "task_playbook" }),
    { emit: () => {} },
    new OpenAiCompatibleProvider({ baseUrl: `http://127.0.0.1:${port}`, model: "mock-model" }),
  );

  assert.equal(result.state, "completed", JSON.stringify(result));
  assert.equal(methods.filter((method) => method === "host.investigation.plan.record").length, 1);
  assert.equal(methods.includes("host.investigation.plan.check"), false);
});

test("E2E: a reused evidence sample cannot advance the bound plan step", async (t) => {
  const seenRequests: string[] = [];
  const { port, close } = await mockLlm([
    [sseToolCall({ index: 0, type: "function", id: "call_plan_allowed", function: { name: "server__info", arguments: "{}" } })],
    [sseText("已记录基线")],
  ], seenRequests);
  t.after(() => close());

  const plan = {
    id: "plan_allowed",
    taskId: "task_plan_allowed",
    revision: 1,
    status: "active" as const,
    createdAt: "2026-09-19T00:00:00.000Z",
    updatedAt: "2026-09-19T00:00:00.000Z",
    currentStepId: "plan_step_allowed",
    steps: [{
      id: "plan_step_allowed",
      ordinal: 0,
      kind: "evidence" as const,
      title: "Collect baseline",
      purpose: "Record the current server state",
      allowedTools: ["server.info"],
      evidenceIds: [],
      successCriteria: ["A baseline exists"],
      requiresApproval: false,
      maxAttempts: 1,
      attempts: 0,
      status: "running" as const,
    }],
  };
  let executeParams: Record<string, unknown> | undefined;
  let checkParams: Record<string, unknown> | undefined;
  let planResultParams: Record<string, unknown> | undefined;
  let artifactRecorded = false;
  const hostClient = new HostRpcClient((frame) => {
    const sent = JSON.parse(frame) as { id: number; method: string; params: Record<string, unknown> };
    if (sent.method === "host.context.fetch") {
      hostClient.handleIncoming({ jsonrpc: "2.0", id: sent.id, result: { status: "not_found" } });
    } else if (sent.method === "host.investigation.plan.check") {
      checkParams = sent.params;
      hostClient.handleIncoming({
        jsonrpc: "2.0",
        id: sent.id,
        result: {
          status: "allowed",
          planId: plan.id,
          stepId: plan.currentStepId,
          stepKind: "evidence",
          evidenceIds: [],
          requiresApproval: false,
        },
      });
    } else if (sent.method === "host.tool.execute") {
      executeParams = sent.params;
      hostClient.handleIncoming({
        jsonrpc: "2.0",
        id: sent.id,
        result: {
          status: "success",
          output: {
            id: "snap_plan_allowed",
            serverId: "srv_e2e",
            collectedAt: "2026-09-19T00:00:00.000Z",
            health: "healthy",
            capabilities: { linux: true },
          },
        },
      });
    } else if (sent.method === "host.investigation.evidence.record") {
      hostClient.handleIncoming({
        jsonrpc: "2.0",
        id: sent.id,
        result: { recorded: true, evidenceId: "ev_canonical", reused: true },
      });
    } else if (sent.method === "host.investigation.artifact.record") {
      artifactRecorded = true;
      hostClient.handleIncoming({
        jsonrpc: "2.0",
        id: sent.id,
        result: { recorded: true, artifact: sent.params.artifact },
      });
    } else if (sent.method === "host.investigation.plan.step_result") {
      planResultParams = sent.params;
      hostClient.handleIncoming({ jsonrpc: "2.0", id: sent.id, result: { recorded: true, plan: { ...plan, updatedAt: "2026-09-19T00:00:01.000Z" } } });
    }
  });
  const runtime = createRuntime({ log: silent, hostToolClient: hostClient });
  const events: AgentStreamEvent[] = [];
  const result = await runtime.loop.start(
    runRequest({ runId: "run_plan_allowed", taskId: plan.taskId }),
    { emit: (event) => events.push(event) },
    new OpenAiCompatibleProvider({ baseUrl: `http://127.0.0.1:${port}`, model: "mock-model" }),
  );

  assert.equal(result.state, "completed", JSON.stringify(result));
  assert.deepEqual(checkParams?.input, {});
  assert.deepEqual(
    { planId: executeParams?.planId, planStepId: executeParams?.planStepId, evidenceIds: executeParams?.evidenceIds },
    { planId: plan.id, planStepId: plan.currentStepId, evidenceIds: [] },
  );
  const toolCall = events.find((event) => event.type === "agent.tool_call");
  assert(toolCall && toolCall.type === "agent.tool_call");
  assert.equal(toolCall.planId, plan.id);
  assert.equal(toolCall.planStepId, plan.currentStepId);
  assert.deepEqual(toolCall.evidenceIds, []);
  assert.deepEqual(
    {
      status: planResultParams?.status,
      retryable: planResultParams?.retryable,
      outputSummary: planResultParams?.outputSummary,
    },
    {
      status: "failed",
      retryable: false,
      outputSummary: "本次读取没有产生新的调查证据，计划步骤未推进；需要重规划或等待用户决定",
    },
  );
  assert.equal(artifactRecorded, false, "a reused sample must not create a fresh baseline artifact");
  assert.equal(seenRequests.length, 2);
  const secondRequest = seenRequests.at(1);
  assert(secondRequest);
  assert.match(JSON.stringify(JSON.parse(secondRequest).messages), /ev_canonical/);
  assert.match(JSON.stringify(JSON.parse(secondRequest).messages), /复用相同证据/);
  assert.match(JSON.stringify(JSON.parse(secondRequest).messages), /没有产生新的调查证据/);
});

test("durable task ids are carried on terminal run events", async (t) => {
  const { port, close } = await mockLlm([[sseText("task complete")]]);
  t.after(() => close());
  const runtime = createRuntime({ log: silent });
  const events: AgentStreamEvent[] = [];

  await runtime.loop.start(
    runRequest({ runId: "run_task_event", taskId: "task_event" }),
    { emit: (event) => events.push(event) },
    new OpenAiCompatibleProvider({ baseUrl: `http://127.0.0.1:${port}`, model: "mock-model" }),
  );

  const completed = events.find((event) => event.type === "agent.completed");
  assert(completed && completed.type === "agent.completed");
  assert.equal(completed.taskId, "task_event");
});

test("E2E: a denied call still closes its trace step, and the run names its trace", async (t) => {
  const { port, close } = await mockLlm([
    [
      sseToolCall({
        index: 0,
        type: "function",
        id: "call_denied",
        function: { name: "filesystem__write", arguments: '{"path":"/etc/motd","content":"x"}' },
      }),
    ],
    [sseText("这个写入被只读模式拒绝了")],
  ]);
  t.after(() => close());

  // The stub only has to answer the context reads: the call under test is denied by the
  // permission engine, so it never reaches the host — which is exactly what is asserted.
  const hostClient = new HostRpcClient((frame) => {
    const sent = JSON.parse(frame) as { id: number; method: string };
    if (sent.method === "host.context.fetch") {
      hostClient.handleIncoming({ jsonrpc: "2.0", id: sent.id, result: { status: "not_found" } });
    }
  });
  const ledgers: TraceRecorder[] = [];
  const runtime = createRuntime({
    log: silent,
    hostToolClient: hostClient,
    createTrace: (info) => {
      const recorder = new TraceRecorder(info.runId, info.title);
      ledgers.push(recorder);
      return recorder;
    },
  });
  const events: AgentStreamEvent[] = [];

  const result = await runtime.loop.start(
    runRequest({ runId: "run_denied", mode: "readonly" }),
    { emit: (event) => events.push(event) },
    new OpenAiCompatibleProvider({ baseUrl: `http://127.0.0.1:${port}`, model: "mock-model" }),
  );

  assert.equal(result.state, "completed", JSON.stringify(result));
  const toolCall = events.find((event) => event.type === "agent.tool_call");
  assert(toolCall && toolCall.type === "agent.tool_call", "the denied call must still be visible");
  assert.equal(toolCall.decision, "deny");
  assert.equal(result.traceId, toolCall.traceId, "the run must name the trace its steps were recorded under");

  assert.equal(ledgers.length, 1, "exactly one ledger per run");
  const ledger = ledgers[0];
  assert(ledger);
  assert.equal(ledger.steps.length, 1);
  assert.equal(ledger.steps[0]?.stepId, toolCall.stepId);
  assert.equal(ledger.steps[0]?.status, "failed", "a denied step is closed, not left running");
  assert.match(ledger.steps[0]?.error ?? "", /readonly/);
});

test("E2E: a general question runs without a server target", async (t) => {
  const { port, close } = await mockLlm([[sseText("可以直接回答一般问题")]]);
  t.after(() => close());

  const runtime = createRuntime({ log: silent });
  const events: AgentStreamEvent[] = [];
  const result = await runtime.loop.start(
    runRequest({
      prompt: "什么是蓝绿部署？",
      target: undefined,
      focusServerId: undefined,
    }),
    { emit: (event) => events.push(event) },
    new OpenAiCompatibleProvider({ baseUrl: `http://127.0.0.1:${port}`, model: "mock-model" }),
  );

  assert.equal(result.state, "completed", JSON.stringify(result));
  assert.match(result.text, /可以直接回答一般问题/);
  assert.equal(events.some((event) => event.type === "agent.tool_call"), false);
});

test("E2E: redacts sensitive prompt and model text at the provider boundary", async (t) => {
  const requests: string[] = [];
  const { port, close } = await mockLlm([[sseText("response sk-example1")]], requests);
  t.after(() => close());

  const runtime = createRuntime({ log: silent });
  const result = await runtime.loop.start(
    runRequest({ runId: "run_redaction", prompt: "api_key=demo123" }),
    { emit: noop },
    new OpenAiCompatibleProvider({ baseUrl: `http://127.0.0.1:${port}`, model: "mock-model" }),
  );

  assert.equal(result.state, "completed", JSON.stringify(result));
  assert.doesNotMatch(requests[0] ?? "", /demo123/);
  assert.doesNotMatch(result.text, /example1/);
  assert.match(result.text, /\[redacted\]/);
});

test("E2E: Stop aborts the in-flight call and lands on cancelled", async (t) => {
  const { port, close } = await mockLlm(["hang"]);
  t.after(() => close());

  const runtime = createRuntime({ log: silent });
  const controller = new AbortController();
  const events: AgentStreamEvent[] = [];

  const started = runtime.loop.start(
    runRequest({ runId: "run_stop" }),
    { emit: (event) => events.push(event), signal: controller.signal },
    new OpenAiCompatibleProvider({ baseUrl: `http://127.0.0.1:${port}`, model: "mock-model" }),
  );

  setTimeout(() => {
    runtime.loop.stop("run_stop");
  }, 50);

  const result = await started;
  assert.equal(result.state, "cancelled", JSON.stringify(result));
  const finalEvent = events.at(-1);
  assert(
    finalEvent && finalEvent.type === "agent.completed" && finalEvent.result.state === "cancelled",
    "completion must carry state=cancelled",
  );
});

test("E2E: the whole run has a wall-clock deadline", async (t) => {
  const { port, close } = await mockLlm(["hang"]);
  t.after(() => close());

  const runtime = createRuntime({ log: silent, maxRunMs: 40 });
  const result = await runtime.loop.start(
    runRequest({ runId: "run_deadline" }),
    { emit: noop },
    new OpenAiCompatibleProvider({ baseUrl: `http://127.0.0.1:${port}`, model: "mock-model" }),
  );

  assert.equal(result.state, "failed", JSON.stringify(result));
  assert.match(result.error ?? "", /maxRunMs=40/);
});


/** 用户配置的 `responses` 方言会经 /responses 端点执行。 */
test("E2E (responses dialee): tool chain via /responses", async (t) => {
  const { port, close } = await mockLlm([
    [
      { type: "response.output_item.added", item: { type: "function_call", id: "fc_1", name: "system__echo", arguments: "" } },
      { type: "response.function_call_arguments.delta", item_id: "fc_1", delta: '{"message":"hi from responses"}' },
    ],
    [
      { type: "response.output_text.delta", delta: "responses answered" },
    ],
  ]);
  t.after(() => close());

  const runtime = createRuntime({ log: silent });
  const events: AgentStreamEvent[] = [];

  const result = await runtime.loop.start(
    runRequest(),
    { emit: (event) => events.push(event) },
    new OpenAiCompatibleProvider({
      baseUrl: `http://127.0.0.1:${port}`,
      model: "gpt-5.6-terra",
      wireApi: "responses",
    }),
  );

  assert.equal(result.state, "completed", JSON.stringify(result));
  assert.equal(result.toolCalls, 1);
  const toolResult = events.find((event) => event.type === "agent.tool_result");
  assert(toolResult && toolResult.type === "agent.tool_result");
  assert.equal(toolResult.status, "success");
  assert.match(result.text, /responses answered/);
});

test("E2E (responses dialect): provider failures are surfaced", async (t) => {
  const { port, close } = await mockLlm([
    [
      {
        type: "response.failed",
        response: { error: { message: "模型暂时不可用（api key: ****g2z5）", code: "model_error" } },
      },
    ],
  ]);
  t.after(() => close());

  const runtime = createRuntime({ log: silent });
  const events: AgentStreamEvent[] = [];
  const result = await runtime.loop.start(
    runRequest({ runId: "run_responses_failed" }),
    { emit: (event) => events.push(event) },
    new OpenAiCompatibleProvider({
      baseUrl: `http://127.0.0.1:${port}`,
      model: "gpt-5.6-terra",
      wireApi: "responses",
    }),
  );

  assert.equal(result.state, "failed", JSON.stringify(result));
  assert.match(result.error ?? "", /模型暂时不可用/);
  assert.doesNotMatch(result.error ?? "", /g2z5/);
  assert.equal(events.some((event) => event.type === "agent.completed"), false);
  assert.equal(events.some((event) => event.type === "agent.failed"), true);
});

test("E2E: HTTP provider errors do not echo response bodies", async (t) => {
  const { port, close } = await mockProviderFailure('{"error":{"message":"Your api key: ****g2z5 is invalid"}}');
  t.after(() => close());

  const runtime = createRuntime({ log: silent });
  const result = await runtime.loop.start(
    runRequest({ runId: "run_http_failure" }),
    { emit: noop },
    new OpenAiCompatibleProvider({ baseUrl: `http://127.0.0.1:${port}`, model: "mock-model" }),
  );

  assert.equal(result.state, "failed", JSON.stringify(result));
  assert.match(result.error ?? "", /failed \(401\)/);
  assert.doesNotMatch(result.error ?? "", /g2z5/);
});
