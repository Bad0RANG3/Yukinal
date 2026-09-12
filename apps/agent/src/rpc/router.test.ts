import assert from "node:assert/strict";
import test from "node:test";
import { createServer } from "node:http";

import {
  AGENT_METHODS,
  YUKINAL_RPC_VERSION,
  RPC_ERROR,
  type AgentRunResult,
  type AgentStreamEvent,
  type JsonRpcRequest,
  type SystemDescribeResult,
} from "@yukinal/shared";

import type { AgentLogger } from "../config.js";
import { RpcFailure } from "../errors.js";
import { createRuntime, type Runtime } from "../runtime/create-runtime.js";
import { registerCatalog } from "../mcp/catalog.js";
import { HostRpcClient } from "../transport/host-client.js";

function request(method: string, params?: unknown, id = 1): JsonRpcRequest {
  return { jsonrpc: "2.0", id, method, params };
}

async function withRuntime(): Promise<{ runtime: Runtime; initialize: () => Promise<unknown> }> {
  const runtime = createRuntime({ log: silentLogger() });
  return {
    runtime,
    initialize: () =>
      runtime.router.handle(
        request(AGENT_METHODS.initialize, {
          protocolVersion: YUKINAL_RPC_VERSION,
          clientVersion: "test",
          dataDir: "/tmp/yukinal-test",
        }),
      ),
  };
}

function silentLogger(): AgentLogger {
  const noop = (): void => {};
  return { debug: noop, info: noop, warn: noop, error: noop, child: () => silentLogger() };
}

/**
 * A real OpenAI-compatible SSE endpoint that answers every request with one text turn
 * and remembers the bodies it was sent.
 *
 * These admission tests must observe whether a run happened at all, and the only honest
 * witness of that is the model endpoint: the number of requests it received. Reusing the
 * production provider client (rather than a stub provider) keeps the test on the wire
 * format the sidecar actually speaks.
 */
async function mockLlm(text: string): Promise<{ baseUrl: string; bodies: string[]; close(): void }> {
  const bodies: string[] = [];
  const server = createServer(async (req, res) => {
    let body = "";
    for await (const chunk of req) body += chunk.toString();
    bodies.push(body);
    res.writeHead(200, { "content-type": "text/event-stream" });
    res.write(`data: ${JSON.stringify({ choices: [{ delta: { content: text }, finish_reason: "stop" }] })}\n\n`);
    res.end("data: [DONE]\n\n");
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  assert.ok(address && typeof address === "object");
  return {
    baseUrl: `http://127.0.0.1:${address.port}/v1`,
    bodies,
    close: () => {
      server.closeAllConnections();
      server.close();
    },
  };
}

interface NotificationCapture {
  events: AgentStreamEvent[];
  waitFor(type: AgentStreamEvent["type"]): Promise<AgentStreamEvent>;
}

/** The `agent.stream` sink the transport would attach, so a test can watch the wire. */
function captureNotifications(runtime: Runtime): NotificationCapture {
  const events: AgentStreamEvent[] = [];
  const waiting = new Map<string, Array<(event: AgentStreamEvent) => void>>();
  runtime.router.attachNotifications((_method, params) => {
    const event = params as AgentStreamEvent;
    events.push(event);
    const resolvers = waiting.get(event.type);
    if (!resolvers) return;
    waiting.delete(event.type);
    for (const resolve of resolvers) resolve(event);
  });
  return {
    events,
    waitFor: (type) =>
      new Promise<AgentStreamEvent>((resolve) => {
        waiting.set(type, [...(waiting.get(type) ?? []), resolve]);
      }),
  };
}

/** Long enough for the router's deferred run to have started, had it been started. */
const settle = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 25));

for (const dialect of ["chat", "responses"] as const) {
  test(`provider test makes a real ${dialect} request without tools or workspace content`, async () => {
    let seenPath: string | undefined;
    let seenBody: Record<string, unknown> = {};
    const server = createServer(async (req, res) => {
      seenPath = req.url;
      let raw = "";
      for await (const chunk of req) raw += chunk.toString();
      seenBody = JSON.parse(raw);
      res.writeHead(200, { "content-type": "text/event-stream" });
      const event = dialect === "chat"
        ? { choices: [{ delta: { content: "OK" }, finish_reason: "stop" }] }
        : { type: "response.output_text.delta", delta: "OK" };
      res.end(`data: ${JSON.stringify(event)}\n\ndata: [DONE]\n\n`);
    });
    await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
    try {
      const address = server.address();
      assert.ok(address && typeof address === "object");
      const { runtime, initialize } = await withRuntime();
      await initialize();
      const result = await runtime.router.handle(request(AGENT_METHODS.providerTest, {
        kind: "openai-compatible",
        baseUrl: `http://127.0.0.1:${address.port}/v1`, model: "test-model", wireApi: dialect,
      }));
      assert.deepEqual(result, { ok: true });
      assert.equal(seenPath, dialect === "chat" ? "/v1/chat/completions" : "/v1/responses");
      assert.equal(seenBody.model, "test-model");
      assert.equal(seenBody.tools, undefined);
      assert.ok(JSON.stringify(seenBody).includes("Reply with OK only."));
    } finally { server.closeAllConnections(); server.close(); }
  });
}

for (const outcome of ["unauthorized", "empty", "failed"] as const) {
  test(`provider test rejects ${outcome} responses without exposing response bodies`, async () => {
    let requests = 0;
    const server = createServer((_req, res) => {
      requests++;
      res.writeHead(outcome === "unauthorized" ? 401 : 200, { "content-type": "text/event-stream" });
      res.end(outcome === "unauthorized" ? "private-upstream-details" : outcome === "empty"
        ? "data: [DONE]\n\n"
        : 'data: {"type":"response.failed","error":{"message":"private-upstream-details"}}\n\n');
    });
    await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
    try {
      const address = server.address();
      assert.ok(address && typeof address === "object");
      const { runtime, initialize } = await withRuntime();
      await initialize();
      await assert.rejects(runtime.router.handle(request(AGENT_METHODS.providerTest, {
        kind: "openai-compatible",
        baseUrl: `http://127.0.0.1:${address.port}/v1`, model: "test-model", wireApi: "responses",
      })), (error: Error) => !error.message.includes("private-upstream-details"));
      assert.equal(requests, 1);
    } finally { server.closeAllConnections(); server.close(); }
  });
}

/* ── kind 轴：三种 Provider 都必须能从装配点构造出来，并且真的被用（ADR 0011） ────── */

interface RecordedRequest {
  url: string;
  method: string | undefined;
  headers: Record<string, string>;
  body: Record<string, unknown>;
}

/**
 * 注入假 `fetch`：记下适配器实际打出去的 URL、头与 body，并把脚本化的响应喂回去。
 *
 * 这个仓库的开发环境没有网络，所以测试从不触达真实上游。它证明的是**翻译逻辑**与装配
 * —— 请求形状、鉴权头、SSE 解析能不能跑完一个回合 —— 不是协议保真度（后者需要真实端点）。
 */
function installFetch(handler: (request: RecordedRequest) => Response): {
  seen: RecordedRequest[];
  restore: () => void;
} {
  const original = globalThis.fetch;
  const seen: RecordedRequest[] = [];
  globalThis.fetch = async (input: string | URL | Request, init?: RequestInit): Promise<Response> => {
    const recorded: RecordedRequest = {
      url: typeof input === "string" ? input : input instanceof URL ? input.href : input.url,
      method: init?.method,
      headers: (init?.headers ?? {}) as Record<string, string>,
      body: typeof init?.body === "string" ? (JSON.parse(init.body) as Record<string, unknown>) : {},
    };
    seen.push(recorded);
    return handler(recorded);
  };
  return {
    seen,
    restore: () => {
      globalThis.fetch = original;
    },
  };
}

/** 一段构造好的 SSE：字符串按原样发（`[DONE]` 这类哨兵不是 JSON），对象序列化后发。 */
function sseResponse(frames: Array<unknown>): Response {
  const body = frames.map((frame) => `data: ${typeof frame === "string" ? frame : JSON.stringify(frame)}\n\n`).join("");
  return new Response(body, { status: 200, headers: { "content-type": "text/event-stream" } });
}

const API_KEY = "sk-router-test";

interface KindWire {
  kind: "openai-compatible" | "anthropic" | "gemini";
  baseUrl: string;
  model: string;
  wireApi?: "chat";
  /** 适配器必须打到的完整 URL。 */
  endpoint: string;
  /** 鉴权头由适配器自己写：三种协议的写法各不相同，这正是各自实现存在的理由之一。 */
  auth: { header: string; value: string };
  frames: unknown[];
  /** 请求体里必须成立的那条协议事实。 */
  assertBody: (body: Record<string, unknown>) => void;
}

const KIND_WIRES: KindWire[] = [
  {
    kind: "openai-compatible",
    baseUrl: "https://compatible.example.com/v1",
    model: "gpt-test",
    wireApi: "chat",
    endpoint: "https://compatible.example.com/v1/chat/completions",
    auth: { header: "authorization", value: `Bearer ${API_KEY}` },
    frames: [{ choices: [{ delta: { content: "OK" }, finish_reason: "stop" }] }, "[DONE]"],
    assertBody: (body) => {
      const messages = body.messages as Array<{ role: string }>;
      assert.equal(messages[0]?.role, "system", "chat 方言把系统提示放在 messages 里");
    },
  },
  {
    kind: "anthropic",
    baseUrl: "https://api.anthropic.example.com",
    model: "claude-test",
    endpoint: "https://api.anthropic.example.com/v1/messages",
    auth: { header: "x-api-key", value: API_KEY },
    frames: [
      { type: "message_start", message: { usage: { input_tokens: 5 } } },
      { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "OK" } },
      { type: "message_delta", delta: { stop_reason: "end_turn" }, usage: { output_tokens: 2 } },
      { type: "message_stop" },
    ],
    assertBody: (body) => {
      // 适配器存在的两个理由之一：系统提示是顶层字段，`messages` 里没有 system 角色。
      assert.equal(typeof body.system, "string");
      const messages = body.messages as Array<{ role: string }>;
      assert.ok(!messages.some((message) => message.role === "system"));
      const tools = body.tools as Array<Record<string, unknown>>;
      assert.ok(tools.length > 0);
      assert.ok("input_schema" in (tools[0] ?? {}), "工具用 input_schema，而不是 parameters");
      assert.ok(typeof body.max_tokens === "number", "max_tokens 是必填字段");
    },
  },
  {
    kind: "gemini",
    baseUrl: "https://generativelanguage.example.com",
    model: "gemini-test",
    endpoint: "https://generativelanguage.example.com/v1beta/models/gemini-test:streamGenerateContent?alt=sse",
    auth: { header: "x-goog-api-key", value: API_KEY },
    frames: [{ candidates: [{ content: { parts: [{ text: "OK" }] }, finishReason: "STOP" }] }],
    assertBody: (body) => {
      assert.equal(typeof body.systemInstruction, "object");
      const contents = body.contents as Array<{ role: string }>;
      assert.ok(!contents.some((content) => content.role === "system"));
      const tools = body.tools as Array<{ functionDeclarations?: unknown[] }>;
      assert.ok((tools[0]?.functionDeclarations?.length ?? 0) > 0);
      assert.equal(body.model, undefined, "模型在 URL 路径里，不在 body 里");
    },
  },
];

/**
 * `buildProvider()` 是唯一按 kind 分支的地方（ADR 0011 第 3 点），所以三种 kind 都必须能
 * 从它被构造**并且真的被用起来**。
 *
 * 判据不是「构造没抛错」，而是适配器打出去的请求：URL、鉴权头、以及系统提示与工具被翻译
 * 成了各自协议的形状。运行本身走的是真实的 `agent.run.start`（`delivery: "sync"`），
 * 所以这条用例覆盖的是装配点 → agent loop → 适配器 → SSE 解析这一整条路径。
 */
for (const wire of KIND_WIRES) {
  test(`agent.run.start drives a ${wire.kind} provider through buildProvider`, async (t) => {
    const fetch = installFetch(() => sseResponse(wire.frames));
    t.after(fetch.restore);

    const { runtime, initialize } = await withRuntime();
    await initialize();
    const response = (await runtime.router.handle(
      request(AGENT_METHODS.runStart, {
        runId: `run_${wire.kind}`,
        sessionId: `ses_${wire.kind}`,
        prompt: "只回答 OK",
        delivery: "sync",
        providerConfig: {
          kind: wire.kind,
          baseUrl: wire.baseUrl,
          model: wire.model,
          apiKey: API_KEY,
          ...(wire.wireApi ? { wireApi: wire.wireApi } : {}),
        },
      }),
    )) as { started: boolean; result?: AgentRunResult };

    assert.equal(response.started, true);
    assert.equal(response.result?.state, "completed");
    assert.match(response.result?.text ?? "", /OK/);

    // 一次运行只该打一次上游：构造失败了会有异常，构造成功了但没被使用则这里会是 0 次。
    assert.equal(fetch.seen.length, 1);
    const sent = fetch.seen[0]!;
    assert.equal(sent.url, wire.endpoint);
    assert.equal(sent.method, "POST");
    // 凭据只以该协议自己的头出现，别的写法都不该同时存在：多一个 Authorization 就多一条
    // 泄漏路径，而两种头同时出现时「哪一个是权威」会变成上游的解释。
    assert.equal(sent.headers[wire.auth.header], wire.auth.value);
    for (const other of ["authorization", "x-api-key", "x-goog-api-key"]) {
      if (other !== wire.auth.header) {
        assert.equal(sent.headers[other], undefined, `${wire.kind} must not also send ${other}`);
      }
    }
    wire.assertBody(sent.body);
  });
}

/**
 * 目录读取同样按各自协议（ADR 0011 第 5 点）：两个原生适配器都有可用的 `listModels()`，
 * 而设置界面的模型选择器就是从这里拿数据的 —— 它如果不能工作，新的 kind 依然配不出来。
 */
for (const catalog of [
  {
    kind: "openai-compatible" as const,
    baseUrl: "https://compatible.example.com/v1",
    endpoint: "https://compatible.example.com/v1/models",
    payload: { data: [{ id: "gpt-test" }] },
    listed: "gpt-test",
  },
  {
    kind: "anthropic" as const,
    baseUrl: "https://api.anthropic.example.com",
    endpoint: "https://api.anthropic.example.com/v1/models",
    payload: { data: [{ id: "claude-test", display_name: "Claude Test" }] },
    listed: "claude-test",
  },
  {
    kind: "gemini" as const,
    baseUrl: "https://generativelanguage.example.com",
    endpoint: "https://generativelanguage.example.com/v1beta/models",
    payload: {
      models: [
        { name: "models/gemini-test", supportedGenerationMethods: ["generateContent"] },
        // 目录里也有只支持别的方法的模型：选择器不该把它列出来。
        { name: "models/text-embedding-004", supportedGenerationMethods: ["embedContent"] },
      ],
    },
    listed: "gemini-test",
  },
]) {
  test(`provider.models reads the ${catalog.kind} catalog from its own endpoint`, async (t) => {
    const fetch = installFetch(() => Response.json(catalog.payload));
    t.after(fetch.restore);

    const { runtime, initialize } = await withRuntime();
    await initialize();
    const { models } = (await runtime.router.handle(
      request(AGENT_METHODS.providerModels, {
        kind: catalog.kind,
        baseUrl: catalog.baseUrl,
        model: "unused-for-the-catalog",
        apiKey: API_KEY,
      }),
    )) as { models: Array<{ id: string }> };

    assert.equal(fetch.seen[0]?.url, catalog.endpoint);
    assert.deepEqual(models.map((model) => model.id), [catalog.listed]);
  });
}

test("initialize negotiates the protocol version", async () => {
  const { initialize } = await withRuntime();
  const result = (await initialize()) as { protocolVersion: string; capabilities: Record<string, boolean> };
  assert.equal(result.protocolVersion, YUKINAL_RPC_VERSION);
  assert.equal(result.capabilities.cancellation, true);
});

test("capabilities.mcp reports the registry, not an intention", async () => {
  const { initialize } = await withRuntime();
  const first = (await initialize()) as { capabilities: Record<string, boolean> };
  // At handshake time the catalog cannot have arrived yet: the host does not relay sidecar
  // requests before the handshake completes. So `false` here is the truth about this
  // instant, and the point of the assertion is that it is *read*, not written down.
  assert.equal(first.capabilities.mcp, false, "nothing is registered yet, so the flag is false");

  // Register one tool that really came from a server, and the same flag must flip. A
  // literal `false` would pass the assertion above and be wrong forever after.
  const runtime = createRuntime({ log: silentLogger() });
  registerCatalog(
    runtime.registry,
    {
      servers: [
        {
          serverId: "mcp_1",
          segment: "mcp-1",
          label: "fixture",
          tools: [
            {
              name: "mcp.mcp-1.echo",
              serverId: "mcp_1",
              tool: "echo",
              description: "Echo text back.",
              inputSchema: { type: "object", properties: {} },
            },
          ],
        },
      ],
      failures: [],
    },
    {
      executeOnHost: async () => ({
        status: "success",
        output: { serverId: "mcp_1", tool: "echo", isError: false, text: "", content: [] },
      }),
    },
  );
  const second = (await runtime.router.handle(
    request(AGENT_METHODS.initialize, {
      protocolVersion: YUKINAL_RPC_VERSION,
      clientVersion: "test",
      dataDir: "/tmp/yukinal-test",
    }),
  )) as { capabilities: Record<string, boolean> };
  assert.equal(second.capabilities.mcp, true, "a registered MCP tool makes the flag true");
});

test("initialize cannot be negotiated twice on one sidecar session", async () => {
  const { initialize } = await withRuntime();
  await initialize();
  await assert.rejects(
    initialize(),
    (error: unknown) => error instanceof RpcFailure && error.code === RPC_ERROR.INVALID_REQUEST,
  );
});

test("a mismatched protocol version is refused, not guessed", async () => {
  const { runtime } = await withRuntime();
  await assert.rejects(
    runtime.router.handle(
      request(AGENT_METHODS.initialize, { protocolVersion: "0.1", clientVersion: "old", dataDir: "" }),
    ),
    (error: unknown) => error instanceof RpcFailure && error.code === RPC_ERROR.INVALID_PARAMS,
  );
});

test("every other method requires initialization first", async () => {
  const { runtime } = await withRuntime();
  await assert.rejects(
    runtime.router.handle(request(AGENT_METHODS.ping, {})),
    (error: unknown) => error instanceof RpcFailure && error.code === RPC_ERROR.INVALID_REQUEST,
  );
});

test("ping answers and reports the process id", async () => {
  const { runtime, initialize } = await withRuntime();
  await initialize();
  assert.deepEqual(await runtime.router.handle(request(AGENT_METHODS.ping, { echo: "orb" })), {
    pong: "orb",
    agentPid: process.pid,
  });
});

test("tools.list returns declarations, never implementations", async () => {
  const { runtime, initialize } = await withRuntime();
  await initialize();
  const { tools } = (await runtime.router.handle(request(AGENT_METHODS.listTools, {}))) as {
    tools: Array<{ name: string; risk: string; timeoutMs: number }>;
  };
  assert.ok(tools.some((tool) => tool.name === "system.echo"));
  assert.ok(tools.every((tool) => tool.timeoutMs > 0 && typeof tool.risk === "string"));
});

/**
 * 宿主文件工具必须**同时**存在实现与注册。
 *
 * 这个仓库真的发生过「工具写完了、测试全绿、但没人注册」：模型永远看不到它，而任何单元
 * 测试都不会红，因为测试直接构造工具、不经过 `createRuntime`。这条测试走 `createRuntime`
 * 那条路径（也就是 sidecar 真实启动时走的那条），把「注册」这件事本身钉住。
 *
 * 它同时钉住一条边界：没有宿主客户端时这些工具**不该**出现。它们每一次调用都要过
 * `host.tool.execute`，没有宿主就没有实现可言。
 */
test("the host file tools are registered, not merely implemented", async () => {
  const { runtime, initialize } = await withRuntime();
  await initialize();
  const bare = (await runtime.router.handle(request(AGENT_METHODS.listTools, {}))) as {
    tools: Array<{ name: string }>;
  };
  assert.ok(
    !bare.tools.some((tool) => tool.name.startsWith("filesystem.")),
    "without a host client there is nothing to serve these tools",
  );

  const hosted = createRuntime({ log: silentLogger(), hostToolClient: new HostRpcClient(() => {}) });
  const declared = hosted.declarations.map((tool) => tool.name);
  for (const name of ["filesystem.read", "filesystem.write", "filesystem.edit"]) {
    assert.ok(declared.includes(name), `${name} must be declared to the model`);
  }
});

test("describe advertises what is and is not implemented yet", async () => {
  const { runtime, initialize } = await withRuntime();
  await initialize();
  const described = (await runtime.router.handle(
    request(AGENT_METHODS.describe, {}),
  )) as SystemDescribeResult;

  assert.deepEqual(described.toolNameCollisions, []);
  assert.equal(described.implemented[AGENT_METHODS.initialize], true);
  assert.equal(described.implemented[AGENT_METHODS.runStart], true);
  assert.equal(described.implemented[AGENT_METHODS.runStop], true);
  assert.equal(described.implemented[AGENT_METHODS.approvalRespond], true);
  assert.ok(described.permissionPolicyIds.includes("policy.production"));
});

test("agent.run.start requires providerConfig and validates it", async () => {
  const { runtime, initialize } = await withRuntime();
  await initialize();

  // 没有 providerConfig：契约错误（INVALID_PARAMS），不是"未实现"。
  await assert.rejects(
    runtime.router.handle(
      request(AGENT_METHODS.runStart, {
        runId: "run_1",
        sessionId: "ses_1",
        prompt: "why is the api restarting",
        target: { host: "remote", serverId: "srv_1", environment: "staging" },
      }),
    ),
    (error: unknown) => error instanceof RpcFailure && error.code === RPC_ERROR.INVALID_PARAMS,
  );

  // 带 providerConfig 但没有可用端点时，run.start 返回 started 而 run 失败会走事件。
  const ok = (await runtime.router.handle(
    request(AGENT_METHODS.runStart, {
      runId: "run_2",
      sessionId: "ses_2",
      prompt: "hi",
      providerConfig: { kind: "openai-compatible", baseUrl: "http://127.0.0.1:1", model: "m" },
    }),
  )) as { runId: string; started: boolean };
  assert.equal(ok.started, true);
  assert.equal(ok.runId, "run_2");

  // 契约里没有的 kind 仍然是契约错误。`anthropic` 曾经也走这条断言（那时它确实不被支持）；
  // 现在它是一条正例，见下面的「三种 kind 都能从装配点被构造并被真正使用」。
  await assert.rejects(
    runtime.router.handle(
      request(AGENT_METHODS.runStart, {
        runId: "run_3",
        sessionId: "ses_3",
        prompt: "hi",
        providerConfig: { kind: "cohere", baseUrl: "http://x", model: "m" } as never,
      }),
    ),
    (error: unknown) => error instanceof RpcFailure && error.code === RPC_ERROR.INVALID_PARAMS,
  );
});

test("agent.run.start admits one OpenCode-style message exactly once", async () => {
  const { runtime, initialize } = await withRuntime();
  await initialize();
  const params = {
    runId: "run_admitted",
    sessionId: "ses_admitted",
    messageId: "msg_admitted",
    prompt: "fallback text",
    parts: [{ type: "text" as const, text: "canonical text" }],
    delivery: "async" as const,
    resume: true,
    providerConfig: { kind: "openai-compatible" as const, baseUrl: "http://127.0.0.1:1", model: "m" },
  };
  const first = (await runtime.router.handle(request(AGENT_METHODS.runStart, params, 1))) as { runId: string; started: boolean };
  const retry = (await runtime.router.handle(request(AGENT_METHODS.runStart, { ...params, runId: "run_other" }, 2))) as {
    runId: string;
    started: boolean;
    duplicate: boolean;
  };
  assert.equal(first.runId, "run_admitted");
  assert.equal(retry.runId, first.runId);
  assert.equal(retry.duplicate, true);
  await assert.rejects(
    runtime.router.handle(request(AGENT_METHODS.runStart, { ...params, parts: [{ type: "text", text: "different" }] }, 3)),
    (error: unknown) => error instanceof RpcFailure && error.code === RPC_ERROR.INVALID_PARAMS,
  );
});

test("agent.run.start rejects a duplicate run id while the first run is active", async () => {
  const { runtime, initialize } = await withRuntime();
  await initialize();
  const params = {
    runId: "run_duplicate",
    sessionId: "ses_duplicate",
    prompt: "hello",
    providerConfig: { kind: "openai-compatible" as const, baseUrl: "http://127.0.0.1:1", model: "m" },
  };
  await runtime.router.handle(request(AGENT_METHODS.runStart, params, 1));
  await assert.rejects(
    runtime.router.handle(request(AGENT_METHODS.runStart, params, 2)),
    (error: unknown) => error instanceof RpcFailure && error.code === RPC_ERROR.INVALID_PARAMS,
  );
});

test("resume: false admits the message without running it", async (t) => {
  const llm = await mockLlm("this answer must never be produced");
  t.after(() => llm.close());
  const { runtime, initialize } = await withRuntime();
  await initialize();
  const capture = captureNotifications(runtime);

  const admitted = await runtime.router.handle(
    request(AGENT_METHODS.runStart, {
      runId: "run_held",
      sessionId: "ses_held",
      messageId: "msg_held",
      prompt: "重启 staging 的 nginx",
      resume: false,
      providerConfig: { kind: "openai-compatible", baseUrl: llm.baseUrl, model: "m" },
    }),
  );

  // The exact answer matters: nothing was started, so there is no `resumed`, no
  // `duplicate` and no result to report.
  assert.deepEqual(admitted, { runId: "run_held", started: false });
  // Nothing ran, so nothing may be observable: no event, and no model call.
  await settle();
  assert.deepEqual(capture.events, []);
  assert.deepEqual(llm.bodies, []);
});

test("resume: false without a messageId is refused instead of admitting nothing", async () => {
  const { runtime, initialize } = await withRuntime();
  await initialize();
  const capture = captureNotifications(runtime);

  // Admission is keyed by message identity. Answering `started: false` without recording
  // anything would leave the caller unable to tell whether it may resume, or under which
  // run id — so this is a param error, not a silent no-op.
  await assert.rejects(
    runtime.router.handle(
      request(AGENT_METHODS.runStart, {
        runId: "run_anonymous_hold",
        sessionId: "ses_anonymous",
        prompt: "hold this",
        resume: false,
        providerConfig: { kind: "openai-compatible", baseUrl: "http://127.0.0.1:1", model: "m" },
      }),
    ),
    (error: unknown) => error instanceof RpcFailure && error.code === RPC_ERROR.INVALID_PARAMS && /messageId/.test(error.message),
  );
  await settle();
  assert.deepEqual(capture.events, []);
});

test("a later resume starts the admitted message once, under the admitted run id", async (t) => {
  const llm = await mockLlm("admitted answer");
  t.after(() => llm.close());
  const { runtime, initialize } = await withRuntime();
  await initialize();
  const capture = captureNotifications(runtime);

  const params = {
    sessionId: "ses_resume_admitted",
    messageId: "msg_resume_admitted",
    prompt: "部署这次改动",
    providerConfig: { kind: "openai-compatible" as const, baseUrl: llm.baseUrl, model: "m" },
  };
  const admitted = (await runtime.router.handle(
    request(AGENT_METHODS.runStart, { ...params, runId: "run_admitted", resume: false }, 1),
  )) as { runId: string; started: boolean };
  assert.deepEqual(admitted, { runId: "run_admitted", started: false });

  const completed = capture.waitFor("agent.completed");
  // The resuming call carries a different runId on purpose: the message's identity is
  // the admitted one, and that is the id the run must have.
  const resumed = (await runtime.router.handle(
    request(AGENT_METHODS.runStart, { ...params, runId: "run_resuming_call", resume: true }, 2),
  )) as { runId: string; started: boolean; resumed?: boolean; duplicate?: boolean };
  assert.deepEqual(resumed, { runId: "run_admitted", started: true, resumed: true });

  // A third call for the same message — the shape a transport retry takes — must not
  // fork a second run, whether the first one is still in flight or already finished.
  const retry = await runtime.router.handle(request(AGENT_METHODS.runStart, { ...params, runId: "run_third_call" }, 3));
  assert.deepEqual(retry, { runId: "run_admitted", started: false, duplicate: true });

  const finished = (await completed) as Extract<AgentStreamEvent, { type: "agent.completed" }>;
  assert.equal(finished.result.runId, "run_admitted");
  assert.equal(finished.result.state, "completed");
  assert.equal(llm.bodies.length, 1, "three calls for one message must produce exactly one run");
});

test("delivery: sync answers with the run result, after the events have streamed", async (t) => {
  const llm = await mockLlm("同步运行的回答");
  t.after(() => llm.close());
  const { runtime, initialize } = await withRuntime();
  await initialize();

  // One ordered log rather than two collectors: the property under test is *when* the
  // response arrives relative to the run's notifications.
  const observed: string[] = [];
  runtime.router.attachNotifications((_method, params) => {
    observed.push(`event:${(params as AgentStreamEvent).type}`);
  });

  const response = (await runtime.router.handle(
    request(AGENT_METHODS.runStart, {
      runId: "run_sync",
      sessionId: "ses_sync",
      prompt: "只回答一句话",
      delivery: "sync",
      providerConfig: { kind: "openai-compatible", baseUrl: llm.baseUrl, model: "m" },
    }),
  )) as { runId: string; started: boolean; result?: AgentRunResult };
  observed.push("response");

  assert.equal(response.started, true);
  assert.equal(response.result?.state, "completed");
  assert.equal(response.result?.runId, "run_sync");
  assert.match(response.result?.text ?? "", /同步运行的回答/);
  // A sync response necessarily comes last — the transport matches frames by request id,
  // so the events of the run are delivered while the caller waits. This is the ordering
  // the router documents, and the one an async call deliberately does not have.
  assert.equal(observed.at(-1), "response", observed.join(", "));
  assert.deepEqual(observed, ["event:agent.started", "event:agent.thinking", "event:agent.completed", "response"]);
  assert.equal(llm.bodies.length, 1);
});

test("an unknown policyId is refused and starts nothing", async (t) => {
  const llm = await mockLlm("must not run");
  t.after(() => llm.close());
  const { runtime, initialize } = await withRuntime();
  await initialize();
  const capture = captureNotifications(runtime);

  const params = {
    runId: "run_policy",
    sessionId: "ses_policy",
    prompt: "重启生产环境",
    providerConfig: { kind: "openai-compatible" as const, baseUrl: llm.baseUrl, model: "m" },
  };
  await assert.rejects(
    runtime.router.handle(request(AGENT_METHODS.runStart, { ...params, policyId: "policy.terraform" }, 1)),
    (error: unknown) => {
      assert.ok(error instanceof RpcFailure, String(error));
      assert.equal(error.code, RPC_ERROR.INVALID_PARAMS);
      // The report names what was rejected *and* what exists: a fallback to the
      // environment default would have run this request under a policy nobody asked for.
      assert.match(error.message, /policy\.terraform/);
      assert.match(error.message, /policy\.production/);
      return true;
    },
  );
  await settle();
  assert.deepEqual(capture.events, []);
  assert.deepEqual(llm.bodies, []);

  // The refusal left nothing behind: the same runId is still usable, which it would not
  // be if the rejected call had registered itself as in flight.
  const completed = capture.waitFor("agent.completed");
  const accepted = (await runtime.router.handle(request(AGENT_METHODS.runStart, params, 2))) as { runId: string; started: boolean };
  assert.equal(accepted.started, true);
  assert.equal(accepted.runId, "run_policy");
  await completed;
  assert.equal(llm.bodies.length, 1);
});

test("unknown methods are rejected", async () => {
  const { runtime, initialize } = await withRuntime();
  await initialize();
  await assert.rejects(
    runtime.router.handle(request("shell.do_anything", {})),
    (error: unknown) => error instanceof RpcFailure && error.code === RPC_ERROR.METHOD_NOT_FOUND,
  );
});
