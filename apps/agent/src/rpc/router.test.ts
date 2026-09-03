import assert from "node:assert/strict";
import test from "node:test";

import {
  AGENT_METHODS,
  YUKINAL_RPC_VERSION,
  RPC_ERROR,
  type JsonRpcRequest,
  type SystemDescribeResult,
} from "@yukinal/shared";

import type { AgentLogger } from "../config.js";
import { RpcFailure } from "../errors.js";
import { createRuntime, type Runtime } from "../runtime/create-runtime.js";

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

test("initialize negotiates the protocol version", async () => {
  const { initialize } = await withRuntime();
  const result = (await initialize()) as { protocolVersion: string; capabilities: Record<string, boolean> };
  assert.equal(result.protocolVersion, YUKINAL_RPC_VERSION);
  assert.equal(result.capabilities.cancellation, true);
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

test("unknown methods are rejected", async () => {
  const { runtime, initialize } = await withRuntime();
  await initialize();
  await assert.rejects(
    runtime.router.handle(request("shell.do_anything", {})),
    (error: unknown) => error instanceof RpcFailure && error.code === RPC_ERROR.METHOD_NOT_FOUND,
  );
});
