import type { AgentPermissionMode, AgentRunMode, Environment, RiskLevel } from "./risk.js";
import type { ToolTarget } from "./tool.js";

/** A durable objective; one Agent run is only one attempt at a task. */
export const TASK_STATUSES = [
  "pending",
  "investigating",
  "waiting_user",
  "executing",
  "verifying",
  "completed",
  "failed",
  "stopped",
  "expired",
] as const;
export type TaskStatus = (typeof TASK_STATUSES)[number];

/** How far a task may proceed without first producing a user-facing choice. */
export const TASK_AUTOMATION_LEVELS = ["readonly", "propose", "execute"] as const;
export type TaskAutomationLevel = (typeof TASK_AUTOMATION_LEVELS)[number];

/** Durable stage, kept separate from the coarse task status so recovery is explicit. */
export const TASK_PHASES = ["investigating", "decision", "execution", "verification", "recovery", "completed"] as const;
export type TaskPhase = (typeof TASK_PHASES)[number];

export const TASK_RUN_STATUSES = ["admitted", "running", "waiting_user", "completed", "failed", "cancelled", "interrupted"] as const;
export type TaskRunStatus = (typeof TASK_RUN_STATUSES)[number];

export const TASK_STEP_KINDS = ["plan", "evidence", "decision", "action", "verification", "recovery"] as const;
export type TaskStepKind = (typeof TASK_STEP_KINDS)[number];

export const TASK_STEP_STATUSES = ["pending", "running", "waiting_user", "succeeded", "failed", "skipped"] as const;
export type TaskStepStatus = (typeof TASK_STEP_STATUSES)[number];

/** Durable plan lifecycle. A plan is replaceable, but only one revision is active. */
export const PLAN_STATUSES = ["draft", "active", "superseded", "completed"] as const;
export type PlanStatus = (typeof PLAN_STATUSES)[number];

/** Work a plan step promises to do. The host uses this to compare a tool call with intent. */
export const PLAN_STEP_KINDS = ["evidence", "decision", "action", "verification"] as const;
export type PlanStepKind = (typeof PLAN_STEP_KINDS)[number];

export const PLAN_IDEMPOTENCY = ["safe", "conditional", "unsafe"] as const;
export type PlanIdempotency = (typeof PLAN_IDEMPOTENCY)[number];

export const PLAN_STEP_STATUSES = ["pending", "running", "succeeded", "blocked", "skipped"] as const;
export type PlanStepStatus = (typeof PLAN_STEP_STATUSES)[number];

export const PLAN_DEVIATION_CODES = [
  "missing_plan",
  "no_active_step",
  "tool_not_allowed",
  "target_mismatch",
  "outside_time_window",
  "scope_forbidden",
  "evidence_missing",
  "step_budget_exhausted",
  "binding_mismatch",
  "duplicate_call",
] as const;
export type PlanDeviationCode = (typeof PLAN_DEVIATION_CODES)[number];

export const PLAN_DEVIATION_ACTIONS = ["replan", "wait_user", "deny"] as const;
export type PlanDeviationAction = (typeof PLAN_DEVIATION_ACTIONS)[number];

/** Normalised failure classes used by task recovery and the UI. */
export const TASK_FAILURE_CODES = [
  "budget_exhausted",
  "timeout",
  "cancelled",
  "approval_required",
  "approval_rejected",
  "authentication",
  "transport",
  "target_not_found",
  "permission_denied",
  "invalid_input",
  "plan_deviation",
  "command_failed",
  "output_truncated",
  "evidence_missing",
  "stale_target",
  "unsupported",
  "internal",
  "unknown",
] as const;
export type TaskFailureCode = (typeof TASK_FAILURE_CODES)[number];

/** User-visible choices that can be offered after a bounded task failure. */
export const FAILURE_OPTION_ACTIONS = ["retry", "replan", "wait_user", "inspect", "stop", "resume", "rollback"] as const;
export type FailureOptionAction = (typeof FAILURE_OPTION_ACTIONS)[number];

export const PLAN_APPROVAL_STATUSES = ["pending", "approved", "rejected"] as const;
export type PlanApprovalStatus = (typeof PLAN_APPROVAL_STATUSES)[number];

export const PLAN_APPROVAL_SOURCES = ["user", "policy"] as const;
export type PlanApprovalSource = (typeof PLAN_APPROVAL_SOURCES)[number];

export const OBSERVATION_WINDOW_STATUSES = ["pending", "running", "succeeded", "failed", "cancelled"] as const;
export type ObservationWindowStatus = (typeof OBSERVATION_WINDOW_STATUSES)[number];

/** User/policy choice that unlocks proposal-mode action steps for one plan revision. */
export interface InvestigationPlanApproval {
  status: PlanApprovalStatus;
  source?: PlanApprovalSource;
  optionId?: string;
  approvedAt?: string;
  note?: string;
}

/**
 * Host-owned state for the bounded post-change observation period. The model may
 * propose the duration, interval, allowed read tools and success criteria, but it
 * cannot advance sampleCount or mark the window successful.
 */
export interface InvestigationObservationWindow {
  durationSeconds: number;
  intervalSeconds: number;
  allowedTools: string[];
  successCriteria: string[];
  status: ObservationWindowStatus;
  sampleCount: number;
  startedAt?: string;
  deadlineAt?: string;
  /** Host-derived epoch value used for deterministic deadline checks. */
  deadlineEpochSeconds?: number;
  lastSampleAt?: string;
  lastSampleEpochSeconds?: number;
  lastFailure?: string;
}

export interface InvestigationFailureOption {
  id: string;
  action: FailureOptionAction;
  title: string;
  description: string;
  requiresApproval: boolean;
}

/**
 * Host-enforced boundaries for a durable task. These are deliberately narrower
 * than a general policy language: the task may close its execution window,
 * deny exact internal tool names, and deny lexical remote path prefixes.
 * The sidecar may see them in context, but it cannot widen them.
 */
export interface InvestigationTaskGuardrails {
  notBeforeAt?: string;
  expiresAt?: string;
  forbiddenTools: string[];
  forbiddenPathPrefixes: string[];
}

export const EVIDENCE_KINDS = ["snapshot", "log", "service", "container", "file", "tool_result", "failure"] as const;
export type EvidenceKind = (typeof EVIDENCE_KINDS)[number];

export const EVIDENCE_CONTENT_TYPES = ["json", "text"] as const;
export type EvidenceContentType = (typeof EVIDENCE_CONTENT_TYPES)[number];

export const EVIDENCE_REDACTION_STATUSES = ["clean", "redacted", "unknown"] as const;
export type EvidenceRedactionStatus = (typeof EVIDENCE_REDACTION_STATUSES)[number];

/** Host-derived freshness signal; it is recalculated when evidence is presented. */
export const EVIDENCE_FRESHNESS_STATUSES = ["fresh", "stale", "expired", "unknown"] as const;
export type EvidenceFreshnessStatus = (typeof EVIDENCE_FRESHNESS_STATUSES)[number];

export interface EvidenceFreshness {
  status: EvidenceFreshnessStatus;
  policy: "default-v1";
  /** Host clock used for this evaluation, not a timestamp supplied by the Agent. */
  evaluatedAt: string;
  /** Age in whole seconds when the collection timestamp was parseable and not future-dated. */
  ageSeconds?: number;
  /** Default-v1 policy boundary after which evidence is no longer considered fresh. */
  staleAfterSeconds: number;
  /** Default-v1 policy boundary after which evidence must not be treated as current state. */
  expiresAfterSeconds: number;
  /** Present only when the timestamp cannot be safely classified. */
  reason?: string;
}

export const FINDING_KINDS = ["fact", "inference", "unknown"] as const;
export type FindingKind = (typeof FINDING_KINDS)[number];

export const FINDING_CONFIDENCE_LEVELS = ["high", "medium", "low"] as const;
export type FindingConfidence = (typeof FINDING_CONFIDENCE_LEVELS)[number];

export const DECISION_BRIEF_STATUSES = ["draft", "presented", "selected", "dismissed"] as const;
export type DecisionBriefStatus = (typeof DECISION_BRIEF_STATUSES)[number];

export const DECISION_OPTION_STATUSES = ["available", "selected", "rejected"] as const;
export type DecisionOptionStatus = (typeof DECISION_OPTION_STATUSES)[number];

/** Explicit continuation requested by a selected decision option. Omitted means wait for the user. */
export const DECISION_OPTION_CONTINUATIONS = ["continue_readonly", "start_plan", "wait_user", "stop"] as const;
export type DecisionOptionContinuation = (typeof DECISION_OPTION_CONTINUATIONS)[number];

/** Durable output of a task phase. Content is already bounded and redacted by the caller. */
export const TASK_ARTIFACT_KINDS = [
  "investigation_plan",
  "baseline",
  "evidence_set",
  "decision_brief",
  "change_plan",
  "execution",
  "verification",
  "failure",
] as const;
export type TaskArtifactKind = (typeof TASK_ARTIFACT_KINDS)[number];

export const TASK_ARTIFACT_STATUSES = ["draft", "ready", "succeeded", "failed", "superseded"] as const;
export type TaskArtifactStatus = (typeof TASK_ARTIFACT_STATUSES)[number];

export interface InvestigationArtifact {
  id: string;
  taskId: string;
  runId?: string;
  /** Host-validated plan revision and step that produced this artifact. */
  planId?: string;
  planStepId?: string;
  phase: TaskPhase;
  kind: TaskArtifactKind;
  status: TaskArtifactStatus;
  title: string;
  summary: string;
  content: unknown;
  evidenceIds: string[];
  createdAt: string;
  updatedAt: string;
}

/** Metadata-only artifact projection used when task state is injected into Agent context. */
export interface InvestigationArtifactSummary {
  id: string;
  taskId: string;
  runId?: string;
  planId?: string;
  planStepId?: string;
  phase: TaskPhase;
  kind: TaskArtifactKind;
  status: TaskArtifactStatus;
  title: string;
  summary: string;
  evidenceIds: string[];
  createdAt: string;
  updatedAt: string;
}

export interface TaskBudget {
  maxSteps: number;
  maxRunMs: number;
  maxAttempts: number;
}

export interface InvestigationFailure {
  code: TaskFailureCode;
  message: string;
  retryable: boolean;
  attempt: number;
  at: string;
  detail?: unknown;
  options?: InvestigationFailureOption[];
}

export interface InvestigationRun {
  id: string;
  taskId: string;
  sessionId?: string;
  messageId?: string;
  traceId?: string;
  attempt: number;
  phase: TaskPhase;
  status: TaskRunStatus;
  startedAt: string;
  updatedAt: string;
  endedAt?: string;
  checkpoint?: unknown;
  failure?: InvestigationFailure;
}

export interface InvestigationStep {
  id: string;
  taskId: string;
  runId: string;
  ordinal: number;
  kind: TaskStepKind;
  title: string;
  status: TaskStepStatus;
  attempt: number;
  toolName?: string;
  /** The durable plan revision and step that authorized this observed call. */
  planId?: string;
  planStepId?: string;
  target?: ToolTarget;
  inputSummary?: string;
  outputSummary?: string;
  evidenceIds: string[];
  startedAt?: string;
  endedAt?: string;
  failure?: InvestigationFailure;
}

export interface InvestigationPlanStep {
  id: string;
  ordinal: number;
  kind: PlanStepKind;
  title: string;
  purpose: string;
  allowedTools: string[];
  /** Exact scalar tool arguments the host must bind before executing this step. */
  inputBindings?: Record<string, string>;
  /** Playbook metadata; optional on rows written before the maintenance contract existed. */
  idempotency?: PlanIdempotency;
  riskLevel?: RiskLevel;
  /** Action steps may require a baseline artifact from this plan before execution. */
  requiresBaseline?: boolean;
  preconditions?: string[];
  verificationCriteria?: string[];
  preview?: string;
  rollback?: string;
  target?: ToolTarget;
  evidenceIds: string[];
  successCriteria: string[];
  requiresApproval: boolean;
  maxAttempts: number;
  attempts: number;
  status: PlanStepStatus;
  startedAt?: string;
  endedAt?: string;
  lastDeviation?: InvestigationPlanDeviation;
}

export interface InvestigationPlan {
  id: string;
  taskId: string;
  revision: number;
  status: PlanStatus;
  createdAt: string;
  updatedAt: string;
  currentStepId?: string;
  approval?: InvestigationPlanApproval;
  observationWindow?: InvestigationObservationWindow;
  steps: InvestigationPlanStep[];
}

export interface InvestigationPlanDeviation {
  code: PlanDeviationCode;
  action: PlanDeviationAction;
  message: string;
  toolName: string;
  at: string;
  planId?: string;
  stepId?: string;
}

export interface InvestigationTask {
  id: string;
  workspaceId?: string;
  serverId?: string;
  objective: string;
  successCriteria: string[];
  scope: ToolTarget;
  guardrails: InvestigationTaskGuardrails;
  mode: AgentRunMode;
  permissionMode: AgentPermissionMode;
  automationLevel: TaskAutomationLevel;
  createdBy: string;
  phase: TaskPhase;
  status: TaskStatus;
  budget: TaskBudget;
  createdAt: string;
  updatedAt: string;
  completedAt?: string;
  activeRunId?: string;
  lastFailure?: InvestigationFailure;
}

export interface Evidence {
  id: string;
  taskId: string;
  /** Host-owned run association; never trusted from the model for persistence. */
  runId?: string;
  scope: ToolTarget;
  kind: EvidenceKind;
  sourceTool: string;
  collectedAt: string;
  inputSummary: string;
  contentType: EvidenceContentType;
  /** Already redacted, bounded tool output. The raw secret-bearing response never enters this field. */
  content: unknown;
  contentHash: string;
  truncated: boolean;
  redactionStatus: EvidenceRedactionStatus;
  /** Present on host responses; ignored and overwritten if supplied on record input. */
  freshness?: EvidenceFreshness;
}

/** Metadata returned by evidence search; the content body requires an explicit fetch by id. */
export interface EvidenceSummary {
  id: string;
  taskId: string;
  /** Host-owned run association; never trusted from the model for persistence. */
  runId?: string;
  scope: ToolTarget;
  kind: EvidenceKind;
  sourceTool: string;
  collectedAt: string;
  inputSummary: string;
  contentType: EvidenceContentType;
  contentHash: string;
  truncated: boolean;
  redactionStatus: EvidenceRedactionStatus;
  /** Host-derived status; absent only for historical or test-only projections. */
  freshness?: EvidenceFreshness;
}

/** Bounded, task-scoped search filters. Raw content is deliberately not part of the result. */
export interface EvidenceSearchInput {
  sourceTool?: string;
  kind?: EvidenceKind;
  from?: string;
  to?: string;
  target?: ToolTarget;
  limit?: number;
}

export interface EvidenceSearchResult {
  evidence: EvidenceSummary[];
}

/** Deterministic host-side grouping used to inspect one observation across sources. */
export const EVIDENCE_CORRELATION_MATCHES = ["same_run", "time_window"] as const;
export type EvidenceCorrelationMatch = (typeof EVIDENCE_CORRELATION_MATCHES)[number];

export interface EvidenceCorrelationInput {
  anchorEvidenceId: string;
  /** Used only when the anchor has no host-owned run; bounded by the host. */
  windowSeconds?: number;
  limit?: number;
}

export interface EvidenceCorrelation {
  anchor: EvidenceSummary;
  evidence: EvidenceSummary[];
  matchedBy: EvidenceCorrelationMatch;
  windowSeconds: number;
  sourceTools: string[];
  warnings: string[];
}

export interface EvidenceCorrelationResult {
  correlation: EvidenceCorrelation;
}

/** A host-derived, body-free comparison of two persisted evidence items. */
export const EVIDENCE_COMPARISON_STATUSES = ["identical", "changed"] as const;
export type EvidenceComparisonStatus = (typeof EVIDENCE_COMPARISON_STATUSES)[number];

export const EVIDENCE_COMPARISON_SHAPES = ["json", "text", "mixed"] as const;
export type EvidenceComparisonShape = (typeof EVIDENCE_COMPARISON_SHAPES)[number];

export interface EvidenceComparisonInput {
  leftEvidenceId: string;
  rightEvidenceId: string;
}

export interface EvidenceTextComparison {
  leftLineCount: number;
  rightLineCount: number;
  changedLineCount: number;
  addedLineCount: number;
  removedLineCount: number;
}

export interface EvidenceComparison {
  status: EvidenceComparisonStatus;
  shape: EvidenceComparisonShape;
  left: EvidenceSummary;
  right: EvidenceSummary;
  /** JSON paths changed by value; raw values are intentionally not returned. */
  changedPaths: string[];
  /** May exceed changedPaths.length when the bounded path list was truncated. */
  changedPathCount: number;
  diffTruncated: boolean;
  text?: EvidenceTextComparison;
  warnings: string[];
}

export interface EvidenceComparisonResult {
  comparison: EvidenceComparison;
}

export interface Finding {
  id: string;
  taskId: string;
  title: string;
  kind: FindingKind;
  statement: string;
  evidenceIds: string[];
  confidence: FindingConfidence;
  nextVerification?: string;
  createdAt: string;
}

export interface DecisionOption {
  id: string;
  title: string;
  summary: string;
  impact: string;
  riskLevel: RiskLevel;
  evidenceIds: string[];
  findingIds: string[];
  preview?: string;
  verification: string;
  rollback?: string;
  requiresApproval: boolean;
  status: DecisionOptionStatus;
  /** The host may only auto-continue when this is explicitly set. */
  continuation?: DecisionOptionContinuation;
}

export interface DecisionBrief {
  id: string;
  taskId: string;
  /** Plan revision whose evidence and proposed actions this brief describes. */
  planId?: string;
  generatedAt: string;
  status: DecisionBriefStatus;
  findingIds: string[];
  options: DecisionOption[];
  selectedOptionId?: string;
}

export interface InvestigationTaskCreateInput {
  id?: string;
  workspaceId?: string;
  serverId?: string;
  objective: string;
  successCriteria: string[];
  scope: ToolTarget;
  guardrails?: InvestigationTaskGuardrails;
  mode: AgentRunMode;
  permissionMode: AgentPermissionMode;
  automationLevel: TaskAutomationLevel;
  budget?: TaskBudget;
}

export interface InvestigationTaskListInput {
  status?: TaskStatus;
  limit?: number;
}

export interface InvestigationTaskListResponse {
  tasks: InvestigationTask[];
}

export interface InvestigationTaskResponse {
  task: InvestigationTask;
}

/** Result of starting a durable task without first opening the chat panel. */
export interface InvestigationTaskStartResponse {
  runId: string;
  started: boolean;
}

export interface InvestigationTaskDetailResponse {
  task: InvestigationTask;
  evidence: Evidence[];
  findings: Finding[];
  decisionBrief?: DecisionBrief;
  plan?: InvestigationPlan;
  artifacts: InvestigationArtifact[];
  runs: InvestigationRun[];
  steps: InvestigationStep[];
}

/**
 * Host-owned, metadata-only task projection for sidecar context assembly.
 * Evidence and artifact bodies are intentionally absent; the sidecar must use
 * an explicit evidence ID fetch when it needs one redacted body.
 */
export interface InvestigationContext {
  task: InvestigationTask;
  evidence: EvidenceSummary[];
  findings: Finding[];
  decisionBrief?: DecisionBrief;
  plan?: InvestigationPlan;
  artifacts: InvestigationArtifactSummary[];
  runs: InvestigationRun[];
  steps: InvestigationStep[];
}

export interface InvestigationTaskStatusUpdateInput {
  taskId: string;
  status: TaskStatus;
  completedAt?: string;
}

/** Host-owned cancellation request for one durable task. */
export interface InvestigationTaskStopInput {
  taskId: string;
}

export interface InvestigationBriefSelectInput {
  taskId: string;
  briefId: string;
  optionId: string;
}

export interface InvestigationBriefResponse {
  brief: DecisionBrief;
  /** Omitted legacy options are normalized to wait_user by the host. */
  continuation: DecisionOptionContinuation;
}

export interface InvestigationTaskRecoverInput {
  taskId: string;
  /** Optional failure choice selected by the user; omitted keeps the legacy resume action. */
  optionId?: string;
}

export interface InvestigationPlanSaveInput {
  plan: InvestigationPlan;
}

export interface InvestigationPlanResponse {
  plan: InvestigationPlan;
}

export interface InvestigationArtifactRecordInput {
  artifact: InvestigationArtifact;
}

export interface InvestigationArtifactRecordResponse {
  artifact: InvestigationArtifact;
}

export const INVESTIGATION_LIMITS = {
  maxObjectiveChars: 8_192,
  maxSuccessCriteria: 16,
  maxSuccessCriterionChars: 1_024,
  maxFailureMessageChars: 4_096,
  maxStepTitleChars: 512,
  maxStepSummaryChars: 4_096,
  maxRunsPerTask: 64,
  maxStepsPerTask: 512,
  maxEvidenceInputSummaryChars: 4_096,
  maxEvidenceSerializedBytes: 1_048_576,
  maxFindingsPerTask: 256,
  maxDecisionOptions: 16,
  maxPlanSteps: 64,
  maxPlanAllowedTools: 32,
  maxPlanInputBindings: 8,
  maxPlanInputBindingKeyChars: 64,
  maxPlanInputBindingValueChars: 4_096,
  maxPlanStepCriteria: 16,
  maxPlanTextChars: 4_096,
  maxObservationDurationSeconds: 86_400,
  maxObservationIntervalSeconds: 86_400,
  maxObservationTools: 32,
  maxObservationCriteria: 16,
  maxPlanDeviationMessageChars: 4_096,
  maxArtifactTitleChars: 512,
  maxArtifactSummaryChars: 8_192,
  maxArtifactSerializedBytes: 1_048_576,
  maxArtifactEvidenceIds: 256,
  maxFailureOptions: 8,
  maxFailureOptionTextChars: 1_024,
  maxGuardrailTools: 64,
  maxGuardrailPathPrefixes: 64,
  maxGuardrailNameChars: 256,
  maxGuardrailPathChars: 4_096,
} as const;

export type InvestigationEnvironment = Environment;
