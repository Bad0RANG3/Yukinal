import { z } from "zod";

import { SERVER_EXEC_LIMITS } from "../types/server-exec.js";

export const ServerExecInputSchema = z.strictObject({
  // Preserve the exact approved input through ToolRegistry parsing; validation must not
  // silently transform the command after its one-time host approval fingerprint is made.
  command: z.string().min(1).max(SERVER_EXEC_LIMITS.maxCommandChars).refine((value) => value.trim().length > 0),
  purpose: z.string().min(1).max(SERVER_EXEC_LIMITS.maxPurposeChars).refine((value) => value.trim().length > 0),
  timeoutMs: z.number().int().min(1).max(SERVER_EXEC_LIMITS.maxTimeoutMs),
  maxOutputBytes: z.number().int().min(1).max(SERVER_EXEC_LIMITS.maxOutputBytes),
  workdir: z.string().min(1).max(SERVER_EXEC_LIMITS.maxWorkdirChars).refine((value) => value.trim().length > 0).optional(),
  env: z.record(z.string(), z.string().max(256)).optional(),
});

export const ServerExecResultSchema = z.strictObject({
  state: z.literal("completed"),
  exitCode: z.number().int().min(-1).max(255),
  stdout: z.string(),
  stderr: z.string(),
  stdoutTruncated: z.boolean(),
  stderrTruncated: z.boolean(),
  durationMs: z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER),
});

export const TaskCommandGrantSchema = z.strictObject({
  grantId: z.string().trim().min(1).max(256),
  taskId: z.string().trim().min(1).max(256),
  serverId: z.string().trim().min(1).max(256),
  environment: z.enum(["development", "staging"]),
  grantedBy: z.string().trim().min(1).max(256),
  grantedAt: z.string().trim().min(1).max(80),
  expiresAt: z.string().trim().min(1).max(80),
  maxCalls: z.number().int().positive().max(32),
  callsUsed: z.number().int().nonnegative().max(32),
  maxTotalDurationMs: z.number().int().positive().max(3_600_000),
  totalDurationMs: z.number().int().nonnegative().max(3_600_000),
  maxTotalOutputBytes: z.number().int().positive().max(4_194_304),
  totalOutputBytes: z.number().int().nonnegative().max(4_194_304),
});

export const ServerExecInterruptionSchema = z.strictObject({
  state: z.enum(["timed_out", "cancelled", "result_unknown"]),
  exitCode: z.null(),
  stdout: z.string(),
  stderr: z.string(),
  stdoutTruncated: z.boolean(),
  stderrTruncated: z.boolean(),
  durationMs: z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER),
});
