import assert from "node:assert/strict";
import test from "node:test";

import {
  ServerExecInputSchema,
  ServerExecInterruptionSchema,
  ServerExecResultSchema,
  TaskCommandGrantSchema,
} from "./server-exec.js";

test("server.exec contract accepts bounded command input and rejects unknown host selectors", () => {
  const input = {
    command: "systemctl status api",
    purpose: "inspect current state",
    timeoutMs: 30_000,
    maxOutputBytes: 32_768,
    workdir: "/srv/app",
    env: { LANG: "C.UTF-8" },
  };
  assert.deepEqual(ServerExecInputSchema.parse(input), input);
  assert.equal(ServerExecInputSchema.safeParse({ ...input, serverId: "srv_other" }).success, false);
  assert.equal(ServerExecInputSchema.safeParse({ ...input, timeoutMs: 120_001 }).success, false);
});

test("server.exec schema validates without rewriting the approved command input", () => {
  const input = {
    command: "  printf '%s' exact  ",
    purpose: "  preserve the reviewed purpose  ",
    timeoutMs: 1_000,
    maxOutputBytes: 4_096,
    workdir: " /srv/app ",
  };
  assert.deepEqual(ServerExecInputSchema.parse(input), input);
});

test("server.exec result preserves nonzero exit and truncation state", () => {
  const result = {
    state: "completed",
    exitCode: 7,
    stdout: "partial",
    stderr: "failure",
    stdoutTruncated: true,
    stderrTruncated: false,
    durationMs: 12,
  } as const;
  assert.deepEqual(ServerExecResultSchema.parse(result), result);
});

test("task command grant and interruption contracts preserve host-owned bounds and uncertain outcomes", () => {
  const grant = {
    grantId: "cmdgrant_1",
    taskId: "task_1",
    serverId: "srv_01abc",
    environment: "staging",
    grantedBy: "user",
    grantedAt: "2026-09-30T00:00:00Z",
    expiresAt: "2026-09-30T04:00:00Z",
    maxCalls: 12,
    callsUsed: 2,
    maxTotalDurationMs: 900_000,
    totalDurationMs: 50_000,
    maxTotalOutputBytes: 1_048_576,
    totalOutputBytes: 65_536,
  } as const;
  assert.deepEqual(TaskCommandGrantSchema.parse(grant), grant);
  assert.equal(TaskCommandGrantSchema.safeParse({ ...grant, environment: "production" }).success, false);

  const interrupted = {
    state: "result_unknown",
    exitCode: null,
    stdout: "",
    stderr: "",
    stdoutTruncated: false,
    stderrTruncated: false,
    durationMs: 500,
  } as const;
  assert.deepEqual(ServerExecInterruptionSchema.parse(interrupted), interrupted);
});
