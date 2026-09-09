import assert from "node:assert/strict";
import test from "node:test";

import { HOST_METHODS, type HostContextRequest } from "@yukinal/shared";

import { HostRpcClient } from "./host-client.js";

const request = {
  callId: "call_1",
  traceId: "trace_1",
  toolName: "server.info",
  input: {},
  target: { host: "remote" as const, serverId: "srv_01abc", environment: "staging" as const },
};

test("host client sends a typed tool request and resolves its response", async () => {
  const frames: string[] = [];
  const client = new HostRpcClient((frame) => frames.push(frame));
  const pending = client.execute(request);
  const frame = JSON.parse(frames[0] ?? "{}") as { id: number; method: string; params: unknown };

  assert.equal(frame.method, HOST_METHODS.toolExecute);
  assert.deepEqual(frame.params, request);
  assert.equal(client.handleIncoming({ jsonrpc: "2.0", id: frame.id, result: { status: "success", output: { ok: true } } }), true);
  assert.deepEqual(await pending, { status: "success", output: { ok: true } });
});

test("host client sends a typed context request and resolves its response", async () => {
  const frames: string[] = [];
  const client = new HostRpcClient((frame) => frames.push(frame));
  const request: HostContextRequest = { kind: "server", id: "srv_01abc" };
  const pending = client.fetchContext(request);
  const frame = JSON.parse(frames[0] ?? "{}") as { id: number; method: string; params: unknown };

  assert.equal(frame.method, HOST_METHODS.contextFetch);
  assert.deepEqual(frame.params, request);
  assert.equal(
    client.handleIncoming({ jsonrpc: "2.0", id: frame.id, result: { status: "success", data: { id: request.id } } }),
    true,
  );
  assert.deepEqual(await pending, { status: "success", data: { id: request.id } });
});

test("aborting a host request removes it and rejects promptly", async () => {
  const frames: string[] = [];
  const client = new HostRpcClient((frame) => frames.push(frame));
  const controller = new AbortController();
  const pending = client.execute(request, controller.signal);
  const requestFrame = JSON.parse(frames[0] ?? "{}") as { id: number; method: string; params: unknown };
  controller.abort();

  await assert.rejects(pending, /host request cancelled/);
  const cancelFrame = JSON.parse(frames[1] ?? "{}") as {
    id: number;
    method: string;
    params: unknown;
  };
  assert.equal(requestFrame.method, HOST_METHODS.toolExecute);
  assert.equal(cancelFrame.method, HOST_METHODS.toolCancel);
  assert.deepEqual(cancelFrame.params, { requestId: requestFrame.id });
  assert.equal(client.handleIncoming({ jsonrpc: "2.0", id: requestFrame.id, result: { status: "success" } }), true);
});
