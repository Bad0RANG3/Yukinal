import assert from "node:assert/strict";
import test from "node:test";

import {
  AGENT_METHODS,
  AGENT_NOTIFICATIONS,
  YUKINAL_RPC_VERSION,
  RPC_ERROR,
  isJsonRpcRequest,
  type JsonRpcMessage,
} from "@yukinal/shared";

import { AgentClient, AgentRpcError } from "./client.js";
import { createLoopbackTransport } from "./transport.js";

function clientWith(responder: (message: JsonRpcMessage) => JsonRpcMessage | undefined) {
  const transport = createLoopbackTransport(responder);
  return new AgentClient(transport, { clientVersion: "test" });
}

test("resolves a typed response from the sidecar", async () => {
  const client = clientWith((message) =>
    isJsonRpcRequest(message) && message.method === AGENT_METHODS.ping
      ? { jsonrpc: "2.0", id: message.id, result: { pong: "hi", agentPid: 4242 } }
      : undefined,
  );

  assert.deepEqual(await client.ping("hi"), { pong: "hi", agentPid: 4242 });
  client.close();
});

test("turns an error frame into AgentRpcError and exposes notImplemented", async () => {
  const client = clientWith((message) =>
    isJsonRpcRequest(message)
      ? {
          jsonrpc: "2.0",
          id: message.id,
          error: { code: RPC_ERROR.NOT_IMPLEMENTED, message: "agent loop not implemented yet" },
        }
      : undefined,
  );

  await assert.rejects(
    client.startRun({
      runId: "run_1",
      sessionId: "ses_1",
      prompt: "why is api restarting",
      target: { host: "remote", serverId: "srv_1", environment: "staging" },
    }),
    (error: unknown) => error instanceof AgentRpcError && error.notImplemented,
  );
  client.close();
});

test("delivers stream notifications to subscribers only while subscribed", async () => {
  const transport = createLoopbackTransport(() => undefined);
  const client = new AgentClient(transport, { clientVersion: "test" });

  const seen: string[] = [];
  const unsubscribe = client.onStream((event) => seen.push(event.type));

  transport.emit({
    jsonrpc: "2.0",
    method: AGENT_NOTIFICATIONS.stream,
    params: { type: "agent.thinking", runId: "run_1", at: new Date().toISOString() },
  });
  transport.emit({ jsonrpc: "2.0", method: AGENT_NOTIFICATIONS.stream, params: { bogus: true } });

  unsubscribe();
  transport.emit({
    jsonrpc: "2.0",
    method: AGENT_NOTIFICATIONS.stream,
    params: { type: "agent.completed", runId: "run_1", result: { runId: "run_1", state: "completed", text: "", steps: 0, toolCalls: 0 }, at: "" },
  });

  assert.deepEqual(seen, ["agent.thinking"]);
  client.close();
});

test("a request that is never answered times out instead of hanging", async () => {
  const silent = {
    send(): void {},
    onMessage: () => () => {},
    onClose: () => () => {},
    close(): void {},
  };
  const client = new AgentClient(silent, { requestTimeoutMs: 5 });

  await assert.rejects(client.describe(), (error: unknown) => {
    return error instanceof AgentRpcError && error.code === RPC_ERROR.TIMEOUT;
  });
  client.close();
});

test("protocol version is a single shared constant", () => {
  assert.equal(YUKINAL_RPC_VERSION, "1.0");
});
