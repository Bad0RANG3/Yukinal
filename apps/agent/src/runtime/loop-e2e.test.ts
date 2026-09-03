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
import { createRuntime } from "./create-runtime.js";

const noop = (): void => {};
const silent: AgentLogger = { debug: noop, info: noop, warn: noop, error: noop, child: () => silent };

/** 按请求顺序回放脚本的 SSE 服务器。测试结束后必须 close，否则进程不退出。 */
function mockLlm(script: Array<Array<object> | "hang">): Promise<{ port: number; close(): void }> {
  return new Promise((resolve) => {
    let index = 0;
    const server: Server = createServer((_req, res) => {
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
  const toolResult = events.find((event) => event.type === "agent.tool_result");
  assert(toolResult && toolResult.type === "agent.tool_result");
  assert.equal(toolResult.status, "success");
  assert.match(toolResult.outputSummary, /hello from mock/);
  assert(types.includes("agent.completed"), JSON.stringify(types));
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


/** codex `responses` 方言：CC Switch 导入的中转常走 /responses（如 My Codex）。 */
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
