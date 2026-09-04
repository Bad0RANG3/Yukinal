import assert from "node:assert/strict";
import test from "node:test";

import type { HostToolExecuteRequest, HostToolExecuteResponse, ToolTarget } from "@yukinal/shared";

import { dockerPsTool } from "./docker-ps.js";
import { dockerRestartTool } from "./docker-restart.js";
import { filesystemReadTool } from "./filesystem-read.js";
import { filesystemWriteTool } from "./filesystem-write.js";
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

test("filesystem tools validate bounded output and declare writes as medium risk", async () => {
  const read = filesystemReadTool(
    fakeHost({ status: "success", output: { path: "/etc/app.env", content: "PORT=8080", truncated: false } }),
  );
  const readOutput = await read.execute({ path: "/etc/app.env", maxBytes: 4096 }, context);
  assert.equal(readOutput.content, "PORT=8080");

  const write = filesystemWriteTool(
    fakeHost({ status: "success", output: { path: "/etc/app.env", bytesWritten: 9 } }),
  );
  const writeOutput = await write.execute({ path: "/etc/app.env", content: "PORT=8080" }, context);
  assert.equal(write.risk, "medium");
  assert.equal(writeOutput.bytesWritten, 9);
});

test("docker restart is exposed as a high-risk host action", async () => {
  const tool = dockerRestartTool(
    fakeHost({ status: "success", output: { container: "api_1", restarted: true } }),
  );
  const output = await tool.execute({ container: "api_1", timeoutSeconds: 15 }, context);
  assert.equal(tool.risk, "high");
  assert.equal(output.restarted, true);
});
