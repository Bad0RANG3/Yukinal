/** Group one persisted observation with bounded metadata from the same run/time window. */

import {
  EvidenceCorrelationInputSchema,
  EvidenceCorrelationResultSchema,
  type EvidenceCorrelationResult,
} from "@yukinal/shared";
import { z } from "zod";

import { ToolFailure, type Tool } from "../tool.js";
import type { HostRpcClient } from "../../transport/host-client.js";

export function investigationEvidenceCorrelateTool(
  host: Pick<HostRpcClient, "correlateEvidence">,
): Tool<z.infer<typeof EvidenceCorrelationInputSchema>, EvidenceCorrelationResult> {
  return {
    name: "investigation.evidence.correlate",
    description:
      "Group one persisted evidence item with related metadata from the same host-owned run, " +
      "or a bounded time window when no run id exists. The host enforces the current task and exact target scope; " +
      "the result contains summaries only, never raw bodies or a claim of causal root cause.",
    risk: "read",
    timeoutMs: 10_000,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    input: EvidenceCorrelationInputSchema,
    async execute(request, context) {
      if (!context.taskId) {
        throw new ToolFailure("investigation.evidence.correlate requires a durable task", "invalid_input", false);
      }
      const response = await host.correlateEvidence({ taskId: context.taskId, ...request }, context.signal);
      if (response.status === "success") return EvidenceCorrelationResultSchema.parse({ correlation: response.correlation });
      throw new ToolFailure(response.error.message, response.error.code, response.error.retryable, response.error.detail);
    },
  };
}
