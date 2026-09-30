/** `server.exec` — bounded, host-dispatched remote shell command execution. */

import {
  SERVER_EXEC_LIMITS,
  ServerExecInputSchema,
  ServerExecResultSchema,
  type HostToolExecuteRequest,
  type HostToolExecuteResponse,
  type ServerExecInput,
  type ServerExecResult,
} from "@yukinal/shared";

import { ToolFailure, type Tool, type ToolContext } from "../tool.js";
import type { HostToolExecutor } from "./host-backed.js";

export function serverExecTool(host: HostToolExecutor): Tool<ServerExecInput, ServerExecResult> {
  return {
    name: "server.exec",
    description:
      "Run one bounded command on the already resolved remote server. The command is interpreted by the remote login shell; it is always high risk unless this exact task has an active user-issued command delegation. State the purpose, keep the command focused, and verify nonzero exit codes.",
    risk: "high",
    timeoutMs: SERVER_EXEC_LIMITS.maxTimeoutMs + 10_000,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    effectful: true,
    input: ServerExecInputSchema,
    async execute(input: ServerExecInput, context: ToolContext): Promise<ServerExecResult> {
      const request: HostToolExecuteRequest = {
        callId: context.callId,
        traceId: context.traceId,
        ...(context.runId ? { runId: context.runId } : {}),
        toolName: "server.exec",
        input,
        target: context.target,
        ...(context.taskId ? { taskId: context.taskId } : {}),
        ...(context.planId ? { planId: context.planId } : {}),
        ...(context.planStepId ? { planStepId: context.planStepId } : {}),
        ...(context.evidenceIds ? { evidenceIds: context.evidenceIds } : {}),
        ...(context.approvalId ? { approvalId: context.approvalId } : {}),
      };
      const response: HostToolExecuteResponse = await host.execute(request, context.signal);
      if (response.status !== "success") {
        const error = response.error;
        throw new ToolFailure(
          error?.message ?? "Host cancelled the remote command",
          error?.code ?? "cancelled",
          error?.retryable ?? false,
          error?.detail,
        );
      }

      const result = ServerExecResultSchema.parse(response.output);
      if (result.exitCode !== 0) {
        throw new ToolFailure(
          `Remote command exited with code ${result.exitCode}; inspect the bounded stdout/stderr result before deciding whether to retry.`,
          "execution_failed",
          false,
          result,
        );
      }
      return result;
    },
  };
}
