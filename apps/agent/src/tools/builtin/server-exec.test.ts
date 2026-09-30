import assert from "node:assert/strict";
import test from "node:test";

import type { HostToolExecuteRequest, HostToolExecuteResponse } from "@yukinal/shared";

import { ToolFailure, type ToolContext } from "../tool.js";
import type { HostToolExecutor } from "./host-backed.js";
import { serverExecTool } from "./server-exec.js";

const context: ToolContext = {
  callId: "call_exec_1",
  runId: "run_exec_1",
  traceId: "trace_exec_1",
  target: { host: "remote", serverId: "srv_01abc", environment: "staging" },
  taskId: "task_1",
  planId: "plan_1",
  planStepId: "step_1",
  approvalId: "apr_1",
  signal: new AbortController().signal,
  deadlineAt: Date.now() + 30_000,
  log: () => {},
};

const input = {
  command: "systemctl status api",
  purpose: "inspect the service state",
  timeoutMs: 5_000,
  maxOutputBytes: 8_192,
};

function host(response: HostToolExecuteResponse, seen: HostToolExecuteRequest[]): HostToolExecutor {
  return {
    execute: async (request) => {
      seen.push(request);
      return response;
    },
  };
}

test("server.exec forwards only the resolved host target and task/approval binding", async () => {
  const seen: HostToolExecuteRequest[] = [];
  const result = {
    state: "completed",
    exitCode: 0,
    stdout: "active",
    stderr: "",
    stdoutTruncated: false,
    stderrTruncated: false,
    durationMs: 42,
  } as const;
  const tool = serverExecTool(host({ status: "success", output: result }, seen));

  assert.deepEqual(await tool.execute(input, context), result);
  assert.equal(tool.risk, "high");
  assert.equal(tool.effectful, true);
  assert.deepEqual(seen[0], {
    callId: context.callId,
    runId: context.runId,
    traceId: context.traceId,
    toolName: "server.exec",
    input,
    target: context.target,
    taskId: context.taskId,
    planId: context.planId,
    planStepId: context.planStepId,
    approvalId: context.approvalId,
  });
});

test("server.exec treats nonzero exit as a non-retryable failure with bounded output", async () => {
  const result = {
    state: "completed",
    exitCode: 7,
    stdout: "partial",
    stderr: "failed",
    stdoutTruncated: true,
    stderrTruncated: false,
    durationMs: 100,
  } as const;
  const tool = serverExecTool(host({
    status: "success",
    output: result,
  }, []));

  await assert.rejects(
    tool.execute(input, context),
    (error: unknown) => error instanceof ToolFailure
      && error.code === "execution_failed"
      && !error.retryable
      && JSON.stringify(error.detail) === JSON.stringify(result),
  );
});

test("server.exec preserves host timeout and uncertain-effect failures", async () => {
  const response: HostToolExecuteResponse = {
    status: "failed",
    error: {
      code: "timeout",
      message: "remote command timed out; its effect may have occurred",
      retryable: false,
      detail: {
        state: "timed_out",
        exitCode: null,
        stdout: "",
        stderr: "",
        stdoutTruncated: false,
        stderrTruncated: false,
        durationMs: 5_000,
      },
    },
  };
  const tool = serverExecTool(host(response, []));

  await assert.rejects(
    tool.execute(input, context),
    (error: unknown) => error instanceof ToolFailure
      && error.code === "timeout"
      && !error.retryable
      && (error.detail as { state?: string } | undefined)?.state === "timed_out",
  );
});
