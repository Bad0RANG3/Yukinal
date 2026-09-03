/**
 * Tool abstraction —, one of the day-1 interfaces.
 *
 * A tool is *declarative*: risk / timeout / retry / cancellable live on the object
 * so the registry, permission engine and trace UI can reason about them without
 * executing anything.
 */

import type { RetryPolicy, RiskLevel, ToolError, ToolTarget } from "@yukinal/shared";
import type { z } from "zod";

export interface ToolLogger {
  (message: string, meta?: Record<string, unknown>): void;
}

export interface ToolContext {
  callId: string;
  traceId: string;
  /** Resolved, stable target. A tool must never re-guess the server. */
  target: ToolTarget;
  /** Aborted on user Stop or timeout. Long-running work must check it. */
  signal: AbortSignal;
  /** Epoch ms after which the call is considered timed out. */
  deadlineAt: number;
  log: ToolLogger;
}

export interface Tool<TInput extends Record<string, unknown> = Record<string, unknown>, TOutput = unknown> {
  /** Internal dot-namespaced name (ADR 0004). */
  readonly name: string;
  readonly description: string;
  /** Base risk floor. */
  readonly risk: RiskLevel;
  readonly timeoutMs: number;
  readonly cancellable: boolean;
  readonly retry: RetryPolicy;
  /** Single source of truth: the JSON Schema the model sees is derived from this. */
  readonly input: z.ZodType<TInput>;
  execute(input: TInput, context: ToolContext): Promise<TOutput>;
}

/** Any tool, for storage in the registry. */
export type AnyTool = Tool<Record<string, unknown>, unknown>;

/**
 * Thrown by tool implementations. `code` maps onto the shared ToolError union so the
 * model and the UI see the same failure vocabulary.
 */
export class ToolFailure extends Error {
  constructor(
    message: string,
    readonly code: ToolError["code"] = "execution_failed",
    readonly retryable = false,
    readonly detail?: unknown,
  ) {
    super(message);
    this.name = "ToolFailure";
  }
}
