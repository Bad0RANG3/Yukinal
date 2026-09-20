/** Persist an evidence-linked fact, inference or unknown for the current task. */

import {
  FINDING_CONFIDENCE_LEVELS,
  FINDING_KINDS,
  FindingSchema,
  type Finding,
} from "@yukinal/shared";
import { randomUUID } from "node:crypto";
import { z } from "zod";

import { ToolFailure, type Tool } from "../tool.js";
import type { HostRpcClient } from "../../transport/host-client.js";

const input = z.strictObject({
  title: z.string().trim().min(1).max(512),
  kind: z.enum(FINDING_KINDS),
  statement: z.string().trim().min(1).max(16_384),
  evidenceIds: z.array(z.string().trim().min(1).max(256)).max(256),
  confidence: z.enum(FINDING_CONFIDENCE_LEVELS),
  nextVerification: z.string().trim().max(4_096).optional(),
});

export function investigationFindingTool(host: HostRpcClient): Tool<z.infer<typeof input>, Finding> {
  return {
    name: "investigation.finding",
    description:
      "Record an evidence-linked fact, inference or unknown for the current investigation. " +
      "Facts and inferences must cite evidence ids; do not present an unsupported guess as a fact.",
    risk: "read",
    timeoutMs: 10_000,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    input,
    async execute(request, context) {
      if (!context.taskId) throw new ToolFailure("investigation.finding requires a durable task", "invalid_input", false);
      const finding: Finding = {
        id: `finding_${randomUUID()}`,
        taskId: context.taskId,
        title: request.title,
        kind: request.kind,
        statement: request.statement,
        evidenceIds: request.evidenceIds,
        confidence: request.confidence,
        nextVerification: request.nextVerification,
        createdAt: new Date().toISOString(),
      };
      const response = await host.recordFinding({ finding }, context.signal);
      if (response.recorded) return FindingSchema.parse(response.finding);
      throw new ToolFailure(response.error.message, response.error.code, response.error.retryable, response.error.detail);
    },
  };
}
