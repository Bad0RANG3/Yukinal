/** Persist a user-facing decision brief after the current task has enough evidence. */

import {
  DECISION_OPTION_STATUSES,
  DECISION_OPTION_CONTINUATIONS,
  DecisionBriefSchema,
  RISK_LEVELS,
  type DecisionBrief,
  type DecisionOption,
} from "@yukinal/shared";
import { randomUUID } from "node:crypto";
import { z } from "zod";

import { ToolFailure, type Tool } from "../tool.js";
import type { HostRpcClient } from "../../transport/host-client.js";

const optionInput = z.strictObject({
  title: z.string().trim().min(1).max(512),
  summary: z.string().trim().min(1).max(8_192),
  impact: z.string().trim().min(1).max(8_192),
  riskLevel: z.enum(RISK_LEVELS),
  evidenceIds: z.array(z.string().trim().min(1).max(256)).max(256),
  findingIds: z.array(z.string().trim().min(1).max(256)).max(256),
  preview: z.string().max(16_384).optional(),
  verification: z.string().trim().min(1).max(8_192),
  rollback: z.string().trim().max(8_192).optional(),
  requiresApproval: z.boolean(),
  status: z.enum(DECISION_OPTION_STATUSES).optional(),
  continuation: z.enum(DECISION_OPTION_CONTINUATIONS).optional(),
});

const input = z.strictObject({
  findingIds: z.array(z.string().trim().min(1).max(256)).max(256),
  options: z.array(optionInput).max(16),
});

export function investigationBriefTool(host: HostRpcClient): Tool<z.infer<typeof input>, DecisionBrief> {
  return {
    name: "investigation.brief",
    description:
      "Record a decision brief for the current investigation. Include a conservative option, cite the " +
      "findings and evidence for every option, and state verification and rollback details before asking the user to choose. " +
      "Only set continuation when the selected option should explicitly continue: continue_readonly for a bounded read-only follow-up, " +
      "start_plan for a newly approved plan, stop to stop, or omit it to wait for the user.",
    risk: "read",
    timeoutMs: 10_000,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    input,
    async execute(request, context) {
      if (!context.taskId) throw new ToolFailure("investigation.brief requires a durable task", "invalid_input", false);
      const options: DecisionOption[] = request.options.map((option) => ({
        id: `option_${randomUUID()}`,
        title: option.title,
        summary: option.summary,
        impact: option.impact,
        riskLevel: option.riskLevel,
        evidenceIds: option.evidenceIds,
        findingIds: option.findingIds,
        preview: option.preview,
        verification: option.verification,
        rollback: option.rollback,
        requiresApproval: option.requiresApproval,
        status: option.status ?? "available",
        ...(option.continuation ? { continuation: option.continuation } : {}),
      }));
      const brief: DecisionBrief = {
        id: `brief_${randomUUID()}`,
        taskId: context.taskId,
        planId: context.planId,
        generatedAt: new Date().toISOString(),
        status: "presented",
        findingIds: request.findingIds,
        options,
      };
      const response = await host.recordBrief({ brief }, context.signal);
      if (response.recorded) return DecisionBriefSchema.parse(response.brief);
      throw new ToolFailure(response.error.message, response.error.code, response.error.retryable, response.error.detail);
    },
  };
}
