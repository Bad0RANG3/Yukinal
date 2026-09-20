/** Declare or replace the durable plan for the current investigation task. */

import {
  INVESTIGATION_LIMITS,
  InvestigationPlanSchema,
  OBSERVATION_WINDOW_STATUSES,
  PLAN_IDEMPOTENCY,
  PLAN_STEP_KINDS,
  RISK_LEVELS,
  type InvestigationPlan,
} from "@yukinal/shared";
import { randomUUID } from "node:crypto";
import { z } from "zod";

import { ToolFailure, type Tool } from "../tool.js";
import type { HostRpcClient } from "../../transport/host-client.js";
import { ToolTargetSchema } from "@yukinal/shared";

const stepInput = z.strictObject({
  id: z.string().trim().min(1).max(256).optional(),
  kind: z.enum(PLAN_STEP_KINDS),
  title: z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxPlanTextChars),
  purpose: z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxPlanTextChars),
  allowedTools: z.array(z.string().trim().min(1).max(256)).min(1).max(INVESTIGATION_LIMITS.maxPlanAllowedTools),
  inputBindings: z
    .record(
      z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxPlanInputBindingKeyChars),
      z.string().min(1).max(INVESTIGATION_LIMITS.maxPlanInputBindingValueChars),
    )
    .superRefine((bindings, context) => {
      if (Object.keys(bindings).length > INVESTIGATION_LIMITS.maxPlanInputBindings) {
        context.addIssue({
          code: z.ZodIssueCode.custom,
          message: `at most ${INVESTIGATION_LIMITS.maxPlanInputBindings} input bindings are allowed`,
        });
      }
    })
    .optional(),
  idempotency: z.enum(PLAN_IDEMPOTENCY).default("safe"),
  riskLevel: z.enum(RISK_LEVELS).optional(),
  requiresBaseline: z.boolean().default(false),
  preconditions: z
    .array(z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxSuccessCriterionChars))
    .max(INVESTIGATION_LIMITS.maxPlanStepCriteria)
    .default([]),
  verificationCriteria: z
    .array(z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxSuccessCriterionChars))
    .max(INVESTIGATION_LIMITS.maxPlanStepCriteria)
    .default([]),
  preview: z.string().max(INVESTIGATION_LIMITS.maxPlanTextChars).optional(),
  rollback: z.string().max(INVESTIGATION_LIMITS.maxPlanTextChars).optional(),
  target: ToolTargetSchema.optional(),
  evidenceIds: z.array(z.string().trim().min(1).max(256)).max(256).default([]),
  successCriteria: z
    .array(z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxSuccessCriterionChars))
    .max(INVESTIGATION_LIMITS.maxPlanStepCriteria)
    .default([]),
  requiresApproval: z.boolean().default(false),
  maxAttempts: z.number().int().positive().max(32).default(1),
});

const input = z.strictObject({
  steps: z.array(stepInput).min(1).max(INVESTIGATION_LIMITS.maxPlanSteps),
  observationWindow: z
    .strictObject({
      durationSeconds: z.number().int().positive().max(INVESTIGATION_LIMITS.maxObservationDurationSeconds),
      intervalSeconds: z.number().int().positive().max(INVESTIGATION_LIMITS.maxObservationIntervalSeconds),
      allowedTools: z.array(z.string().trim().min(1).max(256)).min(1).max(INVESTIGATION_LIMITS.maxObservationTools),
      successCriteria: z
        .array(z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxSuccessCriterionChars))
        .min(1)
        .max(INVESTIGATION_LIMITS.maxObservationCriteria),
    })
    .superRefine((window, context) => {
      if (window.intervalSeconds > window.durationSeconds) {
        context.addIssue({
          code: z.ZodIssueCode.custom,
          path: ["intervalSeconds"],
          message: "intervalSeconds must not exceed durationSeconds",
        });
      }
    })
    .optional(),
});

export function investigationPlanTool(host: HostRpcClient): Tool<z.infer<typeof input>, InvestigationPlan> {
  return {
    name: "investigation.plan",
    description:
      "Create the current investigation plan before using remote tools. Break the objective into ordered " +
      "evidence, decision, action and verification steps. Each step must list the exact tools it permits, " +
      "its purpose, evidence prerequisites, preconditions, risk, idempotency, preview, verification " +
      "and rollback details. Action steps must be reviewable before they can run. Re-plan when the host reports a deviation.",
    risk: "read",
    timeoutMs: 10_000,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    input,
    async execute(request, context) {
      if (!context.taskId) {
        throw new ToolFailure("investigation.plan requires a durable task", "invalid_input", false);
      }
      const now = new Date().toISOString();
      const plan: InvestigationPlan = {
        id: `plan_${randomUUID()}`,
        taskId: context.taskId,
        revision: 1,
        status: "active",
        createdAt: now,
        updatedAt: now,
        currentStepId: undefined,
        ...(request.observationWindow
          ? {
              observationWindow: {
                ...request.observationWindow,
                status: OBSERVATION_WINDOW_STATUSES[0],
                sampleCount: 0,
              },
            }
          : {}),
        steps: request.steps.map((step, index) => ({
          id: step.id ?? `plan_step_${randomUUID()}`,
          ordinal: index,
          kind: step.kind,
          title: step.title,
          purpose: step.purpose,
          allowedTools: step.allowedTools,
          inputBindings: step.inputBindings,
          idempotency: step.idempotency,
          riskLevel: step.riskLevel,
          requiresBaseline: step.requiresBaseline,
          preconditions: step.preconditions,
          verificationCriteria: step.verificationCriteria,
          preview: step.preview,
          rollback: step.rollback,
          target: step.target,
          evidenceIds: step.evidenceIds,
          successCriteria: step.successCriteria,
          requiresApproval: step.requiresApproval,
          maxAttempts: step.maxAttempts,
          attempts: 0,
          status: index === 0 ? "running" : "pending",
          startedAt: index === 0 ? now : undefined,
        })),
      };
      plan.currentStepId = plan.steps[0]?.id;
      const response = await host.recordPlan({ plan }, context.signal);
      if (response.recorded) return InvestigationPlanSchema.parse(response.plan);
      throw new ToolFailure(response.error.message, response.error.code, response.error.retryable, response.error.detail);
    },
  };
}
