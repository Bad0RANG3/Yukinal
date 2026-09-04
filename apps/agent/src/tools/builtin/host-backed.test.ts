import assert from "node:assert/strict";
import test from "node:test";

import type { HostToolExecuteRequest, HostToolExecuteResponse, ToolTarget } from "@yukinal/shared";

import { dockerPsTool } from "./docker-ps.js";
import { serverInfoTool } from "./server-info.js";
import { ToolFailure, type ToolContext } from "../tool.js";
import type { HostToolExecutor } from "./host-backed.js";

const target: ToolTarget = { host: "remote", serverId: "srv_01abc", environment: "staging" };
const context: ToolContext = {
  callId: "call_1",
  traceId: "trace_1",
  target,
  signal: new AbortController().signal,
  deadlineAt: Date.now() + 10_000,
  log: () => {},
};

function fakeHost(response: HostToolExecuteResponse, seen: HostToolExecuteRequest[] = []): HostToolExecutor {
  return {
    execute: async (request) => {
      seen.push(request);
      return response;
    },
  };
}

test("host-backed tools forward the resolved target and validate structured output", async () => {
  const seen: HostToolExecuteRequest[] = [];
  const tool = dockerPsTool(
    fakeHost(
      {
        status: "success",
        output: { available: true, containers: [{ name: "web", image: "nginx:1.27", state: "running", status: "Up 1h", restartCount: 0 }] },
      },
      seen,
    ),
  );

  const output = await tool.execute({ all: false }, context);

  assert.deepEqual(output.containers[0]?.name, "web");
  assert.equal(seen[0]?.toolName, "docker.ps");
  assert.deepEqual(seen[0]?.target, target);
});

test("host-backed failures retain the shared error code", async () => {
  const tool = serverInfoTool(
    fakeHost({
      status: "failed",
      error: { code: "transport", message: "server unavailable", retryable: true },
    }),
  );

  await assert.rejects(
    tool.execute({}, context),
    (error: unknown) => error instanceof ToolFailure && error.code === "transport" && error.retryable,
  );
});
