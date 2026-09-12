import assert from "node:assert/strict";
import test from "node:test";

import type { HostToolExecuteRequest, HostToolExecuteResponse, ToolTarget } from "@yukinal/shared";

import { dockerPsTool } from "./docker-ps.js";
import { dockerRestartTool } from "./docker-restart.js";
import { filesystemEditTool } from "./filesystem-edit.js";
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

const REVISION = "a".repeat(64);

test("filesystem tools validate bounded output and declare writes as medium risk", async () => {
  const read = filesystemReadTool(
    fakeHost({
      status: "success",
      output: { path: "/etc/app.env", content: "PORT=8080", truncated: false, revision: REVISION },
    }),
  );
  const readOutput = await read.execute({ path: "/etc/app.env", maxBytes: 4096 }, context);
  assert.equal(readOutput.content, "PORT=8080");
  assert.equal(readOutput.revision, REVISION);

  const write = filesystemWriteTool(
    fakeHost({ status: "success", output: { path: "/etc/app.env", bytesWritten: 9 } }),
  );
  const writeOutput = await write.execute({ path: "/etc/app.env", content: "PORT=8080" }, context);
  assert.equal(write.risk, "medium");
  assert.equal(writeOutput.bytesWritten, 9);
});

test("filesystem.read output without a revision is rejected instead of reaching the model", async () => {
  // The revision is the precondition of every edit: a read result that silently loses it would
  // leave the model with no way to cite one, so the schema refuses the payload outright.
  const read = filesystemReadTool(
    fakeHost({ status: "success", output: { path: "/etc/app.env", content: "PORT=8080", truncated: false } }),
  );
  await assert.rejects(
    read.execute({ path: "/etc/app.env" }, context),
    (error: unknown) => error instanceof ToolFailure && error.code === "internal",
  );
});

test("filesystem.edit forwards the guard inputs and returns the new revision", async () => {
  const seen: HostToolExecuteRequest[] = [];
  const tool = filesystemEditTool(
    fakeHost({ status: "success", output: { path: "/etc/app.env", revision: REVISION, bytesBefore: 10, bytesAfter: 20, lineDelta: 1 } }, seen),
  );

  const output = await tool.execute(
    { path: "/etc/app.env", expectedRevision: REVISION, oldString: "PORT=8080", newString: "PORT=9090\nexport PORT" },
    context,
  );

  assert.equal(tool.name, "filesystem.edit");
  assert.equal(tool.risk, "medium");
  assert.equal(output.revision, REVISION);
  assert.equal(output.bytesAfter, 20);
  assert.equal(output.lineDelta, 1);
  assert.equal(seen[0]?.toolName, "filesystem.edit");
  assert.deepEqual(seen[0]?.target, target);
  assert.deepEqual(seen[0]?.input, {
    path: "/etc/app.env",
    expectedRevision: REVISION,
    oldString: "PORT=8080",
    newString: "PORT=9090\nexport PORT",
  });
});

test("filesystem.edit rejects an input the guard could never satisfy", () => {
  // These are the shapes the model must not be able to send at all: a revision that is not a
  // revision, an empty oldString (it occurs everywhere), and unknown fields.
  const input = filesystemEditTool(fakeHost({ status: "success", output: {} })).input;
  const valid = { path: "/etc/app.env", expectedRevision: REVISION, oldString: "a", newString: "b" };

  assert.equal(input.safeParse(valid).success, true);
  assert.equal(input.safeParse({ ...valid, expectedRevision: "not-a-revision" }).success, false);
  assert.equal(input.safeParse({ ...valid, oldString: "" }).success, false);
  assert.equal(input.safeParse({ ...valid, path: "relative/app.env" }).success, false);
  assert.equal(input.safeParse({ ...valid, extra: true }).success, false);
});

test("a stale revision survives the host boundary as a retryable invalid_input", async () => {
  // The code the edit guard's mismatch maps to on the Rust side (`filesystem_failure`). The Agent
  // must see it as a fixable input problem — re-read, then retry — with the drifted revisions in
  // `detail`, not as a transport failure or a dead end.
  const tool = filesystemEditTool(
    fakeHost({
      status: "failed",
      error: {
        code: "invalid_input",
        message: "the file is not the revision that was read: expected aa, the file is now bb; re-read the file and retry",
        retryable: true,
        detail: { expectedRevision: "aa", actualRevision: "bb" },
      },
    }),
  );

  await assert.rejects(
    tool.execute({ path: "/etc/app.env", expectedRevision: REVISION, oldString: "a", newString: "b" }, context),
    (error: unknown) =>
      error instanceof ToolFailure &&
      error.code === "invalid_input" &&
      error.retryable &&
      (error.detail as { actualRevision?: string }).actualRevision === "bb",
  );
});

test("a file over the edit cap is reported as unfixable, not as something to retry", async () => {
  const tool = filesystemEditTool(
    fakeHost({
      status: "failed",
      error: {
        code: "invalid_input",
        message: "the file is larger than the 524288-byte cap this tool can read in full",
        retryable: false,
        detail: { maxEditableBytes: 524_288 },
      },
    }),
  );

  await assert.rejects(
    tool.execute({ path: "/etc/app.env", expectedRevision: REVISION, oldString: "a", newString: "b" }, context),
    (error: unknown) =>
      error instanceof ToolFailure &&
      error.code === "invalid_input" &&
      !error.retryable &&
      (error.detail as { maxEditableBytes?: number }).maxEditableBytes === 524_288,
  );
});

test("filesystem.edit's description states the guard, the cap and the non-atomic window", () => {
  const tool = filesystemEditTool(fakeHost({ status: "success", output: {} }));
  // The description is user-facing prose and the model's only briefing. Losing any of these three
  // facts from it is a behaviour change, so they are pinned here.
  assert.match(tool.description, /expectedRevision/);
  assert.match(tool.description, /exactly once/);
  assert.match(tool.description, /512 KiB/);
  assert.match(tool.description, /compare-and-swap/);
  assert.match(tool.description, /filesystem\.write/);
});

test("docker restart is exposed as a high-risk host action", async () => {
  const tool = dockerRestartTool(
    fakeHost({ status: "success", output: { container: "api_1", restarted: true } }),
  );
  const output = await tool.execute({ container: "api_1", timeoutSeconds: 15 }, context);
  assert.equal(tool.risk, "high");
  assert.equal(output.restarted, true);
});
