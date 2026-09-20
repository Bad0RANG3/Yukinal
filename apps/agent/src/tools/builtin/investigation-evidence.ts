/** Retrieve one previously persisted, redacted evidence item for the current task. */

import { EvidenceSchema } from "@yukinal/shared";
import { z } from "zod";

import { ToolFailure, type Tool } from "../tool.js";
import type { HostRpcClient } from "../../transport/host-client.js";

const input = z.strictObject({ evidenceId: z.string().trim().min(1).max(256) });

export function investigationEvidenceTool(host: HostRpcClient): Tool<z.infer<typeof input>, z.infer<typeof EvidenceSchema>> {
  return {
    name: "investigation.evidence",
    description:
      "Retrieve one bounded, already-redacted evidence item from the current investigation task. " +
      "Use the evidence id shown in the task context; this never reaches the remote host directly. " +
      "The host attaches freshness metadata; stale or expired evidence is historical context and must not be treated as current state.",
    risk: "read",
    timeoutMs: 10_000,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    input,
    async execute(request, context) {
      if (!context.taskId) {
        throw new ToolFailure("investigation.evidence requires a durable task", "invalid_input", false);
      }
      const response = await host.fetchEvidence({ taskId: context.taskId, evidenceId: request.evidenceId }, context.signal);
      if (response.status === "success") return EvidenceSchema.parse(response.evidence);
      if (response.status === "not_found") {
        throw new ToolFailure(`Evidence ${request.evidenceId} was not found in the current task`, "not_found", false);
      }
      throw new ToolFailure(response.error.message, response.error.code, response.error.retryable, response.error.detail);
    },
  };
}
