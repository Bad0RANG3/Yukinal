//! Durable local triggers for read-only investigation tasks.

use serde::{Deserialize, Serialize};

use super::investigation::TaskBudget;

enum_as_str!(InvestigationScheduleStatus, Active => "active", Paused => "paused", Revoked => "revoked");
enum_as_str!(InvestigationScheduleRunStatus, Queued => "queued", Claimed => "claimed", Running => "running", Succeeded => "succeeded", Failed => "failed", Skipped => "skipped", Interrupted => "interrupted");
enum_as_str!(InvestigationNotificationPolicy, Silent => "silent", OnChange => "on_change", Always => "always", FailedRunsOnly => "failed_runs_only");
enum_as_str!(InvestigationScheduleComparisonStatus, Baseline => "baseline", NoChange => "no_change", Changed => "changed", InsufficientEvidence => "insufficient_evidence");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationScheduleStatus {
    Active,
    Paused,
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationScheduleRunStatus {
    Queued,
    Claimed,
    Running,
    Succeeded,
    Failed,
    Skipped,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationNotificationPolicy {
    Silent,
    OnChange,
    Always,
    FailedRunsOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationScheduleComparisonStatus {
    Baseline,
    NoChange,
    Changed,
    InsufficientEvidence,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationScheduleComparison {
    pub status: InvestigationScheduleComparisonStatus,
    pub current_signature: Option<String>,
    pub previous_signature: Option<String>,
    pub current_evidence_ids: Vec<String>,
    pub previous_evidence_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationSchedule {
    pub id: String,
    pub task_id: String,
    pub status: InvestigationScheduleStatus,
    pub interval_seconds: u64,
    pub cooldown_seconds: u64,
    pub dedupe_window_seconds: u64,
    pub max_concurrent_runs: u32,
    pub budget: TaskBudget,
    pub notification_policy: InvestigationNotificationPolicy,
    pub next_run_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationScheduleRun {
    pub id: String,
    pub schedule_id: String,
    pub task_id: String,
    pub status: InvestigationScheduleRunStatus,
    pub scheduled_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claimed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    pub dedupe_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
