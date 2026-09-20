import { z } from "zod";

import {
  DECISION_BRIEF_STATUSES,
  DECISION_OPTION_CONTINUATIONS,
  DECISION_OPTION_STATUSES,
  EVIDENCE_CORRELATION_MATCHES,
  EVIDENCE_COMPARISON_SHAPES,
  EVIDENCE_COMPARISON_STATUSES,
  EVIDENCE_CONTENT_TYPES,
  EVIDENCE_FRESHNESS_STATUSES,
  EVIDENCE_KINDS,
  EVIDENCE_REDACTION_STATUSES,
  FINDING_CONFIDENCE_LEVELS,
  FINDING_KINDS,
  FAILURE_OPTION_ACTIONS,
  INVESTIGATION_LIMITS,
  PLAN_DEVIATION_ACTIONS,
  PLAN_DEVIATION_CODES,
  PLAN_IDEMPOTENCY,
  PLAN_APPROVAL_SOURCES,
  PLAN_APPROVAL_STATUSES,
  PLAN_STATUSES,
  PLAN_STEP_KINDS,
  PLAN_STEP_STATUSES,
  OBSERVATION_WINDOW_STATUSES,
  TASK_AUTOMATION_LEVELS,
  TASK_ARTIFACT_KINDS,
  TASK_ARTIFACT_STATUSES,
  TASK_FAILURE_CODES,
  TASK_PHASES,
  TASK_RUN_STATUSES,
  TASK_STEP_KINDS,
  TASK_STEP_STATUSES,
  TASK_STATUSES,
} from "../types/investigation.js";
import { AGENT_PERMISSION_MODES, AGENT_RUN_MODES, RISK_LEVELS } from "../types/risk.js";
import { ToolTargetSchema } from "./server.js";

const OPAQUE_ID = z.string().trim().min(1).max(256);
const HASH = z.string().regex(/^[a-f0-9]{64}$/, "contentHash must be a lowercase SHA-256 hex digest");

export const TaskBudgetSchema = z.strictObject({
  maxSteps: z.number().int().positive().max(10_000),
  maxRunMs: z.number().int().positive().max(86_400_000),
  maxAttempts: z.number().int().positive().max(32),
});

export const InvestigationFailureSchema = z.strictObject({
  code: z.enum(TASK_FAILURE_CODES),
  message: z.string().max(INVESTIGATION_LIMITS.maxFailureMessageChars),
  retryable: z.boolean(),
  attempt: z.number().int().positive().max(32),
  at: z.string().trim().min(1).max(80),
  detail: z.unknown().optional(),
  options: z.array(z.strictObject({
    id: OPAQUE_ID,
    action: z.enum(FAILURE_OPTION_ACTIONS),
    title: z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxFailureOptionTextChars),
    description: z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxFailureOptionTextChars),
    requiresApproval: z.boolean(),
  })).max(INVESTIGATION_LIMITS.maxFailureOptions).optional(),
});

export const InvestigationRunSchema = z.strictObject({
  id: OPAQUE_ID,
  taskId: OPAQUE_ID,
  sessionId: OPAQUE_ID.optional(),
  messageId: OPAQUE_ID.optional(),
  traceId: OPAQUE_ID.optional(),
  attempt: z.number().int().positive().max(32),
  phase: z.enum(TASK_PHASES),
  status: z.enum(TASK_RUN_STATUSES),
  startedAt: z.string().trim().min(1).max(80),
  updatedAt: z.string().trim().min(1).max(80),
  endedAt: z.string().trim().min(1).max(80).optional(),
  checkpoint: z.unknown().optional(),
  failure: InvestigationFailureSchema.optional(),
});

export const InvestigationStepSchema = z.strictObject({
  id: OPAQUE_ID,
  taskId: OPAQUE_ID,
  runId: OPAQUE_ID,
  ordinal: z.number().int().nonnegative().max(10_000),
  kind: z.enum(TASK_STEP_KINDS),
  title: z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxStepTitleChars),
  status: z.enum(TASK_STEP_STATUSES),
  attempt: z.number().int().positive().max(32),
  toolName: z.string().trim().min(1).max(256).optional(),
  planId: OPAQUE_ID.optional(),
  planStepId: OPAQUE_ID.optional(),
  target: ToolTargetSchema.optional(),
  inputSummary: z.string().max(INVESTIGATION_LIMITS.maxStepSummaryChars).optional(),
  outputSummary: z.string().max(INVESTIGATION_LIMITS.maxStepSummaryChars).optional(),
  evidenceIds: z.array(OPAQUE_ID).max(256),
  startedAt: z.string().trim().min(1).max(80).optional(),
  endedAt: z.string().trim().min(1).max(80).optional(),
  failure: InvestigationFailureSchema.optional(),
});

export const InvestigationPlanDeviationSchema = z.strictObject({
  code: z.enum(PLAN_DEVIATION_CODES),
  action: z.enum(PLAN_DEVIATION_ACTIONS),
  message: z.string().max(INVESTIGATION_LIMITS.maxPlanDeviationMessageChars),
  toolName: z.string().trim().min(1).max(256),
  at: z.string().trim().min(1).max(80),
  planId: OPAQUE_ID.optional(),
  stepId: OPAQUE_ID.optional(),
});

export const InvestigationPlanApprovalSchema = z.strictObject({
  status: z.enum(PLAN_APPROVAL_STATUSES),
  source: z.enum(PLAN_APPROVAL_SOURCES).optional(),
  optionId: OPAQUE_ID.optional(),
  approvedAt: z.string().trim().min(1).max(80).optional(),
  note: z.string().max(INVESTIGATION_LIMITS.maxPlanTextChars).optional(),
});

export const InvestigationObservationWindowSchema = z
  .strictObject({
    durationSeconds: z.number().int().positive().max(INVESTIGATION_LIMITS.maxObservationDurationSeconds),
    intervalSeconds: z.number().int().positive().max(INVESTIGATION_LIMITS.maxObservationIntervalSeconds),
    allowedTools: z.array(z.string().trim().min(1).max(256)).min(1).max(INVESTIGATION_LIMITS.maxObservationTools),
    successCriteria: z
      .array(z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxSuccessCriterionChars))
      .min(1)
      .max(INVESTIGATION_LIMITS.maxObservationCriteria),
    status: z.enum(OBSERVATION_WINDOW_STATUSES),
    sampleCount: z.number().int().nonnegative().max(100_000),
    startedAt: z.string().trim().min(1).max(80).optional(),
    deadlineAt: z.string().trim().min(1).max(80).optional(),
    deadlineEpochSeconds: z.number().int().nonnegative().optional(),
    lastSampleAt: z.string().trim().min(1).max(80).optional(),
    lastSampleEpochSeconds: z.number().int().nonnegative().optional(),
    lastFailure: z.string().max(INVESTIGATION_LIMITS.maxFailureMessageChars).optional(),
  })
  .superRefine((window, context) => {
    if (window.intervalSeconds > window.durationSeconds) {
      context.addIssue({
        code: z.ZodIssueCode.custom,
        path: ["intervalSeconds"],
        message: "intervalSeconds must not exceed durationSeconds",
      });
    }
  });

export const InvestigationPlanStepSchema = z.strictObject({
  id: OPAQUE_ID,
  ordinal: z.number().int().nonnegative().max(10_000),
  kind: z.enum(PLAN_STEP_KINDS),
  title: z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxPlanTextChars),
  purpose: z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxPlanTextChars),
  allowedTools: z.array(z.string().trim().min(1).max(256)).max(INVESTIGATION_LIMITS.maxPlanAllowedTools),
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
  idempotency: z.enum(PLAN_IDEMPOTENCY).optional(),
  riskLevel: z.enum(RISK_LEVELS).optional(),
  requiresBaseline: z.boolean().optional(),
  preconditions: z.array(z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxSuccessCriterionChars)).max(INVESTIGATION_LIMITS.maxPlanStepCriteria).optional(),
  verificationCriteria: z.array(z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxSuccessCriterionChars)).max(INVESTIGATION_LIMITS.maxPlanStepCriteria).optional(),
  preview: z.string().max(INVESTIGATION_LIMITS.maxPlanTextChars).optional(),
  rollback: z.string().max(INVESTIGATION_LIMITS.maxPlanTextChars).optional(),
  target: ToolTargetSchema.optional(),
  evidenceIds: z.array(OPAQUE_ID).max(256),
  successCriteria: z
    .array(z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxSuccessCriterionChars))
    .max(INVESTIGATION_LIMITS.maxPlanStepCriteria),
  requiresApproval: z.boolean(),
  maxAttempts: z.number().int().positive().max(32),
  attempts: z.number().int().nonnegative().max(32),
  status: z.enum(PLAN_STEP_STATUSES),
  startedAt: z.string().trim().min(1).max(80).optional(),
  endedAt: z.string().trim().min(1).max(80).optional(),
  lastDeviation: InvestigationPlanDeviationSchema.optional(),
});

export const InvestigationPlanSchema = z.strictObject({
  id: OPAQUE_ID,
  taskId: OPAQUE_ID,
  revision: z.number().int().positive().max(10_000),
  status: z.enum(PLAN_STATUSES),
  createdAt: z.string().trim().min(1).max(80),
  updatedAt: z.string().trim().min(1).max(80),
  currentStepId: OPAQUE_ID.optional(),
  approval: InvestigationPlanApprovalSchema.optional(),
  observationWindow: InvestigationObservationWindowSchema.optional(),
  steps: z.array(InvestigationPlanStepSchema).min(1).max(INVESTIGATION_LIMITS.maxPlanSteps),
});

export const InvestigationTaskSchema = z.strictObject({
  id: OPAQUE_ID,
  workspaceId: OPAQUE_ID.optional(),
  serverId: OPAQUE_ID.optional(),
  objective: z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxObjectiveChars),
  successCriteria: z
    .array(z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxSuccessCriterionChars))
    .min(1)
    .max(INVESTIGATION_LIMITS.maxSuccessCriteria),
  scope: ToolTargetSchema,
  guardrails: z
    .strictObject({
      notBeforeAt: z.string().trim().min(1).max(80).optional(),
      expiresAt: z.string().trim().min(1).max(80).optional(),
      forbiddenTools: z
        .array(z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxGuardrailNameChars))
        .max(INVESTIGATION_LIMITS.maxGuardrailTools),
      forbiddenPathPrefixes: z
        .array(z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxGuardrailPathChars))
        .max(INVESTIGATION_LIMITS.maxGuardrailPathPrefixes),
    })
    .default({ forbiddenTools: [], forbiddenPathPrefixes: [] }),
  mode: z.enum(AGENT_RUN_MODES),
  permissionMode: z.enum(AGENT_PERMISSION_MODES),
  automationLevel: z.enum(TASK_AUTOMATION_LEVELS),
  createdBy: z.string().trim().min(1).max(256),
  phase: z.enum(TASK_PHASES),
  status: z.enum(TASK_STATUSES),
  budget: TaskBudgetSchema,
  createdAt: z.string().trim().min(1).max(80),
  updatedAt: z.string().trim().min(1).max(80),
  completedAt: z.string().trim().min(1).max(80).optional(),
  activeRunId: OPAQUE_ID.optional(),
  lastFailure: InvestigationFailureSchema.optional(),
});

export const EvidenceSchema = z.strictObject({
  id: OPAQUE_ID,
  taskId: OPAQUE_ID,
  runId: OPAQUE_ID.optional(),
  scope: ToolTargetSchema,
  kind: z.enum(EVIDENCE_KINDS),
  sourceTool: z.string().trim().min(1).max(256),
  collectedAt: z.string().trim().min(1).max(80),
  inputSummary: z.string().max(INVESTIGATION_LIMITS.maxEvidenceInputSummaryChars),
  contentType: z.enum(EVIDENCE_CONTENT_TYPES),
  content: z.unknown(),
  contentHash: HASH,
  truncated: z.boolean(),
  redactionStatus: z.enum(EVIDENCE_REDACTION_STATUSES),
  freshness: z
    .strictObject({
      status: z.enum(EVIDENCE_FRESHNESS_STATUSES),
      policy: z.literal("default-v1"),
      evaluatedAt: z.string().trim().min(1).max(80),
      ageSeconds: z.number().int().nonnegative().optional(),
      staleAfterSeconds: z.number().int().positive(),
      expiresAfterSeconds: z.number().int().positive(),
      reason: z.string().trim().max(256).optional(),
    })
    .optional(),
});

export const EvidenceSummarySchema = z.strictObject({
  id: OPAQUE_ID,
  taskId: OPAQUE_ID,
  runId: OPAQUE_ID.optional(),
  scope: ToolTargetSchema,
  kind: z.enum(EVIDENCE_KINDS),
  sourceTool: z.string().trim().min(1).max(256),
  collectedAt: z.string().trim().min(1).max(80),
  inputSummary: z.string().max(INVESTIGATION_LIMITS.maxEvidenceInputSummaryChars),
  contentType: z.enum(EVIDENCE_CONTENT_TYPES),
  contentHash: HASH,
  truncated: z.boolean(),
  redactionStatus: z.enum(EVIDENCE_REDACTION_STATUSES),
  freshness: z
    .strictObject({
      status: z.enum(EVIDENCE_FRESHNESS_STATUSES),
      policy: z.literal("default-v1"),
      evaluatedAt: z.string().trim().min(1).max(80),
      ageSeconds: z.number().int().nonnegative().optional(),
      staleAfterSeconds: z.number().int().positive(),
      expiresAfterSeconds: z.number().int().positive(),
      reason: z.string().trim().max(256).optional(),
    })
    .optional(),
});

export const EvidenceSearchInputSchema = z.strictObject({
  sourceTool: z.string().trim().min(1).max(256).optional(),
  kind: z.enum(EVIDENCE_KINDS).optional(),
  from: z.string().trim().min(1).max(80).optional(),
  to: z.string().trim().min(1).max(80).optional(),
  target: ToolTargetSchema.optional(),
  limit: z.number().int().min(1).max(64).optional(),
});

export const EvidenceSearchResultSchema = z.strictObject({
  evidence: z.array(EvidenceSummarySchema).max(64),
});

export const EvidenceCorrelationInputSchema = z.strictObject({
  anchorEvidenceId: OPAQUE_ID,
  windowSeconds: z.number().int().positive().max(3_600).optional(),
  limit: z.number().int().positive().max(64).optional(),
});

export const EvidenceCorrelationSchema = z.strictObject({
  anchor: EvidenceSummarySchema,
  evidence: z.array(EvidenceSummarySchema).max(64),
  matchedBy: z.enum(EVIDENCE_CORRELATION_MATCHES),
  windowSeconds: z.number().int().positive().max(3_600),
  sourceTools: z.array(z.string().trim().min(1).max(256)).max(32),
  warnings: z.array(z.string().trim().min(1).max(512)).max(8),
});

export const EvidenceCorrelationResultSchema = z.strictObject({
  correlation: EvidenceCorrelationSchema,
});

export const EvidenceComparisonInputSchema = z.strictObject({
  leftEvidenceId: OPAQUE_ID,
  rightEvidenceId: OPAQUE_ID,
});

export const EvidenceTextComparisonSchema = z.strictObject({
  leftLineCount: z.number().int().nonnegative().max(100_000),
  rightLineCount: z.number().int().nonnegative().max(100_000),
  changedLineCount: z.number().int().nonnegative().max(100_000),
  addedLineCount: z.number().int().nonnegative().max(100_000),
  removedLineCount: z.number().int().nonnegative().max(100_000),
});

export const EvidenceComparisonSchema = z.strictObject({
  status: z.enum(EVIDENCE_COMPARISON_STATUSES),
  shape: z.enum(EVIDENCE_COMPARISON_SHAPES),
  left: EvidenceSummarySchema,
  right: EvidenceSummarySchema,
  changedPaths: z.array(z.string().min(1).max(512)).max(64),
  changedPathCount: z.number().int().nonnegative().max(10_000),
  diffTruncated: z.boolean(),
  text: EvidenceTextComparisonSchema.optional(),
  warnings: z.array(z.string().trim().min(1).max(512)).max(8),
});

export const EvidenceComparisonResultSchema = z.strictObject({
  comparison: EvidenceComparisonSchema,
});

export const FindingSchema = z.strictObject({
  id: OPAQUE_ID,
  taskId: OPAQUE_ID,
  title: z.string().trim().min(1).max(512),
  kind: z.enum(FINDING_KINDS),
  statement: z.string().trim().min(1).max(16_384),
  evidenceIds: z.array(OPAQUE_ID).max(256),
  confidence: z.enum(FINDING_CONFIDENCE_LEVELS),
  nextVerification: z.string().trim().max(4_096).optional(),
  createdAt: z.string().trim().min(1).max(80),
});

export const DecisionOptionSchema = z.strictObject({
  id: OPAQUE_ID,
  title: z.string().trim().min(1).max(512),
  summary: z.string().trim().min(1).max(8_192),
  impact: z.string().trim().min(1).max(8_192),
  riskLevel: z.enum(RISK_LEVELS),
  evidenceIds: z.array(OPAQUE_ID).max(256),
  findingIds: z.array(OPAQUE_ID).max(256),
  preview: z.string().max(16_384).optional(),
  verification: z.string().trim().min(1).max(8_192),
  rollback: z.string().trim().max(8_192).optional(),
  requiresApproval: z.boolean(),
  status: z.enum(DECISION_OPTION_STATUSES),
  continuation: z.enum(DECISION_OPTION_CONTINUATIONS).optional(),
});

export const DecisionBriefSchema = z.strictObject({
  id: OPAQUE_ID,
  taskId: OPAQUE_ID,
  planId: OPAQUE_ID.optional(),
  generatedAt: z.string().trim().min(1).max(80),
  status: z.enum(DECISION_BRIEF_STATUSES),
  findingIds: z.array(OPAQUE_ID).max(256),
  options: z.array(DecisionOptionSchema).max(INVESTIGATION_LIMITS.maxDecisionOptions),
  selectedOptionId: OPAQUE_ID.optional(),
});

export const InvestigationArtifactSchema = z.strictObject({
  id: OPAQUE_ID,
  taskId: OPAQUE_ID,
  runId: OPAQUE_ID.optional(),
  planId: OPAQUE_ID.optional(),
  planStepId: OPAQUE_ID.optional(),
  phase: z.enum(TASK_PHASES),
  kind: z.enum(TASK_ARTIFACT_KINDS),
  status: z.enum(TASK_ARTIFACT_STATUSES),
  title: z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxArtifactTitleChars),
  summary: z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxArtifactSummaryChars),
  content: z.unknown(),
  evidenceIds: z.array(OPAQUE_ID).max(INVESTIGATION_LIMITS.maxArtifactEvidenceIds),
  createdAt: z.string().trim().min(1).max(80),
  updatedAt: z.string().trim().min(1).max(80),
});

export const InvestigationArtifactSummarySchema = InvestigationArtifactSchema.omit({ content: true });

export const InvestigationTaskCreateInputSchema = z.strictObject({
  id: OPAQUE_ID.optional(),
  workspaceId: OPAQUE_ID.optional(),
  serverId: OPAQUE_ID.optional(),
  objective: z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxObjectiveChars),
  successCriteria: z
    .array(z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxSuccessCriterionChars))
    .min(1)
    .max(INVESTIGATION_LIMITS.maxSuccessCriteria),
  scope: ToolTargetSchema,
  guardrails: z
    .strictObject({
      notBeforeAt: z.string().trim().min(1).max(80).optional(),
      expiresAt: z.string().trim().min(1).max(80).optional(),
      forbiddenTools: z
        .array(z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxGuardrailNameChars))
        .max(INVESTIGATION_LIMITS.maxGuardrailTools),
      forbiddenPathPrefixes: z
        .array(z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxGuardrailPathChars))
        .max(INVESTIGATION_LIMITS.maxGuardrailPathPrefixes),
    })
    .optional(),
  mode: z.enum(AGENT_RUN_MODES),
  permissionMode: z.enum(AGENT_PERMISSION_MODES),
  automationLevel: z.enum(TASK_AUTOMATION_LEVELS),
  budget: TaskBudgetSchema.optional(),
});

export const InvestigationTaskListInputSchema = z.strictObject({
  status: z.enum(TASK_STATUSES).optional(),
  limit: z.number().int().min(1).max(100).optional(),
});

export const InvestigationTaskListResponseSchema = z.strictObject({
  tasks: z.array(InvestigationTaskSchema),
});

export const InvestigationTaskResponseSchema = z.strictObject({ task: InvestigationTaskSchema });

export const InvestigationTaskStartResponseSchema = z.strictObject({
  runId: OPAQUE_ID,
  started: z.boolean(),
});

export const InvestigationTaskDetailResponseSchema = z.strictObject({
  task: InvestigationTaskSchema,
  evidence: z.array(EvidenceSchema),
  findings: z.array(FindingSchema),
  decisionBrief: DecisionBriefSchema.optional(),
  plan: InvestigationPlanSchema.optional(),
  artifacts: z.array(InvestigationArtifactSchema),
  runs: z.array(InvestigationRunSchema),
  steps: z.array(InvestigationStepSchema),
});

export const InvestigationContextSchema = z.strictObject({
  task: InvestigationTaskSchema,
  evidence: z.array(EvidenceSummarySchema).max(64),
  findings: z.array(FindingSchema).max(INVESTIGATION_LIMITS.maxFindingsPerTask),
  decisionBrief: DecisionBriefSchema.optional(),
  plan: InvestigationPlanSchema.optional(),
  artifacts: z.array(InvestigationArtifactSummarySchema).max(128),
  runs: z.array(InvestigationRunSchema).max(INVESTIGATION_LIMITS.maxRunsPerTask),
  steps: z.array(InvestigationStepSchema).max(INVESTIGATION_LIMITS.maxStepsPerTask),
});

export const InvestigationTaskStatusUpdateInputSchema = z.strictObject({
  taskId: OPAQUE_ID,
  status: z.enum(TASK_STATUSES),
  completedAt: z.string().trim().min(1).max(80).optional(),
});

export const InvestigationTaskStopInputSchema = z.strictObject({ taskId: OPAQUE_ID });

export const InvestigationBriefSelectInputSchema = z.strictObject({
  taskId: OPAQUE_ID,
  briefId: OPAQUE_ID,
  optionId: OPAQUE_ID,
});

export const InvestigationBriefResponseSchema = z.strictObject({
  brief: DecisionBriefSchema,
  continuation: z.enum(DECISION_OPTION_CONTINUATIONS),
});

export const InvestigationTaskRecoverInputSchema = z.strictObject({ taskId: OPAQUE_ID, optionId: OPAQUE_ID.optional() });

export const InvestigationPlanSaveInputSchema = z.strictObject({ plan: InvestigationPlanSchema });
export const InvestigationPlanResponseSchema = z.strictObject({ plan: InvestigationPlanSchema });

export const InvestigationArtifactRecordInputSchema = z.strictObject({ artifact: InvestigationArtifactSchema });
export const InvestigationArtifactRecordResponseSchema = z.strictObject({ artifact: InvestigationArtifactSchema });
