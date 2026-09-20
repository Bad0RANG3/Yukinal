/** Persist a bounded phase artifact so a task can be resumed without guessing. */

import {
  INVESTIGATION_LIMITS,
  InvestigationArtifactSchema,
  TASK_ARTIFACT_KINDS,
  TASK_ARTIFACT_STATUSES,
  TASK_PHASES,
  type InvestigationArtifact,
} from "@yukinal/shared";
import { randomUUID } from "node:crypto";
import { z } from "zod";

import { ToolFailure, type Tool } from "../tool.js";
import type { HostRpcClient } from "../../transport/host-client.js";

const input = z.strictObject({
  id: z.string().trim().min(1).max(256).optional(),
  phase: z.enum(TASK_PHASES),
  kind: z.enum(TASK_ARTIFACT_KINDS),
  status: z.enum(TASK_ARTIFACT_STATUSES),
  title: z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxArtifactTitleChars),
  summary: z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxArtifactSummaryChars),
  content: z.unknown(),
  evidenceIds: z.array(z.string().trim().min(1).max(256)).max(INVESTIGATION_LIMITS.maxArtifactEvidenceIds),
});

export function investigationArtifactTool(host: HostRpcClient): Tool<z.infer<typeof input>, InvestigationArtifact> {
  return {
    name: "investigation.artifact",
    description:
      "Persist one bounded phase artifact for the current task. Use it for an execution plan, " +
      "change result, verification result or failure report; include evidence ids and a concise summary.",
    risk: "read",
    timeoutMs: 10_000,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    input,
    async execute(request, context) {
      if (!context.taskId) {
        throw new ToolFailure("investigation.artifact requires a durable task", "invalid_input", false);
      }
      const now = new Date().toISOString();
      const artifact: InvestigationArtifact = {
        id: request.id ?? `artifact_${randomUUID()}`,
        taskId: context.taskId,
        planId: context.planId,
        planStepId: context.planStepId,
        phase: request.phase,
        kind: request.kind,
        status: request.status,
        title: request.title,
        summary: request.summary,
        content: request.content,
        evidenceIds: request.evidenceIds,
        createdAt: now,
        updatedAt: now,
      };
      const response = await host.recordArtifact({
        artifact,
        planId: context.planId,
        planStepId: context.planStepId,
        evidenceIds: context.evidenceIds,
      }, context.signal);
      if (response.recorded) return InvestigationArtifactSchema.parse(response.artifact);
      throw new ToolFailure(response.error.message, response.error.code, response.error.retryable, response.error.detail);
    },
  };
}
