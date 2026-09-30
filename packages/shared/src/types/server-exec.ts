import type { Environment } from "./risk.js";

/** Cross-layer limits for the free-form remote command capability. */
export const SERVER_EXEC_LIMITS = {
  maxCommandChars: 16_384,
  maxPurposeChars: 512,
  maxTimeoutMs: 120_000,
  maxOutputBytes: 65_536,
  maxEnvironmentVariables: 16,
  maxWorkdirChars: 4_096,
} as const;

export interface ServerExecInput extends Record<string, unknown> {
  /** Raw remote shell text. SSH exec does not turn this into a safe argv. */
  command: string;
  /** Why this command is needed, shown in the task trace. */
  purpose: string;
  timeoutMs: number;
  /** Combined stdout + stderr byte limit. */
  maxOutputBytes: number;
  workdir?: string;
  /** Only non-sensitive, host-allowlisted variables are accepted. */
  env?: Record<string, string>;
}

export interface ServerExecResult {
  state: "completed";
  exitCode: number;
  stdout: string;
  stderr: string;
  stdoutTruncated: boolean;
  stderrTruncated: boolean;
  durationMs: number;
}

/** Host-issued, task-scoped authorization state. Counters are host-owned and persisted. */
export interface TaskCommandGrant {
  grantId: string;
  taskId: string;
  serverId: string;
  environment: Extract<Environment, "development" | "staging">;
  grantedBy: string;
  grantedAt: string;
  expiresAt: string;
  maxCalls: number;
  callsUsed: number;
  maxTotalDurationMs: number;
  totalDurationMs: number;
  maxTotalOutputBytes: number;
  totalOutputBytes: number;
}

/** A result state attached to a host error when no command result is available. */
export interface ServerExecInterruption {
  state: "timed_out" | "cancelled" | "result_unknown";
  exitCode: null;
  stdout: string;
  stderr: string;
  stdoutTruncated: boolean;
  stderrTruncated: boolean;
  durationMs: number;
}
