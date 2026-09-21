//! Persistence for the evidence-backed investigation domain.

use rusqlite::{params, OptionalExtension, Row};
use sha2::{Digest, Sha256};

use super::decode::decode_error;
use crate::models::{
    DecisionBrief, DecisionBriefStatus, DecisionOptionStatus, Evidence, EvidenceContentType,
    EvidenceKind, EvidenceRedactionStatus, Finding, FindingConfidence, FindingKind,
    InvestigationArtifact, InvestigationFailure, InvestigationFailureOption,
    InvestigationNotificationPolicy, InvestigationPermissionMode, InvestigationPlan,
    InvestigationRun, InvestigationRunMode, InvestigationRunStatus, InvestigationSchedule,
    InvestigationScheduleComparison, InvestigationScheduleComparisonStatus,
    InvestigationScheduleRun, InvestigationScheduleRunStatus, InvestigationScheduleStatus,
    InvestigationStep, InvestigationStepKind, InvestigationStepStatus, InvestigationTarget,
    InvestigationTask, TaskArtifactKind, TaskArtifactStatus, TaskAutomationLevel, TaskBudget,
    TaskFailureCode, TaskPhase, TaskStatus, MAX_ARTIFACT_SERIALIZED_BYTES,
    MAX_EVIDENCE_SERIALIZED_BYTES,
};
use crate::{Database, DatabaseError, Result};

pub struct InvestigationsRepository<'a> {
    db: &'a Database,
}

pub struct TaskProgressUpdate<'a> {
    pub id: &'a str,
    pub status: TaskStatus,
    pub phase: TaskPhase,
    pub active_run_id: Option<&'a str>,
    pub last_failure: Option<&'a InvestigationFailure>,
    pub updated_at: &'a str,
    pub completed_at: Option<&'a str>,
}

/// Host-validated filters for bounded evidence search. The query remains scoped to
/// one task; `scope` is an exact target match, never a way to widen that task.
#[derive(Debug, Clone, Default)]
pub struct EvidenceSearchQuery {
    pub source_tool: Option<String>,
    pub kind: Option<EvidenceKind>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub scope: Option<InvestigationTarget>,
    pub limit: usize,
}

impl<'a> InvestigationsRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }
}

mod evidence;
mod plans;
mod schedules;
mod tasks;

pub(super) fn validate_schedule_shape(schedule: &InvestigationSchedule) -> Result<()> {
    if schedule.id.trim().is_empty() || schedule.task_id.trim().is_empty() {
        return Err(DatabaseError::Validation(
            "schedule id and task id are required".into(),
        ));
    }
    if schedule.interval_seconds == 0
        || schedule.cooldown_seconds > 86_400
        || schedule.dedupe_window_seconds == 0
        || schedule.dedupe_window_seconds > 86_400
        || schedule.max_concurrent_runs == 0
        || schedule.max_concurrent_runs > 16
    {
        return Err(DatabaseError::Validation(
            "schedule bounds are invalid".into(),
        ));
    }
    if schedule.interval_seconds > 86_400 {
        return Err(DatabaseError::Validation(
            "schedule interval is too large".into(),
        ));
    }
    if yukinal_time::parse_iso8601_utc(&schedule.next_run_at).is_none() {
        return Err(DatabaseError::Validation(
            "schedule nextRunAt must be a UTC timestamp".into(),
        ));
    }
    Ok(())
}

pub(super) fn insert_schedule_run(
    connection: &rusqlite::Connection,
    run: &InvestigationScheduleRun,
) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO investigation_schedule_runs (
            id, schedule_id, task_id, status, scheduled_at, claimed_at, started_at,
            finished_at, dedupe_key, outcome, error
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            run.id,
            run.schedule_id,
            run.task_id,
            run.status.as_str(),
            run.scheduled_at,
            run.claimed_at,
            run.started_at,
            run.finished_at,
            run.dedupe_key,
            run.outcome,
            run.error,
        ],
    )?;
    Ok(())
}

pub(super) fn schedule_run_id(schedule_id: &str, dedupe_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(schedule_id.as_bytes());
    hasher.update([0]);
    hasher.update(dedupe_key.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    format!("schedrun_{hex}")
}

pub(super) fn row_to_schedule(row: &Row<'_>) -> rusqlite::Result<InvestigationSchedule> {
    let interval_seconds = row
        .get::<_, i64>(3)?
        .try_into()
        .map_err(|_| decode_error(3, "schedule interval out of u64 range"))?;
    let cooldown_seconds = row
        .get::<_, i64>(4)?
        .try_into()
        .map_err(|_| decode_error(4, "schedule cooldown out of u64 range"))?;
    let dedupe_window_seconds = row
        .get::<_, i64>(5)?
        .try_into()
        .map_err(|_| decode_error(5, "schedule dedupe window out of u64 range"))?;
    let max_concurrent_runs = row
        .get::<_, i64>(6)?
        .try_into()
        .map_err(|_| decode_error(6, "schedule concurrency out of u32 range"))?;
    Ok(InvestigationSchedule {
        id: row.get(0)?,
        task_id: row.get(1)?,
        status: parse_enum(row.get(2)?, 2, InvestigationScheduleStatus::from_db)?,
        interval_seconds,
        cooldown_seconds,
        dedupe_window_seconds,
        max_concurrent_runs,
        budget: decode_json(row.get(7)?, 7)?,
        notification_policy: parse_enum(row.get(8)?, 8, InvestigationNotificationPolicy::from_db)?,
        next_run_at: row.get(9)?,
        last_run_at: row.get(10)?,
        last_outcome: row.get(11)?,
        last_error: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
        baseline_run_id: row.get(15)?,
    })
}

pub(super) fn row_to_schedule_run(row: &Row<'_>) -> rusqlite::Result<InvestigationScheduleRun> {
    Ok(InvestigationScheduleRun {
        id: row.get(0)?,
        schedule_id: row.get(1)?,
        task_id: row.get(2)?,
        status: parse_enum(row.get(3)?, 3, InvestigationScheduleRunStatus::from_db)?,
        scheduled_at: row.get(4)?,
        claimed_at: row.get(5)?,
        started_at: row.get(6)?,
        finished_at: row.get(7)?,
        dedupe_key: row.get(8)?,
        outcome: row.get(9)?,
        error: row.get(10)?,
    })
}

pub(super) fn row_to_task(row: &Row<'_>) -> rusqlite::Result<InvestigationTask> {
    let max_steps = row
        .get::<_, i64>(12)?
        .try_into()
        .map_err(|_| decode_error(12, "max_steps out of u32 range"))?;
    Ok(InvestigationTask {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        server_id: row.get(2)?,
        objective: row.get(3)?,
        success_criteria: decode_json(row.get(4)?, 4)?,
        scope: decode_json(row.get(5)?, 5)?,
        guardrails: decode_json(row.get(20)?, 20)?,
        mode: parse_enum(row.get(6)?, 6, InvestigationRunMode::from_db)?,
        permission_mode: parse_enum(row.get(7)?, 7, InvestigationPermissionMode::from_db)?,
        automation_level: parse_enum(row.get(8)?, 8, TaskAutomationLevel::from_db)?,
        created_by: row.get(9)?,
        phase: parse_enum(row.get(10)?, 10, TaskPhase::from_db)?,
        status: parse_enum(row.get(11)?, 11, TaskStatus::from_db)?,
        budget: TaskBudget {
            max_steps,
            max_run_ms: row
                .get::<_, i64>(13)?
                .try_into()
                .map_err(|_| decode_error(13, "max_run_ms out of u64 range"))?,
            max_attempts: row
                .get::<_, i64>(14)?
                .try_into()
                .map_err(|_| decode_error(14, "max_attempts out of u32 range"))?,
        },
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
        completed_at: row.get(17)?,
        active_run_id: row.get(18)?,
        last_failure: row
            .get::<_, Option<String>>(19)?
            .map(|value| decode_json(value, 19))
            .transpose()?,
    })
}

pub(super) fn row_to_evidence(row: &Row<'_>) -> rusqlite::Result<Evidence> {
    Ok(Evidence {
        id: row.get(0)?,
        task_id: row.get(1)?,
        run_id: row.get(2)?,
        scope: decode_json(row.get(3)?, 3)?,
        kind: parse_enum(row.get(4)?, 4, EvidenceKind::from_db)?,
        source_tool: row.get(5)?,
        collected_at: row.get(6)?,
        input_summary: row.get(7)?,
        content_type: parse_enum(row.get(8)?, 8, EvidenceContentType::from_db)?,
        content: decode_json(row.get(9)?, 9)?,
        content_hash: row.get(10)?,
        truncated: row.get::<_, i64>(11)? != 0,
        redaction_status: parse_enum(row.get(12)?, 12, EvidenceRedactionStatus::from_db)?,
    })
}

pub(super) fn query_evidence_for_run(
    connection: &rusqlite::Connection,
    run_id: &str,
) -> rusqlite::Result<Vec<Evidence>> {
    let mut statement = connection.prepare(
        "SELECT id, task_id, run_id, scope, kind, source_tool, collected_at, input_summary,
                content_type, content, content_hash, truncated, redaction_status
         FROM investigation_evidence
         WHERE run_id = ?1 ORDER BY collected_at ASC, id ASC",
    )?;
    let rows = statement
        .query_map(params![run_id], row_to_evidence)?
        .collect::<rusqlite::Result<Vec<_>>>();
    rows
}

pub(super) fn evidence_signature(evidence: &[Evidence]) -> Option<String> {
    if evidence.is_empty() {
        return None;
    }
    let mut parts = evidence
        .iter()
        .map(|item| {
            format!(
                "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
                item.source_tool,
                item.kind.as_str(),
                item.input_summary,
                item.content_hash,
                item.truncated,
            )
        })
        .collect::<Vec<_>>();
    parts.sort();
    let digest = Sha256::digest(parts.join("\u{1e}").as_bytes());
    Some(format!("{digest:x}"))
}

pub(super) fn row_to_run(row: &Row<'_>) -> rusqlite::Result<InvestigationRun> {
    let attempt = row
        .get::<_, i64>(5)?
        .try_into()
        .map_err(|_| decode_error(5, "run attempt out of u32 range"))?;
    Ok(InvestigationRun {
        id: row.get(0)?,
        task_id: row.get(1)?,
        session_id: row.get(2)?,
        message_id: row.get(3)?,
        trace_id: row.get(4)?,
        attempt,
        phase: parse_enum(row.get(6)?, 6, TaskPhase::from_db)?,
        status: parse_enum(row.get(7)?, 7, InvestigationRunStatus::from_db)?,
        started_at: row.get(8)?,
        updated_at: row.get(9)?,
        ended_at: row.get(10)?,
        checkpoint: row
            .get::<_, Option<String>>(11)?
            .map(|value| decode_json(value, 11))
            .transpose()?,
        failure: row
            .get::<_, Option<String>>(12)?
            .map(|value| decode_json(value, 12))
            .transpose()?,
    })
}

pub(super) fn row_to_step(row: &Row<'_>) -> rusqlite::Result<InvestigationStep> {
    let ordinal = row
        .get::<_, i64>(3)?
        .try_into()
        .map_err(|_| decode_error(3, "step ordinal out of u32 range"))?;
    let attempt = row
        .get::<_, i64>(7)?
        .try_into()
        .map_err(|_| decode_error(7, "step attempt out of u32 range"))?;
    Ok(InvestigationStep {
        id: row.get(0)?,
        task_id: row.get(1)?,
        run_id: row.get(2)?,
        ordinal,
        kind: parse_enum(row.get(4)?, 4, InvestigationStepKind::from_db)?,
        title: row.get(5)?,
        status: parse_enum(row.get(6)?, 6, InvestigationStepStatus::from_db)?,
        attempt,
        tool_name: row.get(8)?,
        plan_id: row.get(9)?,
        plan_step_id: row.get(10)?,
        target: row
            .get::<_, Option<String>>(11)?
            .map(|value| decode_json(value, 11))
            .transpose()?,
        input_summary: row.get(12)?,
        output_summary: row.get(13)?,
        evidence_ids: decode_json(row.get(14)?, 14)?,
        started_at: row.get(15)?,
        ended_at: row.get(16)?,
        failure: row
            .get::<_, Option<String>>(17)?
            .map(|value| decode_json(value, 17))
            .transpose()?,
    })
}

pub(super) fn row_to_finding(row: &Row<'_>) -> rusqlite::Result<Finding> {
    Ok(Finding {
        id: row.get(0)?,
        task_id: row.get(1)?,
        title: row.get(2)?,
        kind: parse_enum(row.get(3)?, 3, FindingKind::from_db)?,
        statement: row.get(4)?,
        evidence_ids: decode_json(row.get(5)?, 5)?,
        confidence: parse_enum(row.get(6)?, 6, FindingConfidence::from_db)?,
        next_verification: row.get(7)?,
        created_at: row.get(8)?,
    })
}

pub(super) fn row_to_brief(row: &Row<'_>) -> rusqlite::Result<DecisionBrief> {
    Ok(DecisionBrief {
        id: row.get(0)?,
        task_id: row.get(1)?,
        plan_id: row.get(2)?,
        generated_at: row.get(3)?,
        status: parse_enum(row.get(4)?, 4, DecisionBriefStatus::from_db)?,
        finding_ids: decode_json(row.get(5)?, 5)?,
        options: decode_json(row.get(6)?, 6)?,
        selected_option_id: row.get(7)?,
    })
}

pub(super) fn row_to_plan(row: &Row<'_>) -> rusqlite::Result<InvestigationPlan> {
    let revision = row
        .get::<_, i64>(2)?
        .try_into()
        .map_err(|_| decode_error(2, "plan revision out of u32 range"))?;
    Ok(InvestigationPlan {
        id: row.get(0)?,
        task_id: row.get(1)?,
        revision,
        status: parse_enum(row.get(3)?, 3, crate::models::PlanStatus::from_db)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
        current_step_id: row.get(6)?,
        steps: decode_json(row.get(7)?, 7)?,
        approval: row
            .get::<_, Option<String>>(8)?
            .map(|value| decode_json(value, 8))
            .transpose()?,
        observation_window: row
            .get::<_, Option<String>>(9)?
            .map(|value| decode_json(value, 9))
            .transpose()?,
    })
}

pub(super) fn row_to_artifact(row: &Row<'_>) -> rusqlite::Result<InvestigationArtifact> {
    let content: String = row.get(10)?;
    if content.len() > MAX_ARTIFACT_SERIALIZED_BYTES {
        return Err(decode_error(
            10,
            "artifact content exceeds the configured limit",
        ));
    }
    Ok(InvestigationArtifact {
        id: row.get(0)?,
        task_id: row.get(1)?,
        run_id: row.get(2)?,
        plan_id: row.get(3)?,
        plan_step_id: row.get(4)?,
        phase: parse_enum(row.get(5)?, 5, TaskPhase::from_db)?,
        kind: parse_enum(row.get(6)?, 6, TaskArtifactKind::from_db)?,
        status: parse_enum(row.get(7)?, 7, TaskArtifactStatus::from_db)?,
        title: row.get(8)?,
        summary: row.get(9)?,
        content: decode_json(content, 10)?,
        evidence_ids: decode_json(row.get(11)?, 11)?,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
    })
}

pub(super) fn decode_json<T: serde::de::DeserializeOwned>(
    raw: String,
    column: usize,
) -> rusqlite::Result<T> {
    serde_json::from_str(&raw).map_err(|error| decode_error(column, error))
}

pub(super) fn parse_enum<T>(
    raw: String,
    column: usize,
    parse: fn(&str) -> Option<T>,
) -> rusqlite::Result<T> {
    parse(&raw).ok_or_else(|| decode_error(column, format!("unknown value {raw:?}")))
}

pub(super) fn content_hash(content: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(content).expect("serde_json::Value is serializable");
    let digest = Sha256::digest(bytes);
    format!("{digest:x}")
}
