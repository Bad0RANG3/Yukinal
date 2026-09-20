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

    pub fn create_task(&self, task: &InvestigationTask) -> Result<()> {
        let success_criteria = serde_json::to_string(&task.success_criteria)?;
        let scope = serde_json::to_string(&task.scope)?;
        let guardrails = serde_json::to_string(&task.guardrails)?;
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO investigation_tasks (
                    id, workspace_id, server_id, objective, success_criteria, scope, mode,
                    permission_mode, automation_level, created_by, phase, status, max_steps,
                    max_run_ms, max_attempts, created_at, updated_at, completed_at,
                    active_run_id, last_failure, guardrails
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
                params![
                    task.id,
                    task.workspace_id,
                    task.server_id,
                    task.objective,
                    success_criteria,
                    scope,
                    task.mode.as_str(),
                    task.permission_mode.as_str(),
                    task.automation_level.as_str(),
                    task.created_by,
                    task.phase.as_str(),
                    task.status.as_str(),
                    i64::from(task.budget.max_steps),
                    i64::try_from(task.budget.max_run_ms).map_err(|_| {
                        DatabaseError::Validation("max_run_ms exceeds SQLite integer range".into())
                    })?,
                    i64::from(task.budget.max_attempts),
                    task.created_at,
                    task.updated_at,
                    task.completed_at,
                    task.active_run_id,
                    task.last_failure.as_ref().map(serde_json::to_string).transpose()?,
                    guardrails,
                ],
            )?;
            Ok(())
        })
    }

    pub fn get_task(&self, id: &str) -> Result<InvestigationTask> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, workspace_id, server_id, objective, success_criteria, scope, mode,
                            permission_mode, automation_level, created_by, phase, status,
                            max_steps, max_run_ms, max_attempts, created_at, updated_at,
                            completed_at, active_run_id, last_failure, guardrails
                     FROM investigation_tasks WHERE id = ?1",
                    params![id],
                    row_to_task,
                )
                .optional()
                .map_err(DatabaseError::from)?
                .ok_or(DatabaseError::NotFound)
        })
    }

    pub fn list_tasks(
        &self,
        status: Option<TaskStatus>,
        limit: usize,
    ) -> Result<Vec<InvestigationTask>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, workspace_id, server_id, objective, success_criteria, scope, mode,
                        permission_mode, automation_level, created_by, phase, status, max_steps,
                        max_run_ms, max_attempts, created_at, updated_at, completed_at,
                        active_run_id, last_failure, guardrails
                 FROM investigation_tasks
                 WHERE (?1 IS NULL OR status = ?1)
                 ORDER BY updated_at DESC, id DESC LIMIT ?2",
            )?;
            let rows = statement.query_map(
                params![status.map(|value| value.as_str()), limit as i64],
                row_to_task,
            )?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
        })
    }

    pub fn update_task_status(
        &self,
        id: &str,
        status: TaskStatus,
        updated_at: &str,
        completed_at: Option<&str>,
    ) -> Result<InvestigationTask> {
        self.db.with(|connection| {
            let changed = connection.execute(
                "UPDATE investigation_tasks
                    SET status = ?2,
                        phase = CASE ?2
                            WHEN 'waiting_user' THEN 'decision'
                            WHEN 'executing' THEN 'execution'
                            WHEN 'verifying' THEN 'verification'
                            WHEN 'completed' THEN 'completed'
                            WHEN 'failed' THEN 'recovery'
                            WHEN 'stopped' THEN 'recovery'
                            WHEN 'expired' THEN 'recovery'
                            ELSE 'investigating'
                        END,
                        updated_at = ?3, completed_at = ?4
                  WHERE id = ?1",
                params![id, status.as_str(), updated_at, completed_at],
            )?;
            if changed == 0 {
                return Err(DatabaseError::NotFound);
            }
            connection
                .query_row(
                    "SELECT id, workspace_id, server_id, objective, success_criteria, scope, mode,
                            permission_mode, automation_level, created_by, phase, status,
                            max_steps, max_run_ms, max_attempts, created_at, updated_at,
                        completed_at, active_run_id, last_failure, guardrails
                     FROM investigation_tasks WHERE id = ?1",
                    params![id],
                    row_to_task,
                )
                .map_err(DatabaseError::from)
        })
    }

    /// Advance a task only if the event still belongs to the run that owns it.
    ///
    /// The caller may have read `active_run_id` immediately before this update, but a
    /// retry/recovery command can replace it in between those two operations.  Keeping the
    /// expected run in the SQL predicate makes the event fence atomic instead of relying on a
    /// best-effort read/check/write sequence.
    pub fn update_task_status_if_active(
        &self,
        id: &str,
        status: TaskStatus,
        updated_at: &str,
        completed_at: Option<&str>,
        expected_run_id: &str,
    ) -> Result<Option<InvestigationTask>> {
        self.db.with(|connection| {
            let changed = connection.execute(
                "UPDATE investigation_tasks
                    SET status = ?2,
                        phase = CASE ?2
                            WHEN 'waiting_user' THEN 'decision'
                            WHEN 'executing' THEN 'execution'
                            WHEN 'verifying' THEN 'verification'
                            WHEN 'completed' THEN 'completed'
                            WHEN 'failed' THEN 'recovery'
                            WHEN 'stopped' THEN 'recovery'
                            WHEN 'expired' THEN 'recovery'
                            ELSE 'investigating'
                        END,
                        updated_at = ?3, completed_at = ?4
                  WHERE id = ?1 AND active_run_id = ?5",
                params![
                    id,
                    status.as_str(),
                    updated_at,
                    completed_at,
                    expected_run_id
                ],
            )?;
            if changed == 0 {
                return Ok(None);
            }
            connection
                .query_row(
                    "SELECT id, workspace_id, server_id, objective, success_criteria, scope, mode,
                            permission_mode, automation_level, created_by, phase, status,
                            max_steps, max_run_ms, max_attempts, created_at, updated_at,
                            completed_at, active_run_id, last_failure, guardrails
                     FROM investigation_tasks WHERE id = ?1",
                    params![id],
                    row_to_task,
                )
                .map(Some)
                .map_err(DatabaseError::from)
        })
    }

    pub fn update_task_progress(
        &self,
        update: &TaskProgressUpdate<'_>,
    ) -> Result<InvestigationTask> {
        let failure = update.last_failure.map(serde_json::to_string).transpose()?;
        self.db.with(|connection| {
            let changed = connection.execute(
                "UPDATE investigation_tasks
                    SET status = ?2, phase = ?3, active_run_id = ?4, last_failure = ?5,
                        updated_at = ?6, completed_at = ?7
                  WHERE id = ?1",
                params![
                    update.id,
                    update.status.as_str(),
                    update.phase.as_str(),
                    update.active_run_id,
                    failure,
                    update.updated_at,
                    update.completed_at
                ],
            )?;
            if changed == 0 {
                return Err(DatabaseError::NotFound);
            }
            connection
                .query_row(
                    "SELECT id, workspace_id, server_id, objective, success_criteria, scope, mode,
                            permission_mode, automation_level, created_by, phase, status,
                            max_steps, max_run_ms, max_attempts, created_at, updated_at,
                            completed_at, active_run_id, last_failure, guardrails
                     FROM investigation_tasks WHERE id = ?1",
                    params![update.id],
                    row_to_task,
                )
                .map_err(DatabaseError::from)
        })
    }

    /// Persist run finalisation only while the task still points at that run.
    ///
    /// Returning `None` is an expected stale-event outcome, not a missing task: a newer run or
    /// recovery transaction has already taken ownership of the task.
    pub fn update_task_progress_if_active(
        &self,
        update: &TaskProgressUpdate<'_>,
        expected_run_id: &str,
    ) -> Result<Option<InvestigationTask>> {
        let failure = update.last_failure.map(serde_json::to_string).transpose()?;
        self.db.with(|connection| {
            let changed = connection.execute(
                "UPDATE investigation_tasks
                    SET status = ?2, phase = ?3, active_run_id = ?4, last_failure = ?5,
                        updated_at = ?6, completed_at = ?7
                  WHERE id = ?1 AND active_run_id = ?8",
                params![
                    update.id,
                    update.status.as_str(),
                    update.phase.as_str(),
                    update.active_run_id,
                    failure,
                    update.updated_at,
                    update.completed_at,
                    expected_run_id,
                ],
            )?;
            if changed == 0 {
                return Ok(None);
            }
            connection
                .query_row(
                    "SELECT id, workspace_id, server_id, objective, success_criteria, scope, mode,
                            permission_mode, automation_level, created_by, phase, status,
                            max_steps, max_run_ms, max_attempts, created_at, updated_at,
                            completed_at, active_run_id, last_failure, guardrails
                     FROM investigation_tasks WHERE id = ?1",
                    params![update.id],
                    row_to_task,
                )
                .map(Some)
                .map_err(DatabaseError::from)
        })
    }

    pub fn create_run(&self, run: &InvestigationRun) -> Result<()> {
        let checkpoint = run
            .checkpoint
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let failure = run
            .failure
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO investigation_runs (
                    id, task_id, session_id, message_id, trace_id, attempt, phase, status,
                    started_at, updated_at, ended_at, checkpoint, failure
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    run.id,
                    run.task_id,
                    run.session_id,
                    run.message_id,
                    run.trace_id,
                    i64::from(run.attempt),
                    run.phase.as_str(),
                    run.status.as_str(),
                    run.started_at,
                    run.updated_at,
                    run.ended_at,
                    checkpoint,
                    failure,
                ],
            )?;
            Ok(())
        })
    }

    pub fn get_run(&self, id: &str) -> Result<InvestigationRun> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, task_id, session_id, message_id, trace_id, attempt, phase, status,
                            started_at, updated_at, ended_at, checkpoint, failure
                     FROM investigation_runs WHERE id = ?1",
                    params![id],
                    row_to_run,
                )
                .optional()
                .map_err(DatabaseError::from)?
                .ok_or(DatabaseError::NotFound)
        })
    }

    pub fn list_runs(&self, task_id: &str, limit: usize) -> Result<Vec<InvestigationRun>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, task_id, session_id, message_id, trace_id, attempt, phase, status,
                        started_at, updated_at, ended_at, checkpoint, failure
                 FROM investigation_runs
                 WHERE task_id = ?1 ORDER BY updated_at DESC, id DESC LIMIT ?2",
            )?;
            let rows = statement.query_map(params![task_id, limit as i64], row_to_run)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
        })
    }

    pub fn update_run(&self, run: &InvestigationRun) -> Result<InvestigationRun> {
        let checkpoint = run
            .checkpoint
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let failure = run
            .failure
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        self.db.with(|connection| {
            let changed = connection.execute(
                "UPDATE investigation_runs
                    SET session_id = ?2, message_id = ?3, trace_id = ?4, attempt = ?5,
                        phase = ?6, status = ?7, started_at = ?8, updated_at = ?9,
                        ended_at = ?10, checkpoint = ?11, failure = ?12
                  WHERE id = ?1",
                params![
                    run.id,
                    run.session_id,
                    run.message_id,
                    run.trace_id,
                    i64::from(run.attempt),
                    run.phase.as_str(),
                    run.status.as_str(),
                    run.started_at,
                    run.updated_at,
                    run.ended_at,
                    checkpoint,
                    failure,
                ],
            )?;
            if changed == 0 {
                return Err(DatabaseError::NotFound);
            }
            connection
                .query_row(
                    "SELECT id, task_id, session_id, message_id, trace_id, attempt, phase, status,
                            started_at, updated_at, ended_at, checkpoint, failure
                     FROM investigation_runs WHERE id = ?1",
                    params![run.id],
                    row_to_run,
                )
                .map_err(DatabaseError::from)
        })
    }

    pub fn upsert_step(&self, step: &InvestigationStep) -> Result<()> {
        let target = step
            .target
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let failure = step
            .failure
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO investigation_steps (
                    id, task_id, run_id, ordinal, kind, title, status, attempt, tool_name,
                    plan_id, plan_step_id, target, input_summary, output_summary, evidence_ids,
                    started_at, ended_at, failure
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
                 ON CONFLICT(id) DO UPDATE SET
                    task_id = excluded.task_id, run_id = excluded.run_id, ordinal = excluded.ordinal,
                    kind = excluded.kind, title = excluded.title, status = excluded.status,
                    attempt = excluded.attempt, tool_name = excluded.tool_name,
                    plan_id = excluded.plan_id, plan_step_id = excluded.plan_step_id,
                    target = excluded.target, input_summary = excluded.input_summary,
                    output_summary = excluded.output_summary, evidence_ids = excluded.evidence_ids,
                    started_at = excluded.started_at, ended_at = excluded.ended_at,
                    failure = excluded.failure",
                params![
                    step.id,
                    step.task_id,
                    step.run_id,
                    i64::from(step.ordinal),
                    step.kind.as_str(),
                    step.title,
                    step.status.as_str(),
                    i64::from(step.attempt),
                    step.tool_name,
                    step.plan_id,
                    step.plan_step_id,
                    target,
                    step.input_summary,
                    step.output_summary,
                    serde_json::to_string(&step.evidence_ids)?,
                    step.started_at,
                    step.ended_at,
                    failure,
                ],
            )?;
            Ok(())
        })
    }

    pub fn list_steps(&self, task_id: &str, limit: usize) -> Result<Vec<InvestigationStep>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, task_id, run_id, ordinal, kind, title, status, attempt, tool_name,
                        plan_id, plan_step_id, target, input_summary, output_summary, evidence_ids,
                        started_at, ended_at, failure
                 FROM investigation_steps
                 WHERE task_id = ?1 ORDER BY ordinal ASC, id ASC LIMIT ?2",
            )?;
            let rows = statement.query_map(params![task_id, limit as i64], row_to_step)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
        })
    }

    pub fn recover_task(
        &self,
        task_id: &str,
        updated_at: &str,
        failure: &InvestigationFailure,
    ) -> Result<InvestigationTask> {
        let failure_json = serde_json::to_string(failure)?;
        self.db.with(|connection| {
            let tx = connection.unchecked_transaction()?;
            // Close every step that was still open when the run was interrupted. A
            // recovered task must not leave a phantom in-flight step in its timeline:
            // active/waiting steps are failed with the durable interruption reason,
            // while steps that had not started are explicitly skipped. Completed and
            // previously failed steps remain immutable audit history.
            tx.execute(
                "UPDATE investigation_steps
                    SET status = CASE status
                                   WHEN 'pending' THEN 'skipped'
                                   ELSE 'failed'
                                 END,
                        ended_at = ?2,
                        failure = CASE
                                    WHEN status = 'pending' THEN NULL
                                    ELSE ?3
                                  END
                  WHERE task_id = ?1
                    AND status IN ('pending','running','waiting_user')
                    AND run_id IN (
                        SELECT id FROM investigation_runs
                         WHERE task_id = ?1 AND status IN ('admitted','running','waiting_user')
                    )",
                params![task_id, updated_at, failure_json],
            )?;
            let changed = tx.execute(
                "UPDATE investigation_runs
                    SET status = 'interrupted', updated_at = ?2, ended_at = ?2, failure = ?3
                  WHERE task_id = ?1 AND status IN ('admitted','running','waiting_user')",
                params![task_id, updated_at, failure_json],
            )?;
            let _ = changed;
            let task_changed = tx.execute(
                "UPDATE investigation_tasks
                    SET status = 'investigating', phase = 'recovery', active_run_id = NULL,
                        last_failure = ?2, updated_at = ?3, completed_at = NULL
                  WHERE id = ?1 AND status NOT IN ('completed','expired')",
                params![task_id, failure_json, updated_at],
            )?;
            if task_changed == 0 {
                return Err(DatabaseError::NotFound);
            }
            tx.commit()?;
            connection
                .query_row(
                    "SELECT id, workspace_id, server_id, objective, success_criteria, scope, mode,
                            permission_mode, automation_level, created_by, phase, status,
                            max_steps, max_run_ms, max_attempts, created_at, updated_at,
                            completed_at, active_run_id, last_failure, guardrails
                     FROM investigation_tasks WHERE id = ?1",
                    params![task_id],
                    row_to_task,
                )
                .map_err(DatabaseError::from)
        })
    }

    pub fn add_evidence(&self, evidence: &Evidence) -> Result<()> {
        self.validate_evidence(evidence)?;
        let content = serde_json::to_string(&evidence.content)?;
        let scope = serde_json::to_string(&evidence.scope)?;
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO investigation_evidence (
                    id, task_id, run_id, scope, kind, source_tool, collected_at, input_summary,
                    content_type, content, content_hash, truncated, redaction_status
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    evidence.id,
                    evidence.task_id,
                    evidence.run_id,
                    scope,
                    evidence.kind.as_str(),
                    evidence.source_tool,
                    evidence.collected_at,
                    evidence.input_summary,
                    evidence.content_type.as_str(),
                    content,
                    evidence.content_hash,
                    if evidence.truncated { 1_i64 } else { 0_i64 },
                    evidence.redaction_status.as_str(),
                ],
            )?;
            Ok(())
        })
    }

    /// Validate the bounded, redacted evidence envelope without mutating the database.
    ///
    /// The host uses this before same-run deduplication so an invalid retry cannot
    /// reuse a previously persisted row merely because its metadata happens to match.
    pub fn validate_evidence(&self, evidence: &Evidence) -> Result<()> {
        let content = serde_json::to_string(&evidence.content)?;
        if content.len() > MAX_EVIDENCE_SERIALIZED_BYTES {
            return Err(DatabaseError::Validation(format!(
                "evidence content exceeds {MAX_EVIDENCE_SERIALIZED_BYTES} bytes"
            )));
        }
        if evidence.redaction_status == EvidenceRedactionStatus::Unknown {
            return Err(DatabaseError::Validation(
                "evidence must be redacted before persistence".into(),
            ));
        }
        let actual_hash = content_hash(&evidence.content);
        if actual_hash != evidence.content_hash {
            return Err(DatabaseError::Validation(
                "evidence contentHash does not match content".into(),
            ));
        }
        Ok(())
    }

    /// Find an identical observation already persisted for one durable run.
    ///
    /// The run id is deliberately part of the key: two scheduled samples with
    /// identical content are still distinct observations that must remain
    /// comparable across runs.  This lookup only collapses a repeated record
    /// inside the same run, after the host has already assigned the run id.
    pub fn find_evidence_in_run(&self, evidence: &Evidence) -> Result<Option<Evidence>> {
        let Some(run_id) = evidence.run_id.as_deref() else {
            return Ok(None);
        };
        let scope = serde_json::to_string(&evidence.scope)?;
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, task_id, run_id, scope, kind, source_tool, collected_at, input_summary,
                            content_type, content, content_hash, truncated, redaction_status
                     FROM investigation_evidence
                     WHERE task_id = ?1 AND run_id = ?2 AND scope = ?3
                       AND kind = ?4 AND source_tool = ?5 AND input_summary = ?6
                       AND content_hash = ?7 AND truncated = ?8 AND content_type = ?9
                     ORDER BY collected_at DESC, id DESC LIMIT 1",
                    params![
                        evidence.task_id,
                        run_id,
                        scope,
                        evidence.kind.as_str(),
                        evidence.source_tool,
                        evidence.input_summary,
                        evidence.content_hash,
                        if evidence.truncated { 1_i64 } else { 0_i64 },
                        evidence.content_type.as_str(),
                    ],
                    row_to_evidence,
                )
                .optional()
                .map_err(DatabaseError::from)
        })
    }

    pub fn list_evidence(&self, task_id: &str, limit: usize) -> Result<Vec<Evidence>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, task_id, run_id, scope, kind, source_tool, collected_at, input_summary,
                        content_type, content, content_hash, truncated, redaction_status
                 FROM investigation_evidence
                 WHERE task_id = ?1 ORDER BY collected_at DESC, id DESC LIMIT ?2",
            )?;
            let rows = statement.query_map(params![task_id, limit as i64], row_to_evidence)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
        })
    }

    pub fn get_evidence(&self, id: &str) -> Result<Evidence> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, task_id, run_id, scope, kind, source_tool, collected_at, input_summary,
                            content_type, content, content_hash, truncated, redaction_status
                     FROM investigation_evidence WHERE id = ?1",
                    params![id],
                    row_to_evidence,
                )
                .optional()
                .map_err(DatabaseError::from)?
                .ok_or(DatabaseError::NotFound)
        })
    }

    /// Search one task's evidence by bounded metadata only. The body is returned
    /// to the host for summary projection, but the sidecar search contract never
    /// includes it; callers must fetch one id explicitly to read content.
    pub fn search_evidence(
        &self,
        task_id: &str,
        query: &EvidenceSearchQuery,
    ) -> Result<Vec<Evidence>> {
        if !(1..=64).contains(&query.limit) {
            return Err(DatabaseError::Validation(
                "evidence search limit must be between 1 and 64".into(),
            ));
        }
        if query
            .source_tool
            .as_deref()
            .is_some_and(|value| value.trim().is_empty() || value.chars().count() > 256)
        {
            return Err(DatabaseError::Validation(
                "evidence source tool must be between 1 and 256 characters".into(),
            ));
        }
        for timestamp in [query.from.as_deref(), query.to.as_deref()]
            .into_iter()
            .flatten()
        {
            if yukinal_time::parse_iso8601_utc(timestamp).is_none() {
                return Err(DatabaseError::Validation(
                    "evidence search timestamps must be UTC ISO-8601 values".into(),
                ));
            }
        }
        if let (Some(from), Some(to)) = (query.from.as_deref(), query.to.as_deref()) {
            if yukinal_time::parse_iso8601_utc(from) > yukinal_time::parse_iso8601_utc(to) {
                return Err(DatabaseError::Validation(
                    "evidence search from must not be after to".into(),
                ));
            }
        }
        let scope = query
            .scope
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, task_id, run_id, scope, kind, source_tool, collected_at, input_summary,
                        content_type, content, content_hash, truncated, redaction_status
                 FROM investigation_evidence
                 WHERE task_id = ?1
                   AND (?2 IS NULL OR source_tool = ?2)
                   AND (?3 IS NULL OR kind = ?3)
                   AND (?4 IS NULL OR collected_at >= ?4)
                   AND (?5 IS NULL OR collected_at <= ?5)
                   AND (?6 IS NULL OR scope = ?6)
                 ORDER BY collected_at DESC, id DESC LIMIT ?7",
            )?;
            let rows = statement.query_map(
                params![
                    task_id,
                    query.source_tool.as_deref(),
                    query.kind.map(|kind| kind.as_str()),
                    query.from.as_deref(),
                    query.to.as_deref(),
                    scope,
                    query.limit as i64,
                ],
                row_to_evidence,
            )?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
        })
    }

    /// Return the evidence collected by one host-owned run. This is intentionally a
    /// separate query from the task-wide timeline: scheduled comparisons must never
    /// accidentally include a previous run or a user-started run.
    pub fn list_evidence_for_run(&self, run_id: &str, limit: usize) -> Result<Vec<Evidence>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, task_id, run_id, scope, kind, source_tool, collected_at, input_summary,
                        content_type, content, content_hash, truncated, redaction_status
                 FROM investigation_evidence
                 WHERE run_id = ?1 ORDER BY collected_at ASC, id ASC LIMIT ?2",
            )?;
            let rows = statement.query_map(params![run_id, limit as i64], row_to_evidence)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
        })
    }

    /// Compare one scheduled run with its explicit baseline, or the latest successful run of
    /// the same schedule when no baseline was selected. The comparison is intentionally
    /// content-fingerprint based: collection timestamps and row ids are excluded, while
    /// source, input summary, kind, truncation and content hash remain part of the signal.
    /// Empty samples are reported as insufficient evidence rather than a false "no change".
    pub fn compare_schedule_run(&self, run_id: &str) -> Result<InvestigationScheduleComparison> {
        self.db.with(|connection| {
            let (schedule_id, baseline_run_id): (String, Option<String>) = connection
                .query_row(
                    "SELECT r.schedule_id, s.baseline_run_id
                     FROM investigation_schedule_runs r
                     JOIN investigation_schedules s ON s.id = r.schedule_id
                     WHERE r.id = ?1",
                    params![run_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .ok_or(DatabaseError::NotFound)?;
            let previous_run_id: Option<String> = if let Some(baseline_run_id) = baseline_run_id {
                // An explicit baseline is deliberately fail-closed when its evidence has
                // been pruned or the run was otherwise corrupted. Falling back to the
                // latest sample would silently change the user's comparison contract.
                connection
                    .query_row(
                        "SELECT id FROM investigation_schedule_runs
                         WHERE id = ?1 AND schedule_id = ?2 AND id <> ?3
                           AND status = 'succeeded' AND finished_at IS NOT NULL
                           AND EXISTS (
                               SELECT 1 FROM investigation_evidence e WHERE e.run_id = investigation_schedule_runs.id
                           )",
                        params![baseline_run_id, schedule_id, run_id],
                        |row| row.get(0),
                    )
                    .optional()?
            } else {
                connection
                    .query_row(
                        "SELECT id FROM investigation_schedule_runs
                         WHERE schedule_id = ?1 AND id <> ?2 AND status = 'succeeded'
                           AND finished_at IS NOT NULL
                           AND EXISTS (
                               SELECT 1 FROM investigation_evidence e WHERE e.run_id = investigation_schedule_runs.id
                           )
                         ORDER BY finished_at DESC, id DESC LIMIT 1",
                        params![schedule_id, run_id],
                        |row| row.get(0),
                    )
                    .optional()?
            };
            let current = query_evidence_for_run(connection, run_id)?;
            let previous = previous_run_id
                .as_deref()
                .map(|id| query_evidence_for_run(connection, id))
                .transpose()?
                .unwrap_or_default();
            let current_signature = evidence_signature(&current);
            let previous_signature = evidence_signature(&previous);
            let status = match (&current_signature, &previous_signature) {
                (None, _) => InvestigationScheduleComparisonStatus::InsufficientEvidence,
                (Some(_), None) => InvestigationScheduleComparisonStatus::Baseline,
                (Some(current), Some(previous)) if current == previous => {
                    InvestigationScheduleComparisonStatus::NoChange
                }
                (Some(_), Some(_)) => InvestigationScheduleComparisonStatus::Changed,
            };
            Ok(InvestigationScheduleComparison {
                status,
                current_signature,
                previous_signature,
                current_evidence_ids: current.into_iter().map(|evidence| evidence.id).collect(),
                previous_evidence_ids: previous.into_iter().map(|evidence| evidence.id).collect(),
            })
        })
    }

    pub fn add_finding(&self, finding: &Finding) -> Result<()> {
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO investigation_findings (
                    id, task_id, title, kind, statement, evidence_ids, confidence,
                    next_verification, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    finding.id,
                    finding.task_id,
                    finding.title,
                    finding.kind.as_str(),
                    finding.statement,
                    serde_json::to_string(&finding.evidence_ids)?,
                    finding.confidence.as_str(),
                    finding.next_verification,
                    finding.created_at,
                ],
            )?;
            Ok(())
        })
    }

    pub fn list_findings(&self, task_id: &str, limit: usize) -> Result<Vec<Finding>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, task_id, title, kind, statement, evidence_ids, confidence,
                        next_verification, created_at
                 FROM investigation_findings
                 WHERE task_id = ?1 ORDER BY created_at DESC, id DESC LIMIT ?2",
            )?;
            let rows = statement.query_map(params![task_id, limit as i64], row_to_finding)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
        })
    }

    pub fn get_finding(&self, id: &str) -> Result<Finding> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, task_id, title, kind, statement, evidence_ids, confidence,
                            next_verification, created_at
                     FROM investigation_findings WHERE id = ?1",
                    params![id],
                    row_to_finding,
                )
                .optional()
                .map_err(DatabaseError::from)?
                .ok_or(DatabaseError::NotFound)
        })
    }

    pub fn save_decision_brief(&self, brief: &DecisionBrief) -> Result<()> {
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO investigation_decision_briefs (
                    id, task_id, plan_id, generated_at, status, finding_ids, options, selected_option_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    brief.id,
                    brief.task_id,
                    brief.plan_id,
                    brief.generated_at,
                    brief.status.as_str(),
                    serde_json::to_string(&brief.finding_ids)?,
                    serde_json::to_string(&brief.options)?,
                    brief.selected_option_id,
                ],
            )?;
            Ok(())
        })
    }

    pub fn select_decision_brief_option(
        &self,
        task_id: &str,
        brief_id: &str,
        option_id: &str,
    ) -> Result<DecisionBrief> {
        self.db.with(|connection| {
            let brief = connection
                .query_row(
                    "SELECT id, task_id, plan_id, generated_at, status, finding_ids, options, selected_option_id
                     FROM investigation_decision_briefs WHERE id = ?1",
                    params![brief_id],
                    row_to_brief,
                )
                .optional()
                .map_err(DatabaseError::from)?
                .ok_or(DatabaseError::NotFound)?;
            if brief.task_id != task_id {
                return Err(DatabaseError::Validation(
                    "decision brief does not belong to the current investigation task".into(),
                ));
            }
            if brief.status == DecisionBriefStatus::Dismissed {
                return Err(DatabaseError::Validation(
                    "dismissed decision briefs cannot be selected".into(),
                ));
            }
            if !brief.options.iter().any(|option| option.id == option_id) {
                return Err(DatabaseError::Validation(
                    "decision option was not found in the selected brief".into(),
                ));
            }
            let mut options = brief.options.clone();
            for option in &mut options {
                option.status = if option.id == option_id {
                    DecisionOptionStatus::Selected
                } else {
                    DecisionOptionStatus::Rejected
                };
            }
            let options = serde_json::to_string(&options)?;
            connection.execute(
                "UPDATE investigation_decision_briefs
                    SET status = ?3, selected_option_id = ?2, options = ?5
                  WHERE id = ?1 AND task_id = ?4",
                params![
                    brief_id,
                    option_id,
                    DecisionBriefStatus::Selected.as_str(),
                    task_id,
                    options
                ],
            )?;
            connection
                .query_row(
                    "SELECT id, task_id, plan_id, generated_at, status, finding_ids, options, selected_option_id
                     FROM investigation_decision_briefs WHERE id = ?1",
                    params![brief_id],
                    row_to_brief,
                )
                .map_err(DatabaseError::from)
        })
    }

    pub fn latest_decision_brief(&self, task_id: &str) -> Result<Option<DecisionBrief>> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, task_id, plan_id, generated_at, status, finding_ids, options, selected_option_id
                     FROM investigation_decision_briefs
                     WHERE task_id = ?1 ORDER BY generated_at DESC, id DESC LIMIT 1",
                    params![task_id],
                    row_to_brief,
                )
                .optional()
                .map_err(DatabaseError::from)
        })
    }

    /// Save one immutable plan revision. A retry with the same id is idempotent;
    /// saving a new id supersedes the previous draft/active revision for the task.
    pub fn save_plan(&self, plan: &InvestigationPlan) -> Result<()> {
        let steps = serde_json::to_string(&plan.steps)?;
        let approval = plan
            .approval
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let observation_window = plan
            .observation_window
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        self.db.with(|connection| {
            let tx = connection.unchecked_transaction()?;
            let existing_task = tx
                .query_row(
                    "SELECT task_id FROM investigation_plans WHERE id = ?1",
                    params![plan.id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if let Some(task_id) = existing_task {
                if task_id != plan.task_id {
                    return Err(DatabaseError::Validation(
                        "investigation plan id belongs to another task".into(),
                    ));
                }
                tx.execute(
                    "UPDATE investigation_plans
                        SET revision = ?2, status = ?3, created_at = ?4, updated_at = ?5,
                            current_step_id = ?6, steps = ?7, approval = ?8, observation_window = ?9
                      WHERE id = ?1",
                    params![
                        plan.id,
                        i64::from(plan.revision),
                        plan.status.as_str(),
                        plan.created_at,
                        plan.updated_at,
                        plan.current_step_id,
                        steps,
                        approval,
                        observation_window,
                    ],
                )?;
            } else {
                tx.execute(
                    "UPDATE investigation_plans SET status = 'superseded', updated_at = ?2
                      WHERE task_id = ?1 AND status IN ('draft','active')",
                    params![plan.task_id, plan.updated_at],
                )?;
                tx.execute(
                    "INSERT INTO investigation_plans
                        (id, task_id, revision, status, created_at, updated_at, current_step_id, steps, approval, observation_window)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        plan.id,
                        plan.task_id,
                        i64::from(plan.revision),
                        plan.status.as_str(),
                        plan.created_at,
                        plan.updated_at,
                        plan.current_step_id,
                        steps,
                        approval,
                        observation_window,
                    ],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn get_plan(&self, id: &str) -> Result<InvestigationPlan> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, task_id, revision, status, created_at, updated_at,
                            current_step_id, steps, approval, observation_window
                     FROM investigation_plans WHERE id = ?1",
                    params![id],
                    row_to_plan,
                )
                .optional()
                .map_err(DatabaseError::from)?
                .ok_or(DatabaseError::NotFound)
        })
    }

    pub fn latest_plan(&self, task_id: &str) -> Result<Option<InvestigationPlan>> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, task_id, revision, status, created_at, updated_at,
                            current_step_id, steps, approval, observation_window
                     FROM investigation_plans
                     WHERE task_id = ?1 AND status IN ('draft','active','completed')
                     ORDER BY revision DESC, id DESC LIMIT 1",
                    params![task_id],
                    row_to_plan,
                )
                .optional()
                .map_err(DatabaseError::from)
        })
    }

    /// Insert or update one bounded phase artifact. Replaying the same id is idempotent;
    /// a newer artifact id remains a separate immutable observation in the task timeline.
    pub fn upsert_artifact(&self, artifact: &InvestigationArtifact) -> Result<()> {
        let content = serde_json::to_string(&artifact.content)?;
        if content.len() > MAX_ARTIFACT_SERIALIZED_BYTES {
            return Err(DatabaseError::Validation(
                "investigation artifact content exceeds the 1 MiB limit".into(),
            ));
        }
        let evidence_ids = serde_json::to_string(&artifact.evidence_ids)?;
        self.db.with(|connection| {
            let existing_task = connection
                .query_row(
                    "SELECT task_id FROM investigation_artifacts WHERE id = ?1",
                    params![artifact.id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if let Some(task_id) = existing_task {
                if task_id != artifact.task_id {
                    return Err(DatabaseError::Validation(
                        "investigation artifact id belongs to another task".into(),
                    ));
                }
            }
            connection.execute(
                "INSERT INTO investigation_artifacts (
                    id, task_id, run_id, plan_id, plan_step_id, phase, kind, status, title, summary, content,
                    evidence_ids, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                 ON CONFLICT(id) DO UPDATE SET
                    run_id = excluded.run_id,
                    plan_id = excluded.plan_id,
                    plan_step_id = excluded.plan_step_id,
                    phase = excluded.phase,
                    kind = excluded.kind,
                    status = excluded.status,
                    title = excluded.title,
                    summary = excluded.summary,
                    content = excluded.content,
                    evidence_ids = excluded.evidence_ids,
                    updated_at = excluded.updated_at",
                params![
                    artifact.id,
                    artifact.task_id,
                    artifact.run_id,
                    artifact.plan_id,
                    artifact.plan_step_id,
                    artifact.phase.as_str(),
                    artifact.kind.as_str(),
                    artifact.status.as_str(),
                    artifact.title,
                    artifact.summary,
                    content,
                    evidence_ids,
                    artifact.created_at,
                    artifact.updated_at,
                ],
            )?;
            Ok(())
        })
    }

    pub fn list_artifacts(
        &self,
        task_id: &str,
        limit: usize,
    ) -> Result<Vec<InvestigationArtifact>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, task_id, run_id, plan_id, plan_step_id, phase, kind, status, title, summary, content,
                        evidence_ids, created_at, updated_at
                 FROM investigation_artifacts
                 WHERE task_id = ?1 ORDER BY updated_at DESC, id DESC LIMIT ?2",
            )?;
            let rows = statement.query_map(params![task_id, limit as i64], row_to_artifact)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(DatabaseError::from)
        })
    }

    /// Create a durable read-only trigger. The task's safety envelope is checked at
    /// creation time and again by every tick; changing a task to executable never
    /// silently widens an existing schedule.
    pub fn create_schedule(&self, schedule: &InvestigationSchedule) -> Result<()> {
        validate_schedule_shape(schedule)?;
        let budget = serde_json::to_string(&schedule.budget)?;
        self.db.with(|connection| {
            let task = connection
                .query_row(
                    "SELECT mode, automation_level FROM investigation_tasks WHERE id = ?1",
                    params![schedule.task_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?;
            let Some((mode, automation)) = task else {
                return Err(DatabaseError::NotFound);
            };
            if mode != "readonly" || automation != "readonly" {
                return Err(DatabaseError::Validation(
                    "scheduler triggers may only target read-only investigation tasks".into(),
                ));
            }
            connection.execute(
                "INSERT INTO investigation_schedules (
                    id, task_id, status, interval_seconds, cooldown_seconds, dedupe_window_seconds,
                    max_concurrent_runs, budget, notification_policy, next_run_at, last_run_at,
                    last_outcome, last_error, created_at, updated_at, baseline_run_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    schedule.id,
                    schedule.task_id,
                    schedule.status.as_str(),
                    i64::try_from(schedule.interval_seconds)
                        .map_err(|_| DatabaseError::Validation("interval is too large".into()))?,
                    i64::try_from(schedule.cooldown_seconds)
                        .map_err(|_| DatabaseError::Validation("cooldown is too large".into()))?,
                    i64::try_from(schedule.dedupe_window_seconds).map_err(|_| {
                        DatabaseError::Validation("dedupe window is too large".into())
                    })?,
                    i64::from(schedule.max_concurrent_runs),
                    budget,
                    schedule.notification_policy.as_str(),
                    schedule.next_run_at,
                    schedule.last_run_at,
                    schedule.last_outcome,
                    schedule.last_error,
                    schedule.created_at,
                    schedule.updated_at,
                    schedule.baseline_run_id,
                ],
            )?;
            Ok(())
        })
    }

    pub fn get_schedule(&self, schedule_id: &str) -> Result<InvestigationSchedule> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, task_id, status, interval_seconds, cooldown_seconds,
                            dedupe_window_seconds, max_concurrent_runs, budget,
                            notification_policy, next_run_at, last_run_at, last_outcome,
                            last_error, created_at, updated_at, baseline_run_id
                     FROM investigation_schedules WHERE id = ?1",
                    params![schedule_id],
                    row_to_schedule,
                )
                .optional()
                .map_err(DatabaseError::from)?
                .ok_or(DatabaseError::NotFound)
        })
    }

    pub fn get_schedule_for_run(&self, run_id: &str) -> Result<InvestigationSchedule> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT s.id, s.task_id, s.status, s.interval_seconds, s.cooldown_seconds,
                            s.dedupe_window_seconds, s.max_concurrent_runs, s.budget,
                            s.notification_policy, s.next_run_at, s.last_run_at, s.last_outcome,
                            s.last_error, s.created_at, s.updated_at, s.baseline_run_id
                     FROM investigation_schedules s
                     JOIN investigation_schedule_runs r ON r.schedule_id = s.id
                     WHERE r.id = ?1",
                    params![run_id],
                    row_to_schedule,
                )
                .optional()
                .map_err(DatabaseError::from)?
                .ok_or(DatabaseError::NotFound)
        })
    }

    pub fn list_schedules(&self, limit: usize) -> Result<Vec<InvestigationSchedule>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, task_id, status, interval_seconds, cooldown_seconds,
                        dedupe_window_seconds, max_concurrent_runs, budget,
                        notification_policy, next_run_at, last_run_at, last_outcome,
                        last_error, created_at, updated_at, baseline_run_id
                 FROM investigation_schedules ORDER BY next_run_at ASC, id ASC LIMIT ?1",
            )?;
            let rows = statement.query_map(params![limit as i64], row_to_schedule)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(DatabaseError::from)
        })
    }

    pub fn update_schedule(&self, schedule: &InvestigationSchedule) -> Result<()> {
        validate_schedule_shape(schedule)?;
        let budget = serde_json::to_string(&schedule.budget)?;
        self.db.with(|connection| {
            let changed = connection.execute(
                "UPDATE investigation_schedules SET status = ?2, interval_seconds = ?3,
                        cooldown_seconds = ?4, dedupe_window_seconds = ?5,
                        max_concurrent_runs = ?6, budget = ?7, notification_policy = ?8,
                        next_run_at = ?9, last_run_at = ?10, last_outcome = ?11,
                        last_error = ?12, updated_at = ?13, baseline_run_id = ?14 WHERE id = ?1",
                params![
                    schedule.id,
                    schedule.status.as_str(),
                    i64::try_from(schedule.interval_seconds)
                        .map_err(|_| DatabaseError::Validation("interval is too large".into()))?,
                    i64::try_from(schedule.cooldown_seconds)
                        .map_err(|_| DatabaseError::Validation("cooldown is too large".into()))?,
                    i64::try_from(schedule.dedupe_window_seconds).map_err(|_| {
                        DatabaseError::Validation("dedupe window is too large".into())
                    })?,
                    i64::from(schedule.max_concurrent_runs),
                    budget,
                    schedule.notification_policy.as_str(),
                    schedule.next_run_at,
                    schedule.last_run_at,
                    schedule.last_outcome,
                    schedule.last_error,
                    schedule.updated_at,
                    schedule.baseline_run_id,
                ],
            )?;
            if changed == 0 {
                return Err(DatabaseError::NotFound);
            }
            Ok(())
        })
    }

    pub fn list_schedule_runs(
        &self,
        schedule_id: &str,
        limit: usize,
    ) -> Result<Vec<InvestigationScheduleRun>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, schedule_id, task_id, status, scheduled_at, claimed_at,
                        started_at, finished_at, dedupe_key, outcome, error
                 FROM investigation_schedule_runs
                 WHERE schedule_id = ?1 ORDER BY scheduled_at DESC, id DESC LIMIT ?2",
            )?;
            let rows =
                statement.query_map(params![schedule_id, limit as i64], row_to_schedule_run)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(DatabaseError::from)
        })
    }

    /// Read one durable schedule run for host-side baseline validation. The
    /// schedule update command uses this rather than trusting a UI-provided run
    /// from a different trigger or an unbounded history page.
    pub fn get_schedule_run(&self, run_id: &str) -> Result<InvestigationScheduleRun> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, schedule_id, task_id, status, scheduled_at, claimed_at,
                            started_at, finished_at, dedupe_key, outcome, error
                     FROM investigation_schedule_runs WHERE id = ?1",
                    params![run_id],
                    row_to_schedule_run,
                )
                .optional()
                .map_err(DatabaseError::from)?
                .ok_or(DatabaseError::NotFound)
        })
    }

    /// Atomically claim due schedules. The transaction makes a UI refresh, a second
    /// scheduler tick and startup recovery observe one run at most. It returns skipped
    /// rows as first-class records so the user can see why a trigger did not run.
    pub fn claim_due_schedule_runs(
        &self,
        now: &str,
        limit: usize,
    ) -> Result<(Vec<InvestigationScheduleRun>, Vec<InvestigationScheduleRun>)> {
        let now_epoch = yukinal_time::parse_iso8601_utc(now).ok_or_else(|| {
            DatabaseError::Validation("scheduler now must be a UTC timestamp".into())
        })?;
        self.db.with(|connection| {
            let tx = connection.unchecked_transaction()?;
            let mut statement = tx.prepare(
                "SELECT id, task_id, status, interval_seconds, cooldown_seconds,
                        dedupe_window_seconds, max_concurrent_runs, budget,
                        notification_policy, next_run_at, last_run_at, last_outcome,
                        last_error, created_at, updated_at, baseline_run_id
                 FROM investigation_schedules
                 WHERE status = 'active' AND next_run_at <= ?1
                 ORDER BY next_run_at ASC, id ASC LIMIT ?2",
            )?;
            let schedules = statement
                .query_map(params![now, limit as i64], row_to_schedule)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(statement);
            let mut claimed = Vec::new();
            let mut skipped = Vec::new();
            for schedule in schedules {
                let task = tx
                    .query_row(
                        "SELECT status, mode, automation_level FROM investigation_tasks WHERE id = ?1",
                        params![schedule.task_id],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                            ))
                        },
                    )
                    .optional()?;
                let due_at = schedule.next_run_at.clone();
                let due_epoch = yukinal_time::parse_iso8601_utc(&due_at).unwrap_or(now_epoch);
                let dedupe_bucket = due_epoch / schedule.dedupe_window_seconds.max(1);
                let dedupe_key = format!("bucket:{dedupe_bucket}");
                let next_run_at = yukinal_time::iso8601_utc(
                    now_epoch.saturating_add(schedule.interval_seconds),
                );
                let existing = tx
                    .query_row(
                        "SELECT id, schedule_id, task_id, status, scheduled_at, claimed_at,
                                started_at, finished_at, dedupe_key, outcome, error
                         FROM investigation_schedule_runs
                         WHERE schedule_id = ?1 AND dedupe_key = ?2",
                        params![schedule.id, dedupe_key],
                        row_to_schedule_run,
                    )
                    .optional()?;
                if let Some(existing) = existing {
                    tx.execute(
                        "UPDATE investigation_schedules SET next_run_at = ?2, updated_at = ?3 WHERE id = ?1",
                        params![schedule.id, next_run_at, now],
                    )?;
                    let task_terminal = task.as_ref().is_some_and(|(status, _, _)| {
                        matches!(status.as_str(), "completed" | "failed" | "stopped" | "expired")
                    });
                    if task_terminal || existing.outcome.as_deref() == Some("task_terminal") {
                        // Keep upgrades and manually repaired rows quiet too: an older
                        // terminal skip may already occupy this dedupe bucket before the
                        // current claim reaches the reason-mapping branch below.
                        tx.execute(
                            "UPDATE investigation_schedules SET status = 'revoked' WHERE id = ?1",
                            params![schedule.id],
                        )?;
                    }
                    if matches!(
                        existing.status,
                        InvestigationScheduleRunStatus::Claimed | InvestigationScheduleRunStatus::Running
                    ) {
                        claimed.push(existing);
                    } else {
                        skipped.push(existing);
                    }
                    continue;
                }
                let reason = match task {
                    Some((status, mode, automation))
                        if mode == "readonly"
                            && automation == "readonly"
                            && matches!(status.as_str(), "pending" | "investigating" | "waiting_user") => None,
                    Some((status, _, _)) if matches!(status.as_str(), "completed" | "failed" | "stopped" | "expired") => {
                        Some("task_terminal")
                    }
                    Some(_) => Some("task_not_readonly_or_waiting_user"),
                    None => Some("task_missing"),
                };
                let active_count: u32 = tx.query_row(
                    "SELECT COUNT(*) FROM investigation_schedule_runs
                     WHERE schedule_id = ?1 AND status IN ('queued','claimed','running')",
                    params![schedule.id],
                    |row| row.get::<_, i64>(0),
                )?.try_into().unwrap_or(u32::MAX);
                let cooldown_active = schedule
                    .last_run_at
                    .as_deref()
                    .and_then(yukinal_time::parse_iso8601_utc)
                    .is_some_and(|last| now_epoch.saturating_sub(last) < schedule.cooldown_seconds);
                let concurrency_reason = if reason.is_some() {
                    reason
                } else if cooldown_active {
                    Some("cooldown")
                } else if active_count >= schedule.max_concurrent_runs {
                    Some("concurrency_limit")
                } else {
                    None
                };
                let run_id = schedule_run_id(&schedule.id, &dedupe_key);
                let run = if let Some(reason) = concurrency_reason {
                    let run = InvestigationScheduleRun {
                        id: run_id,
                        schedule_id: schedule.id.clone(),
                        task_id: schedule.task_id.clone(),
                        status: InvestigationScheduleRunStatus::Skipped,
                        scheduled_at: due_at,
                        claimed_at: None,
                        started_at: None,
                        finished_at: Some(now.to_string()),
                        dedupe_key,
                        outcome: Some(reason.to_string()),
                        error: None,
                    };
                    insert_schedule_run(&tx, &run)?;
                    skipped.push(run.clone());
                    run
                } else {
                    let run = InvestigationScheduleRun {
                        id: run_id,
                        schedule_id: schedule.id.clone(),
                        task_id: schedule.task_id.clone(),
                        status: InvestigationScheduleRunStatus::Claimed,
                        scheduled_at: due_at,
                        claimed_at: Some(now.to_string()),
                        started_at: None,
                        finished_at: None,
                        dedupe_key,
                        outcome: None,
                        error: None,
                    };
                    insert_schedule_run(&tx, &run)?;
                    claimed.push(run.clone());
                    run
                };
                let last_run_at = if run.status == InvestigationScheduleRunStatus::Skipped {
                    schedule.last_run_at
                } else {
                    Some(now.to_string())
                };
                if run.status == InvestigationScheduleRunStatus::Skipped {
                    // A skipped trigger is itself a terminal schedule outcome.  A claimed
                    // run, however, is only an in-flight marker: keep the previous terminal
                    // outcome so the scheduler can give the sidecar a meaningful changed /
                    // no_change / baseline follow-up prompt.
                    tx.execute(
                        "UPDATE investigation_schedules SET next_run_at = ?2, last_run_at = ?3,
                                last_outcome = ?4, last_error = ?5, updated_at = ?6 WHERE id = ?1",
                        params![
                            schedule.id,
                            next_run_at,
                            last_run_at,
                            run.outcome,
                            run.error,
                            now
                        ],
                    )?;
                    if run.outcome.as_deref() == Some("task_terminal") {
                        // A terminal task can no longer be scheduled. Revoke the
                        // trigger in the same transaction as the skipped run so
                        // every later tick stays quiet and auditable.
                        tx.execute(
                            "UPDATE investigation_schedules SET status = 'revoked' WHERE id = ?1",
                            params![schedule.id],
                        )?;
                    }
                } else {
                    tx.execute(
                        "UPDATE investigation_schedules SET next_run_at = ?2, last_run_at = ?3,
                                updated_at = ?3 WHERE id = ?1",
                        params![schedule.id, next_run_at, last_run_at],
                    )?;
                }
            }
            tx.commit()?;
            Ok((claimed, skipped))
        })
    }

    /// Close a scheduled run that failed before the sidecar could start.
    ///
    /// A schedule claim creates both an investigation run and a schedule run.
    /// The launch failure therefore has to clear the task fence, terminate the
    /// investigation row, close open steps, and finish the schedule row in one
    /// transaction. Repeating the call is safe once the combined terminal state
    /// has already been persisted.
    #[allow(clippy::too_many_arguments)]
    pub fn fail_schedule_run_launch(
        &self,
        task_id: &str,
        run_id: &str,
        message: &str,
        code: TaskFailureCode,
        retryable: bool,
        options: Vec<InvestigationFailureOption>,
        outcome: &str,
        finished_at: &str,
    ) -> Result<InvestigationScheduleRun> {
        let message: String = message.chars().take(4_096).collect();
        let outcome: String = outcome.chars().take(256).collect();
        self.db.with(|connection| {
            let tx = connection.unchecked_transaction()?;
            let schedule_id = tx
                .query_row(
                    "SELECT schedule_id FROM investigation_schedule_runs
                     WHERE id = ?1 AND task_id = ?2",
                    params![run_id, task_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or(DatabaseError::NotFound)?;
            let active_run_id = tx
                .query_row(
                    "SELECT active_run_id FROM investigation_tasks WHERE id = ?1",
                    params![task_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .ok_or(DatabaseError::NotFound)?;
            if active_run_id
                .as_deref()
                .is_some_and(|active_run_id| active_run_id != run_id)
            {
                return Err(DatabaseError::Validation(
                    "a newer investigation run owns the task".into(),
                ));
            }

            let attempt = tx
                .query_row(
                    "SELECT attempt FROM investigation_runs
                     WHERE id = ?1 AND task_id = ?2",
                    params![run_id, task_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .ok_or(DatabaseError::NotFound)?
                .try_into()
                .unwrap_or(u32::MAX);
            let run_status = tx
                .query_row(
                    "SELECT status FROM investigation_runs WHERE id = ?1 AND task_id = ?2",
                    params![run_id, task_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or(DatabaseError::NotFound)?;
            if !matches!(
                run_status.as_str(),
                "admitted" | "running" | "waiting_user" | "interrupted" | "failed"
            ) {
                return Err(DatabaseError::Validation(
                    "investigation run is already terminal and cannot be failed".into(),
                ));
            }

            let failure = InvestigationFailure {
                code,
                message,
                retryable,
                attempt,
                at: finished_at.to_string(),
                detail: None,
                options: Some(options),
            };
            let failure_json = serde_json::to_string(&failure)?;

            tx.execute(
                "UPDATE investigation_steps
                    SET status = CASE status WHEN 'pending' THEN 'skipped' ELSE 'failed' END,
                        ended_at = ?3,
                        failure = CASE WHEN status = 'pending' THEN NULL ELSE ?4 END
                  WHERE task_id = ?1 AND run_id = ?2
                    AND status IN ('pending','running','waiting_user')",
                params![task_id, run_id, finished_at, failure_json],
            )?;
            tx.execute(
                "UPDATE investigation_runs
                    SET status = 'failed', updated_at = ?3,
                        ended_at = ?3, failure = ?4
                  WHERE id = ?1 AND task_id = ?2
                    AND status IN ('admitted','running','waiting_user','interrupted')",
                params![run_id, task_id, finished_at, failure_json],
            )?;
            tx.execute(
                "UPDATE investigation_tasks
                    SET status = 'failed', phase = 'recovery', active_run_id = NULL,
                        last_failure = ?3, updated_at = ?4, completed_at = ?4
                  WHERE id = ?1 AND active_run_id = ?2",
                params![task_id, run_id, failure_json, finished_at],
            )?;

            let schedule_changed = tx.execute(
                "UPDATE investigation_schedule_runs
                    SET status = 'failed', finished_at = ?2,
                        outcome = ?3, error = ?4
                  WHERE id = ?1 AND status IN ('queued','claimed','running','interrupted')",
                params![run_id, finished_at, outcome, failure.message],
            )?;
            let schedule_run_status = tx.query_row(
                "SELECT status FROM investigation_schedule_runs WHERE id = ?1",
                params![run_id],
                |row| row.get::<_, String>(0),
            )?;
            if schedule_changed == 0 && schedule_run_status != "failed" {
                return Err(DatabaseError::Validation(
                    "schedule run is already terminal and cannot be failed".into(),
                ));
            }
            tx.execute(
                "UPDATE investigation_schedules
                    SET last_outcome = ?2, last_error = ?3, updated_at = ?4
                  WHERE id = ?1",
                params![schedule_id, outcome, failure.message, finished_at],
            )?;
            tx.commit()?;

            connection
                .query_row(
                    "SELECT id, schedule_id, task_id, status, scheduled_at, claimed_at,
                            started_at, finished_at, dedupe_key, outcome, error
                     FROM investigation_schedule_runs WHERE id = ?1",
                    params![run_id],
                    row_to_schedule_run,
                )
                .map_err(DatabaseError::from)
        })
    }

    pub fn finish_schedule_run(
        &self,
        run_id: &str,
        status: InvestigationScheduleRunStatus,
        outcome: Option<&str>,
        error: Option<&str>,
        finished_at: &str,
    ) -> Result<InvestigationScheduleRun> {
        self.db.with(|connection| {
            let schedule_id = connection.query_row(
                "SELECT schedule_id FROM investigation_schedule_runs WHERE id = ?1",
                params![run_id],
                |row| row.get::<_, String>(0),
            )?;
            let changed = connection.execute(
                "UPDATE investigation_schedule_runs SET status = ?2, finished_at = ?3,
                        outcome = ?4, error = ?5 WHERE id = ?1
                 AND status IN ('claimed','running','queued')",
                params![run_id, status.as_str(), finished_at, outcome, error],
            )?;
            if changed == 0 {
                return Err(DatabaseError::Validation(
                    "schedule run is missing or already terminal".into(),
                ));
            }
            connection.execute(
                "UPDATE investigation_schedules
                    SET last_outcome = COALESCE(?2, ?3), last_error = ?4, updated_at = ?5
                  WHERE id = ?1",
                params![schedule_id, outcome, status.as_str(), error, finished_at],
            )?;
            connection
                .query_row(
                    "SELECT id, schedule_id, task_id, status, scheduled_at, claimed_at,
                            started_at, finished_at, dedupe_key, outcome, error
                     FROM investigation_schedule_runs WHERE id = ?1",
                    params![run_id],
                    row_to_schedule_run,
                )
                .map_err(DatabaseError::from)
        })
    }

    pub fn start_schedule_run(
        &self,
        run_id: &str,
        started_at: &str,
    ) -> Result<InvestigationScheduleRun> {
        self.db.with(|connection| {
            let changed = connection.execute(
                "UPDATE investigation_schedule_runs SET status = 'running', started_at = ?2
                 WHERE id = ?1 AND status = 'claimed'",
                params![run_id, started_at],
            )?;
            if changed == 0 {
                return Err(DatabaseError::Validation(
                    "schedule run is missing or not claimable".into(),
                ));
            }
            connection
                .query_row(
                    "SELECT id, schedule_id, task_id, status, scheduled_at, claimed_at,
                            started_at, finished_at, dedupe_key, outcome, error
                     FROM investigation_schedule_runs WHERE id = ?1",
                    params![run_id],
                    row_to_schedule_run,
                )
                .map_err(DatabaseError::from)
        })
    }

    /// Close every task whose active run can no longer be owned after an
    /// application restart or sidecar exit. The task fence is part of the
    /// transaction so a stale worker cannot reopen a run after recovery.
    pub fn interrupt_active_investigation_runs(&self, now: &str, reason: &str) -> Result<usize> {
        let reason: String = reason.chars().take(256).collect();
        self.db.with(|connection| {
            let tx = connection.unchecked_transaction()?;
            let active = {
                let mut statement = tx.prepare(
                    "SELECT id, active_run_id FROM investigation_tasks
                     WHERE active_run_id IS NOT NULL
                     ORDER BY id ASC",
                )?;
                let rows = statement.query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };

            for (task_id, run_id) in &active {
                let attempt = tx
                    .query_row(
                        "SELECT attempt FROM investigation_runs
                         WHERE id = ?1 AND task_id = ?2",
                        params![run_id, task_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?
                    .unwrap_or(0)
                    .try_into()
                    .unwrap_or(u32::MAX);
                let failure = InvestigationFailure {
                    code: TaskFailureCode::Transport,
                    message: format!("active investigation run was interrupted: {reason}"),
                    retryable: true,
                    attempt,
                    at: now.to_string(),
                    detail: Some(serde_json::json!({ "reason": reason })),
                    options: None,
                };
                let failure_json = serde_json::to_string(&failure)?;

                tx.execute(
                    "UPDATE investigation_steps
                        SET status = CASE status WHEN 'pending' THEN 'skipped' ELSE 'failed' END,
                            ended_at = ?3,
                            failure = CASE WHEN status = 'pending' THEN NULL ELSE ?4 END
                      WHERE task_id = ?1 AND run_id = ?2
                        AND status IN ('pending','running','waiting_user')",
                    params![task_id, run_id, now, failure_json],
                )?;
                tx.execute(
                    "UPDATE investigation_runs
                        SET status = 'interrupted', updated_at = ?3,
                            ended_at = ?3, failure = ?4
                      WHERE id = ?1 AND task_id = ?2
                        AND status IN ('admitted','running','waiting_user')",
                    params![run_id, task_id, now, failure_json],
                )?;
                tx.execute(
                    "UPDATE investigation_tasks
                        SET status = CASE
                                WHEN status IN ('completed','expired','stopped') THEN status
                                ELSE 'investigating'
                            END,
                            phase = CASE
                                WHEN status IN ('completed','expired','stopped') THEN phase
                                ELSE 'recovery'
                            END,
                            active_run_id = NULL,
                            last_failure = CASE
                                WHEN status IN ('completed','expired') THEN last_failure
                                ELSE ?3
                            END,
                            updated_at = ?4,
                            completed_at = CASE
                                WHEN status IN ('completed','expired','stopped') THEN completed_at
                                ELSE NULL
                            END
                      WHERE id = ?1 AND active_run_id = ?2",
                    params![task_id, run_id, failure_json, now],
                )?;

                // A schedule run owns the same id as its durable investigation
                // run. Keep the scheduler state in sync even when the failure is
                // observed by the general sidecar-exit path.
                tx.execute(
                    "UPDATE investigation_schedule_runs
                        SET status = 'interrupted', finished_at = ?2,
                            outcome = 'interrupted', error = ?3
                      WHERE id = ?1 AND status IN ('claimed','running')",
                    params![run_id, now, reason],
                )?;
                tx.execute(
                    "UPDATE investigation_schedules
                        SET last_outcome = 'interrupted', last_error = ?2, updated_at = ?3
                      WHERE id = (
                          SELECT schedule_id FROM investigation_schedule_runs WHERE id = ?1
                      )",
                    params![run_id, reason, now],
                )?;
            }

            tx.commit()?;
            Ok(active.len())
        })
    }

    /// On application restart, an in-flight scheduler claim cannot be assumed to
    /// have reached the sidecar. Mark the schedule run and its matching durable
    /// investigation run interrupted, close open steps, and clear the task's
    /// active-run fence atomically. The next due tick can then make a deliberate,
    /// deduplicated decision instead of getting stuck behind a dead run id.
    pub fn interrupt_schedule_runs(&self, now: &str) -> Result<usize> {
        self.db.with(|connection| {
            let tx = connection.unchecked_transaction()?;
            let in_flight = {
                let mut statement = tx.prepare(
                    "SELECT id, task_id FROM investigation_schedule_runs
                     WHERE status IN ('claimed','running')
                     ORDER BY id ASC",
                )?;
                let rows = statement
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            for (run_id, task_id) in &in_flight {
                let attempt = tx
                    .query_row(
                        "SELECT attempt FROM investigation_runs
                         WHERE id = ?1 AND task_id = ?2",
                        params![run_id, task_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?
                    .unwrap_or(0)
                    .try_into()
                    .unwrap_or(u32::MAX);
                let failure = InvestigationFailure {
                    code: TaskFailureCode::Transport,
                    message: "应用重启时持续巡检运行被标记为中断".into(),
                    retryable: true,
                    attempt,
                    at: now.to_string(),
                    detail: Some(serde_json::json!({
                        "source": "scheduler",
                        "reason": "application_restarted",
                    })),
                    options: None,
                };
                let failure_json = serde_json::to_string(&failure)?;

                // Do not leave a phantom running/waiting step in the timeline.  The
                // schedule run owns the same id as its durable investigation run.
                tx.execute(
                    "UPDATE investigation_steps
                        SET status = CASE status WHEN 'pending' THEN 'skipped' ELSE 'failed' END,
                            ended_at = ?3,
                            failure = CASE WHEN status = 'pending' THEN NULL ELSE ?4 END
                      WHERE task_id = ?1 AND run_id = ?2
                        AND status IN ('pending','running','waiting_user')",
                    params![task_id, run_id, now, failure_json],
                )?;
                tx.execute(
                    "UPDATE investigation_runs
                        SET status = 'interrupted', updated_at = ?3,
                            ended_at = ?3, failure = ?4
                      WHERE id = ?1 AND task_id = ?2
                        AND status IN ('admitted','running','waiting_user')",
                    params![run_id, task_id, now, failure_json],
                )?;
                // Only clear the fence if this task still points at the interrupted
                // scheduler run. A newer explicit run must retain ownership.
                tx.execute(
                    "UPDATE investigation_tasks
                        SET status = CASE WHEN status = 'stopped' THEN status ELSE 'investigating' END,
                            phase = CASE WHEN status = 'stopped' THEN phase ELSE 'recovery' END,
                            active_run_id = NULL, last_failure = ?3,
                            updated_at = ?4,
                            completed_at = CASE WHEN status = 'stopped' THEN completed_at ELSE NULL END
                      WHERE id = ?1 AND active_run_id = ?2
                        AND status NOT IN ('completed','expired')",
                    params![task_id, run_id, failure_json, now],
                )?;
                tx.execute(
                    "UPDATE investigation_schedule_runs
                        SET status = 'interrupted', finished_at = ?2,
                            outcome = 'interrupted', error = 'application_restarted'
                      WHERE id = ?1 AND status IN ('claimed','running')",
                    params![run_id, now],
                )?;
                tx.execute(
                    "UPDATE investigation_schedules
                        SET last_outcome = 'interrupted', last_error = 'application_restarted',
                            updated_at = ?2
                      WHERE id = (SELECT schedule_id FROM investigation_schedule_runs WHERE id = ?1)",
                    params![run_id, now],
                )?;
            }
            tx.commit()?;
            Ok(in_flight.len())
        })
    }
}

fn validate_schedule_shape(schedule: &InvestigationSchedule) -> Result<()> {
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

fn insert_schedule_run(
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

fn schedule_run_id(schedule_id: &str, dedupe_key: &str) -> String {
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

fn row_to_schedule(row: &Row<'_>) -> rusqlite::Result<InvestigationSchedule> {
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

fn row_to_schedule_run(row: &Row<'_>) -> rusqlite::Result<InvestigationScheduleRun> {
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

fn row_to_task(row: &Row<'_>) -> rusqlite::Result<InvestigationTask> {
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

fn row_to_evidence(row: &Row<'_>) -> rusqlite::Result<Evidence> {
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

fn query_evidence_for_run(
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

fn evidence_signature(evidence: &[Evidence]) -> Option<String> {
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

fn row_to_run(row: &Row<'_>) -> rusqlite::Result<InvestigationRun> {
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

fn row_to_step(row: &Row<'_>) -> rusqlite::Result<InvestigationStep> {
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

fn row_to_finding(row: &Row<'_>) -> rusqlite::Result<Finding> {
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

fn row_to_brief(row: &Row<'_>) -> rusqlite::Result<DecisionBrief> {
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

fn row_to_plan(row: &Row<'_>) -> rusqlite::Result<InvestigationPlan> {
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

fn row_to_artifact(row: &Row<'_>) -> rusqlite::Result<InvestigationArtifact> {
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

fn decode_json<T: serde::de::DeserializeOwned>(raw: String, column: usize) -> rusqlite::Result<T> {
    serde_json::from_str(&raw).map_err(|error| decode_error(column, error))
}

fn parse_enum<T>(raw: String, column: usize, parse: fn(&str) -> Option<T>) -> rusqlite::Result<T> {
    parse(&raw).ok_or_else(|| decode_error(column, format!("unknown value {raw:?}")))
}

fn content_hash(content: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(content).expect("serde_json::Value is serializable");
    let digest = Sha256::digest(bytes);
    format!("{digest:x}")
}
