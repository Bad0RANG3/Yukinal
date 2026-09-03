/**
 * Tool Registry — registration + the only execution path (-R5).
 *
 * Invariants enforced here:
 *  1. a tool must declare name/description/risk/timeout/retry/cancellable
 *  2. the model never sees dot-names; the mapping is derived, not hand-written (ADR 0004)
 *  3. execution requires a ticket from the Permission Engine
 *  4. every call is timed, cancellable and traced (-R8)
 */

import { z } from "zod";

import {
  RPC_ERROR,
  isValidInternalToolName,
  type JsonSchema,
  type PermissionDecision,
  type ToolCallRequest,
  type ToolCallResult,
  type ToolDeclaration,
  type ToolError,
} from "@yukinal/shared";

import { NotImplementedError } from "../errors.js";
import type { TraceRecorder } from "../trace/trace-recorder.js";
import type { AnyTool, Tool } from "./tool.js";

export type ExecutionTicket =
  | { kind: "policy_auto"; decision: PermissionDecision }
  | { kind: "user_approved"; decision: PermissionDecision; approvalId: string; respondedAt: string };

export class DeniedByPolicyError extends NotImplementedError {}

export interface ExecuteOptions {
  /** External cancellation (user pressed Stop). */
  signal?: AbortSignal;
  trace?: TraceRecorder;
  /** Injectable clock for tests. */
  now?: () => number;
  log?: (message: string, meta?: Record<string, unknown>) => void;
}

export interface RegisterOptions {
  /** Replace an existing tool of the same name (used by hot-reload in dev). */
  allowOverride?: boolean;
}

const MAX_SUMMARY_CHARS = 4_000;

export class ToolRegistry {
  readonly #tools = new Map<string, AnyTool>();
  readonly #declarations = new Map<string, ToolDeclaration>();

  register<TInput extends Record<string, unknown>, TOutput>(
    tool: Tool<TInput, TOutput>,
    options: RegisterOptions = {},
  ): ToolDeclaration {
    if (!isValidInternalToolName(tool.name)) {
      throw new Error(
        `Tool name "${tool.name}" is not a valid dot-namespaced name (ADR 0004); refusing to register.`,
      );
    }
    if (!options.allowOverride && this.#tools.has(tool.name)) {
      throw new Error(`Tool "${tool.name}" is already registered`);
    }
    if (tool.timeoutMs <= 0) {
      throw new Error(`Tool "${tool.name}" must declare a positive timeoutMs`);
    }
    if (tool.description.trim().length === 0) {
      throw new Error(`Tool "${tool.name}" must declare a description the model can read`);
    }

    const declaration: ToolDeclaration = {
      name: tool.name,
      description: tool.description,
      risk: tool.risk,
      timeoutMs: tool.timeoutMs,
      cancellable: tool.cancellable,
      retry: tool.retry,
      inputSchema: toJsonSchema(tool.input),
      origin: { kind: "builtin" },
    };

    this.#tools.set(tool.name, tool as unknown as AnyTool);
    this.#declarations.set(tool.name, declaration);
    return declaration;
  }

  get(name: string): AnyTool | undefined {
    return this.#tools.get(name);
  }

  declaration(name: string): ToolDeclaration | undefined {
    return this.#declarations.get(name);
  }

  get size(): number {
    return this.#tools.size;
  }

  list(): ToolDeclaration[] {
    return [...this.#declarations.values()];
  }

  /**
   * Execute a call. Never throws for expected failures — they come back as a
   * `ToolCallResult` with an error, because the model must be able to read the
   * failure and continue reasoning.
   */
  async execute(request: ToolCallRequest, ticket: ExecutionTicket, options: ExecuteOptions = {}): Promise<ToolCallResult> {
    const now = options.now ?? Date.now;
    const startedAtMs = now();
    const startedAt = new Date(startedAtMs).toISOString();
    const base = { callId: request.callId, toolName: request.toolName, traceId: request.traceId };

    const tool = this.#tools.get(request.toolName);
    const declaration = this.#declarations.get(request.toolName);
    if (!tool || !declaration) {
      return failure(base, startedAt, now(), {
        code: "not_found",
        message: `Unknown tool "${request.toolName}". Available: ${this.list().map((d) => d.name).join(", ")}`,
        retryable: false,
      });
    }

    const gate = checkTicket(declaration, request, ticket);
    if (gate !== undefined) {
      return failure(base, startedAt, now(), gate);
    }

    const parsed = tool.input.safeParse(request.input);
    if (!parsed.success) {
      return failure(base, startedAt, now(), {
        code: "invalid_input",
        message: `Input for ${declaration.name} failed validation`,
        detail: parsed.error.issues.map((issue) => ({ path: issue.path.join("."), message: issue.message })),
        retryable: true,
      });
    }

    const step = options.trace?.startToolStep({
      title: humanTitle(declaration),
      toolName: declaration.name,
      callInput: request.input,
      intent: request.intent,
    });

    const controller = new AbortController();
    const abortFromCaller = () => controller.abort(new Error("cancelled-by-user"));
    options.signal?.addEventListener("abort", abortFromCaller, { once: true });

    const deadlineAt = startedAtMs + declaration.timeoutMs;
    const timer = setTimeout(() => controller.abort(new Error("timeout")), declaration.timeoutMs);
    timer.unref?.();

    let lastError: ToolError | undefined;
    let output: unknown;
    let succeeded = false;

    try {
      for (let attempt = 1; attempt <= Math.max(1, declaration.retry.maxAttempts); attempt += 1) {
        if (controller.signal.aborted) break;
        try {
          const running = tool.execute(parsed.data, {
            callId: request.callId,
            traceId: request.traceId,
            target: request.target,
            signal: controller.signal,
            deadlineAt,
            log: (message, meta) => options.log?.(`${declaration.name}: ${message}`, meta),
          });
          // The registry never waits on a tool that ignored cancellation: the call is
          // reported as timed out / cancelled immediately. The abandoned
          // promise is still observed so a misbehaving tool surfaces in logs instead of
          // silently finishing work the user already stopped.
          output = await Promise.race([running, interruption(controller.signal)]);
          void running.then(
            () => undefined,
            (error: unknown) => {
              options.log?.(`${declaration.name}: abandoned run rejected`, { error: String(error) });
            },
          );
          succeeded = true;
          break;
        } catch (error) {
          lastError = toToolError(error);
          const canRetry =
            lastError.retryable &&
            attempt < declaration.retry.maxAttempts &&
            !controller.signal.aborted &&
            now() < deadlineAt;
          if (!canRetry) break;
          await sleep(declaration.retry.backoffMs, controller.signal);
        }
      }
    } finally {
      clearTimeout(timer);
      options.signal?.removeEventListener("abort", abortFromCaller);
    }

    const endedAtMs = now();
    const cancelled = controller.signal.aborted && !succeeded;

    const result: ToolCallResult = succeeded
      ? {
          ...base,
          status: "success",
          output,
          outputSummary: summarize(output),
          startedAt,
          endedAt: new Date(endedAtMs).toISOString(),
          durationMs: endedAtMs - startedAtMs,
        }
      : failure(base, startedAt, endedAtMs, cancelled ? cancelledError(controller.signal) : (lastError ?? fallbackError()));

    if (options.trace && step) options.trace.finishToolStep(step.stepId, result);
    return result;
  }
}

export function checkTicket(
  declaration: ToolDeclaration,
  request: ToolCallRequest,
  ticket: ExecutionTicket,
): ToolError | undefined {
  const { decision } = ticket;
  if (decision.toolName !== declaration.name) {
    return {
      code: "denied_by_policy",
      message: `Decision was made for "${decision.toolName}", not "${declaration.name}"`,
      retryable: false,
    };
  }
  // The decision is bound to one target. Re-using it elsewhere is exactly the
  // cross-server mistake exists to prevent.
  if (!sameTarget(decision.target, request.target)) {
    return {
      code: "denied_by_policy",
      message: `Approval was granted for ${describe(decision.target)} but the call targets ${describe(request.target)}`,
      retryable: false,
    };
  }
  if (decision.finalRisk === "critical" && ticket.kind !== "user_approved") {
    return {
      code: "denied_by_policy",
      message: "Critical actions require an explicit user approval",
      retryable: false,
    };
  }
  if (decision.outcome === "deny") {
    return { code: "denied_by_policy", message: decision.reason, retryable: false };
  }
  if (ticket.kind === "policy_auto" && decision.outcome !== "auto") {
    return {
      code: "denied_by_policy",
      message: `Policy said "${decision.outcome}", but the call arrived with an auto ticket`,
      retryable: false,
    };
  }
  if (ticket.kind === "user_approved" && decision.approvalId !== ticket.approvalId) {
    return { code: "denied_by_policy", message: "Approval id does not match the pending decision", retryable: false };
  }
  return undefined;
}

function sameTarget(a: ToolCallRequest["target"], b: ToolCallRequest["target"]): boolean {
  return a.host === b.host && a.serverId === b.serverId && a.environment === b.environment;
}

function describe(target: ToolCallRequest["target"]): string {
  return `${target.host}:${target.serverId ?? "-"} (${target.environment})`;
}

class InterruptedError extends Error {
  constructor(readonly code: "timeout" | "cancelled") {
    super(code === "timeout" ? "Tool exceeded its timeout" : "Cancelled by user");
    this.name = "InterruptedError";
  }
}

/** Rejects as soon as the call's own AbortSignal fires. */
function interruption(signal: AbortSignal): Promise<never> {
  return new Promise<never>((_, reject) => {
    const fire = (): void => {
      const reason = String((signal.reason as Error | undefined)?.message ?? "abort");
      reject(new InterruptedError(reason === "timeout" ? "timeout" : "cancelled"));
    };
    if (signal.aborted) fire();
    else signal.addEventListener("abort", fire, { once: true });
  });
}

function toToolError(error: unknown): ToolError {
  if (error instanceof InterruptedError) {
    return {
      code: error.code,
      message: error.message,
      retryable: error.code === "timeout",
    };
  }
  if (error instanceof NotImplementedError) {
    return { code: "internal", message: error.message, retryable: false };
  }
  const shape = error as Partial<ToolError> & Error;
  if (shape?.code !== undefined) {
    return {
      code: shape.code,
      message: shape.message,
      detail: shape.detail,
      retryable: shape.retryable ?? false,
    };
  }
  return { code: "execution_failed", message: error instanceof Error ? error.message : String(error), retryable: false };
}

function cancelledError(signal: AbortSignal): ToolError {
  const reason = String((signal.reason as Error | undefined)?.message ?? "cancelled");
  return {
    code: reason === "timeout" ? "timeout" : "cancelled",
    message: reason === "timeout" ? "Tool exceeded its timeout" : "Cancelled by user",
    retryable: reason !== "timeout",
  };
}

function fallbackError(): ToolError {
  return { code: "internal", message: "Tool produced no result and no error", retryable: false };
}

function failure(
  base: { callId: string; toolName: string; traceId: string },
  startedAt: string,
  endedAtMs: number,
  error: ToolError,
): ToolCallResult {
  return {
    ...base,
    status: error.code === "cancelled" ? "cancelled" : "failed",
    error,
    startedAt,
    endedAt: new Date(endedAtMs).toISOString(),
    durationMs: endedAtMs - Date.parse(startedAt),
    outputSummary: error.message,
  };
}

function toJsonSchema(schema: z.ZodType): JsonSchema {
  return z.toJSONSchema(schema, { target: "draft-2020-12" }) as JsonSchema;
}

function humanTitle(declaration: ToolDeclaration): string {
  const [namespace, action] = declaration.name.split(".");
  if (!namespace || !action) return declaration.name;
  return `${namespace} ${action.replace(/-/g, " ")}`;
}

export function summarize(output: unknown): string {
  const text = typeof output === "string" ? output : JSON.stringify(output, null, 2) ?? String(output);
  return text.length > MAX_SUMMARY_CHARS ? `${text.slice(0, MAX_SUMMARY_CHARS)}\n…[truncated]` : text;
}

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  if (ms <= 0) return Promise.resolve();
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, ms);
    timer.unref?.();
    signal.addEventListener("abort", () => resolve(), { once: true });
  });
}

export const RPC_ERROR_CODES = RPC_ERROR;
export type { ToolCallRequest, ToolCallResult };
