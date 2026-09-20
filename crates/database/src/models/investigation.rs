//! Durable investigation objects: tasks, evidence, findings and decision briefs.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::execution::RiskLevel;
use super::server::Environment;

enum_as_str!(TaskStatus, Pending => "pending", Investigating => "investigating", WaitingUser => "waiting_user", Executing => "executing", Verifying => "verifying", Completed => "completed", Failed => "failed", Stopped => "stopped", Expired => "expired");
enum_as_str!(TaskAutomationLevel, Readonly => "readonly", Propose => "propose", Execute => "execute");
enum_as_str!(TaskPhase, Investigating => "investigating", Decision => "decision", Execution => "execution", Verification => "verification", Recovery => "recovery", Completed => "completed");
enum_as_str!(InvestigationRunStatus, Admitted => "admitted", Running => "running", WaitingUser => "waiting_user", Completed => "completed", Failed => "failed", Cancelled => "cancelled", Interrupted => "interrupted");
enum_as_str!(InvestigationStepKind, Plan => "plan", Evidence => "evidence", Decision => "decision", Action => "action", Verification => "verification", Recovery => "recovery");
enum_as_str!(InvestigationStepStatus, Pending => "pending", Running => "running", WaitingUser => "waiting_user", Succeeded => "succeeded", Failed => "failed", Skipped => "skipped");
enum_as_str!(TaskFailureCode, BudgetExhausted => "budget_exhausted", Timeout => "timeout", Cancelled => "cancelled", ApprovalRequired => "approval_required", ApprovalRejected => "approval_rejected", Authentication => "authentication", Transport => "transport", TargetNotFound => "target_not_found", PermissionDenied => "permission_denied", InvalidInput => "invalid_input", PlanDeviation => "plan_deviation", CommandFailed => "command_failed", OutputTruncated => "output_truncated", EvidenceMissing => "evidence_missing", StaleTarget => "stale_target", Unsupported => "unsupported", Internal => "internal", Unknown => "unknown");
enum_as_str!(FailureOptionAction, Retry => "retry", Replan => "replan", WaitUser => "wait_user", Inspect => "inspect", Stop => "stop", Resume => "resume", Rollback => "rollback");
enum_as_str!(EvidenceKind, Snapshot => "snapshot", Log => "log", Service => "service", Container => "container", File => "file", ToolResult => "tool_result", Failure => "failure");
enum_as_str!(EvidenceContentType, Json => "json", Text => "text");
enum_as_str!(EvidenceRedactionStatus, Clean => "clean", Redacted => "redacted", Unknown => "unknown");
enum_as_str!(FindingKind, Fact => "fact", Inference => "inference", Unknown => "unknown");
enum_as_str!(FindingConfidence, High => "high", Medium => "medium", Low => "low");
enum_as_str!(DecisionBriefStatus, Draft => "draft", Presented => "presented", Selected => "selected", Dismissed => "dismissed");
enum_as_str!(DecisionOptionStatus, Available => "available", Selected => "selected", Rejected => "rejected");
enum_as_str!(DecisionOptionContinuation, ContinueReadonly => "continue_readonly", StartPlan => "start_plan", WaitUser => "wait_user", Stop => "stop");
enum_as_str!(InvestigationTargetHost, Local => "local", Remote => "remote");
enum_as_str!(InvestigationRunMode, Goal => "goal", Plan => "plan", Readonly => "readonly");
enum_as_str!(InvestigationPermissionMode, Ask => "ask", Auto => "auto");
enum_as_str!(PlanStatus, Draft => "draft", Active => "active", Superseded => "superseded", Completed => "completed");
enum_as_str!(PlanApprovalStatus, Pending => "pending", Approved => "approved", Rejected => "rejected");
enum_as_str!(PlanApprovalSource, User => "user", Policy => "policy");
enum_as_str!(ObservationWindowStatus, Pending => "pending", Running => "running", Succeeded => "succeeded", Failed => "failed", Cancelled => "cancelled");
enum_as_str!(PlanStepKind, Evidence => "evidence", Decision => "decision", Action => "action", Verification => "verification");
enum_as_str!(PlanIdempotency, Safe => "safe", Conditional => "conditional", Unsafe => "unsafe");
enum_as_str!(PlanStepStatus, Pending => "pending", Running => "running", Succeeded => "succeeded", Blocked => "blocked", Skipped => "skipped");
enum_as_str!(PlanDeviationCode, MissingPlan => "missing_plan", NoActiveStep => "no_active_step", ToolNotAllowed => "tool_not_allowed", TargetMismatch => "target_mismatch", OutsideTimeWindow => "outside_time_window", ScopeForbidden => "scope_forbidden", EvidenceMissing => "evidence_missing", StepBudgetExhausted => "step_budget_exhausted", BindingMismatch => "binding_mismatch", DuplicateCall => "duplicate_call");
enum_as_str!(PlanDeviationAction, Replan => "replan", WaitUser => "wait_user", Deny => "deny");
enum_as_str!(TaskArtifactKind, InvestigationPlan => "investigation_plan", Baseline => "baseline", EvidenceSet => "evidence_set", DecisionBrief => "decision_brief", ChangePlan => "change_plan", Execution => "execution", Verification => "verification", Failure => "failure");
enum_as_str!(TaskArtifactStatus, Draft => "draft", Ready => "ready", Succeeded => "succeeded", Failed => "failed", Superseded => "superseded");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Investigating,
    WaitingUser,
    Executing,
    Verifying,
    Completed,
    Failed,
    Stopped,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskAutomationLevel {
    Readonly,
    Propose,
    Execute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskPhase {
    Investigating,
    Decision,
    Execution,
    Verification,
    Recovery,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationRunStatus {
    Admitted,
    Running,
    WaitingUser,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InvestigationStepKind {
    Plan,
    Evidence,
    Decision,
    Action,
    Verification,
    Recovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationStepStatus {
    Pending,
    Running,
    WaitingUser,
    Succeeded,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskFailureCode {
    BudgetExhausted,
    Timeout,
    Cancelled,
    ApprovalRequired,
    ApprovalRejected,
    Authentication,
    Transport,
    TargetNotFound,
    PermissionDenied,
    InvalidInput,
    PlanDeviation,
    CommandFailed,
    OutputTruncated,
    EvidenceMissing,
    StaleTarget,
    Unsupported,
    Internal,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureOptionAction {
    Retry,
    Replan,
    WaitUser,
    Inspect,
    Stop,
    Resume,
    Rollback,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationFailureOption {
    pub id: String,
    pub action: FailureOptionAction,
    pub title: String,
    pub description: String,
    pub requires_approval: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Snapshot,
    Log,
    Service,
    Container,
    File,
    ToolResult,
    Failure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EvidenceContentType {
    Json,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EvidenceRedactionStatus {
    Clean,
    Redacted,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingKind {
    Fact,
    Inference,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingConfidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecisionBriefStatus {
    Draft,
    Presented,
    Selected,
    Dismissed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecisionOptionStatus {
    Available,
    Selected,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionOptionContinuation {
    ContinueReadonly,
    StartPlan,
    WaitUser,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InvestigationTargetHost {
    Local,
    Remote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InvestigationRunMode {
    Goal,
    Plan,
    Readonly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InvestigationPermissionMode {
    Ask,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanStatus {
    Draft,
    Active,
    Superseded,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanApprovalStatus {
    Pending,
    Approved,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanApprovalSource {
    User,
    Policy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObservationWindowStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanStepKind {
    Evidence,
    Decision,
    Action,
    Verification,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanIdempotency {
    Safe,
    Conditional,
    Unsafe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanStepStatus {
    Pending,
    Running,
    Succeeded,
    Blocked,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanDeviationCode {
    MissingPlan,
    NoActiveStep,
    ToolNotAllowed,
    TargetMismatch,
    OutsideTimeWindow,
    ScopeForbidden,
    EvidenceMissing,
    StepBudgetExhausted,
    BindingMismatch,
    DuplicateCall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanDeviationAction {
    Replan,
    WaitUser,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationTarget {
    pub host: InvestigationTargetHost,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub environment: Environment,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskBudget {
    pub max_steps: u32,
    pub max_run_ms: u64,
    pub max_attempts: u32,
}

/// Host-enforced limits attached to one durable task. Empty arrays and absent
/// timestamps preserve the pre-guardrail task behaviour when decoding older rows.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationTaskGuardrails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_before_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub forbidden_tools: Vec<String>,
    #[serde(default)]
    pub forbidden_path_prefixes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationFailure {
    pub code: TaskFailureCode,
    pub message: String,
    pub retryable: bool,
    pub attempt: u32,
    pub at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<InvestigationFailureOption>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationTask {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    pub objective: String,
    pub success_criteria: Vec<String>,
    pub scope: InvestigationTarget,
    pub guardrails: InvestigationTaskGuardrails,
    pub mode: InvestigationRunMode,
    pub permission_mode: InvestigationPermissionMode,
    pub automation_level: TaskAutomationLevel,
    pub created_by: String,
    pub phase: TaskPhase,
    pub status: TaskStatus,
    pub budget: TaskBudget,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<InvestigationFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationRun {
    pub id: String,
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    pub attempt: u32,
    pub phase: TaskPhase,
    pub status: InvestigationRunStatus,
    pub started_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<InvestigationFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationStep {
    pub id: String,
    pub task_id: String,
    pub run_id: String,
    pub ordinal: u32,
    pub kind: InvestigationStepKind,
    pub title: String,
    pub status: InvestigationStepStatus,
    pub attempt: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_step_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<InvestigationTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_summary: Option<String>,
    pub evidence_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<InvestigationFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationPlanDeviation {
    pub code: PlanDeviationCode,
    pub action: PlanDeviationAction,
    pub message: String,
    pub tool_name: String,
    pub at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationPlanStep {
    pub id: String,
    pub ordinal: u32,
    pub kind: PlanStepKind,
    pub title: String,
    pub purpose: String,
    pub allowed_tools: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub input_bindings: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency: Option<PlanIdempotency>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk_level: Option<RiskLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_baseline: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preconditions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_criteria: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rollback: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<InvestigationTarget>,
    pub evidence_ids: Vec<String>,
    pub success_criteria: Vec<String>,
    pub requires_approval: bool,
    pub max_attempts: u32,
    pub attempts: u32,
    pub status: PlanStepStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_deviation: Option<InvestigationPlanDeviation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationPlanApproval {
    pub status: PlanApprovalStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<PlanApprovalSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub option_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationObservationWindow {
    pub duration_seconds: u64,
    pub interval_seconds: u64,
    pub allowed_tools: Vec<String>,
    pub success_criteria: Vec<String>,
    pub status: ObservationWindowStatus,
    pub sample_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline_epoch_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sample_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sample_epoch_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationPlan {
    pub id: String,
    pub task_id: String,
    pub revision: u32,
    pub status: PlanStatus,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_step_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval: Option<InvestigationPlanApproval>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation_window: Option<InvestigationObservationWindow>,
    pub steps: Vec<InvestigationPlanStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    pub id: String,
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub scope: InvestigationTarget,
    pub kind: EvidenceKind,
    pub source_tool: String,
    pub collected_at: String,
    pub input_summary: String,
    pub content_type: EvidenceContentType,
    pub content: serde_json::Value,
    pub content_hash: String,
    pub truncated: bool,
    pub redaction_status: EvidenceRedactionStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub id: String,
    pub task_id: String,
    pub title: String,
    pub kind: FindingKind,
    pub statement: String,
    pub evidence_ids: Vec<String>,
    pub confidence: FindingConfidence,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_verification: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionOption {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub impact: String,
    pub risk_level: RiskLevel,
    pub evidence_ids: Vec<String>,
    pub finding_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    pub verification: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rollback: Option<String>,
    pub requires_approval: bool,
    pub status: DecisionOptionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<DecisionOptionContinuation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionBrief {
    pub id: String,
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    pub generated_at: String,
    pub status: DecisionBriefStatus,
    pub finding_ids: Vec<String>,
    pub options: Vec<DecisionOption>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_option_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskArtifactKind {
    InvestigationPlan,
    Baseline,
    EvidenceSet,
    DecisionBrief,
    ChangePlan,
    Execution,
    Verification,
    Failure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskArtifactStatus {
    Draft,
    Ready,
    Succeeded,
    Failed,
    Superseded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationArtifact {
    pub id: String,
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_step_id: Option<String>,
    pub phase: TaskPhase,
    pub kind: TaskArtifactKind,
    pub status: TaskArtifactStatus,
    pub title: String,
    pub summary: String,
    pub content: serde_json::Value,
    pub evidence_ids: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

pub const MAX_EVIDENCE_SERIALIZED_BYTES: usize = 1_048_576;
pub const MAX_ARTIFACT_SERIALIZED_BYTES: usize = 1_048_576;
