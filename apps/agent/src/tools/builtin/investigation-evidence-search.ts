/** Search persisted evidence metadata without injecting raw bodies into context. */

import {
  EvidenceSearchInputSchema,
  EvidenceSearchResultSchema,
  type HostEvidenceSearchRequest,
  type HostEvidenceSearchResponse,
  type EvidenceSearchResult,
} from "@yukinal/shared";
import { z } from "zod";

import { ToolFailure, type Tool } from "../tool.js";

interface EvidenceSearchClient {
  searchEvidence(request: HostEvidenceSearchRequest, signal?: AbortSignal): Promise<HostEvidenceSearchResponse>;
}

export function investigationEvidenceSearchTool(
  host: EvidenceSearchClient,
): Tool<z.infer<typeof EvidenceSearchInputSchema>, EvidenceSearchResult> {
  return {
    name: "investigation.evidence.search",
    description:
      "Search the current investigation's persisted evidence by source, kind, time window or exact task target. " +
      "The result contains metadata, ids and host-derived freshness only; use investigation.evidence with one id to retrieve a bounded redacted body. " +
      "Stale or expired rows are historical context, not proof of current state.",
    risk: "read",
    timeoutMs: 10_000,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    input: EvidenceSearchInputSchema,
    async execute(request, context) {
      if (!context.taskId) {
        throw new ToolFailure("investigation.evidence.search requires a durable task", "invalid_input", false);
      }
      const response = await host.searchEvidence({ taskId: context.taskId, ...request }, context.signal);
      if (response.status === "success") return EvidenceSearchResultSchema.parse({ evidence: response.evidence });
      throw new ToolFailure(response.error.message, response.error.code, response.error.retryable, response.error.detail);
    },
  };
}
