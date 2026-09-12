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

test("initialize negotiates the protocol version", async () => {
  const { initialize } = await withRuntime();
  const result = (await initialize()) as { protocolVersion: string; capabilities: Record<string, boolean> };
  assert.equal(result.protocolVersion, YUKINAL_RPC_VERSION);
  assert.equal(result.capabilities.cancellation, true);
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

  // 畸形 provider kind：契约错误。
  await assert.rejects(
    runtime.router.handle(
      request(AGENT_METHODS.runStart, {
        runId: "run_3",
        sessionId: "ses_3",
        prompt: "hi",
        providerConfig: { kind: "anthropic", baseUrl: "http://x", model: "m" } as never,
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
