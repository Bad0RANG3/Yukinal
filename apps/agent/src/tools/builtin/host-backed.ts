/** Shared adapter for tools whose execution belongs to the Rust host. */

import type { HostToolExecuteRequest, HostToolExecuteResponse, RiskLevel, ToolError } from "@yukinal/shared";
import { ToolFailure, type Tool, type ToolContext } from "../tool.js";
import type { z } from "zod";

export interface HostToolExecutor {
  execute(request: HostToolExecuteRequest, signal?: AbortSignal): Promise<HostToolExecuteResponse>;
}

interface HostBackedToolSpec<TInput extends Record<string, unknown>, TOutput> {
  name: string;
  description: string;
  risk?: RiskLevel;
  timeoutMs: number;
  input: z.ZodType<TInput>;
  output: z.ZodType<TOutput>;
}

export function hostBackedTool<TInput extends Record<string, unknown>, TOutput>(
  host: HostToolExecutor,
  spec: HostBackedToolSpec<TInput, TOutput>,
): Tool<TInput, TOutput> {
  return {
    name: spec.name,
    description: spec.description,
    risk: spec.risk ?? "read",
    timeoutMs: spec.timeoutMs,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    input: spec.input,
    async execute(input: TInput, context: ToolContext): Promise<TOutput> {
      const response = await host.execute(
        {
          callId: context.callId,
          traceId: context.traceId,
          toolName: spec.name,
          input,
          target: context.target,
        },
        context.signal,
      );

      if (response.status !== "success") {
        const error = response.error ?? cancelledError();
        throw new ToolFailure(error.message, error.code, error.retryable, error.detail);
      }

      try {
        return spec.output.parse(response.output);
      } catch (error) {
        throw new ToolFailure(
          `Host returned invalid output for ${spec.name}`,
          "internal",
          false,
          error instanceof Error ? error.message : String(error),
        );
      }
    },
  };
}

function cancelledError(): ToolError {
  return { code: "cancelled", message: "Host cancelled the tool call", retryable: false };
}
