/** Compare two persisted evidence bodies without returning either raw body. */

import {
  EvidenceComparisonInputSchema,
  EvidenceComparisonResultSchema,
  type EvidenceComparisonResult,
} from "@yukinal/shared";
import { z } from "zod";

import { ToolFailure, type Tool } from "../tool.js";
import type { HostRpcClient } from "../../transport/host-client.js";

export function investigationEvidenceCompareTool(
  host: Pick<HostRpcClient, "compareEvidence">,
): Tool<z.infer<typeof EvidenceComparisonInputSchema>, EvidenceComparisonResult> {
  return {
    name: "investigation.evidence.compare",
    description:
      "Compare two persisted, already-redacted evidence items from the current task. " +
      "The host returns metadata, freshness, changed JSON paths or bounded text counts, never raw values; " +
      "this is a local read-only comparison and does not collect a new sample or advance a plan step.",
    risk: "read",
    timeoutMs: 10_000,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    input: EvidenceComparisonInputSchema,
    async execute(request, context) {
      if (!context.taskId) {
        throw new ToolFailure("investigation.evidence.compare requires a durable task", "invalid_input", false);
      }
      const response = await host.compareEvidence({ taskId: context.taskId, ...request }, context.signal);
      if (response.status === "success") return EvidenceComparisonResultSchema.parse({ comparison: response.comparison });
      throw new ToolFailure(response.error.message, response.error.code, response.error.retryable, response.error.detail);
    },
  };
}
