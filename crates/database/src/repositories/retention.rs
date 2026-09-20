//! Host-owned retention for one terminal investigation task.
//!
//! Retention is deliberately a two-step operation.  A preview only reports
//! unreferenced evidence and superseded phase artifacts; a prune request must
//! name the exact preview items and is re-checked inside one SQLite transaction.
//! Evidence referenced by any finding, brief, plan, step or artifact is never a
//! candidate.  Remote filesystem backup bytes are not handled here: their
//! lifecycle still goes through the guarded `filesystem.backup.cleanup` path.

use std::collections::HashSet;

use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::Value;

use crate::{Database, DatabaseError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvestigationRetentionKind {
    Evidence,
    Artifact,
}

impl InvestigationRetentionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Evidence => "evidence",
            Self::Artifact => "artifact",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvestigationRetentionItem {
    pub id: String,
    pub task_id: String,
    pub kind: InvestigationRetentionKind,
    pub created_at: String,
    pub bytes: u64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvestigationRetentionSkip {
    pub id: String,
    pub kind: InvestigationRetentionKind,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvestigationRetentionPreview {
    pub task_id: String,
    pub cutoff_at: String,
    pub candidates: Vec<InvestigationRetentionItem>,
    pub protected_count: u32,
    pub candidate_bytes: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvestigationRetentionPruneResult {
    pub task_id: String,
    pub cutoff_at: String,
    pub deleted: Vec<InvestigationRetentionItem>,
    pub skipped: Vec<InvestigationRetentionSkip>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvestigationRetentionRequestItem<'a> {
    pub id: &'a str,
    pub kind: InvestigationRetentionKind,
}

pub struct InvestigationRetentionRepository<'a> {
    db: &'a Database,
}

impl<'a> InvestigationRetentionRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Build a bounded preview for one task.  Only terminal tasks participate;
    /// active or waiting tasks are intentionally invisible to cleanup.
    pub fn preview(
        &self,
        task_id: &str,
        cutoff_at: &str,
        limit: usize,
    ) -> Result<InvestigationRetentionPreview> {
        validate_cutoff(cutoff_at)?;
        validate_limit(limit)?;
        self.db.with(|connection| {
            ensure_terminal_task(connection, task_id)?;
            let protected = collect_protected_evidence(connection, task_id)?;
            let protected_count =
                count_protected_old_evidence(connection, task_id, cutoff_at, &protected)?;
            let total_candidate_bytes =
                candidate_bytes(connection, task_id, cutoff_at, &protected)?;
            let candidates = query_candidates(connection, task_id, cutoff_at, &protected, limit)?;
            let total_candidates = count_candidates(connection, task_id, cutoff_at, &protected)?;
            Ok(InvestigationRetentionPreview {
                task_id: task_id.to_string(),
                cutoff_at: cutoff_at.to_string(),
                truncated: total_candidates > candidates.len(),
                candidates,
                protected_count,
                candidate_bytes: total_candidate_bytes,
            })
        })
    }

    /// Prune exactly the items named by a prior preview.  Every item is
    /// re-validated while holding the SQLite transaction, so a new finding or
    /// plan reference wins over a stale UI preview.
    pub fn prune(
        &self,
        task_id: &str,
        cutoff_at: &str,
        items: &[InvestigationRetentionRequestItem<'_>],
        audit_id: &str,
        now: &str,
    ) -> Result<InvestigationRetentionPruneResult> {
        validate_cutoff(cutoff_at)?;
        if items.is_empty() || items.len() > 128 {
            return Err(DatabaseError::Validation(
                "retention prune requires between 1 and 128 items".into(),
            ));
        }
        validate_text(audit_id, "retention audit id", 256)?;
        validate_text(now, "retention timestamp", 80)?;
        self.db.with(|connection| {
            let tx = connection.unchecked_transaction()?;
            ensure_terminal_task_tx(&tx, task_id)?;
            let protected = collect_protected_evidence(&tx, task_id)?;
            let mut deleted = Vec::new();
            let mut skipped = Vec::new();
            let mut seen = HashSet::new();
            for item in items {
                validate_text(item.id, "retention item id", 256)?;
                if !seen.insert((item.kind.as_str(), item.id)) {
                    return Err(DatabaseError::Validation(
                        "retention prune items must be unique".into(),
                    ));
                }
                match item.kind {
                    InvestigationRetentionKind::Evidence => {
                        let row = tx
                            .query_row(
                                "SELECT id, task_id, collected_at,
                                        length(CAST(content AS BLOB))
                                 FROM investigation_evidence
                                 WHERE id = ?1 AND task_id = ?2",
                                params![item.id, task_id],
                                |row| {
                                    Ok((
                                        row.get::<_, String>(0)?,
                                        row.get::<_, String>(1)?,
                                        row.get::<_, String>(2)?,
                                        row.get::<_, i64>(3)?,
                                    ))
                                },
                            )
                            .optional()?;
                        let Some((id, owner, created_at, bytes)) = row else {
                            skipped.push(InvestigationRetentionSkip {
                                id: item.id.to_string(),
                                kind: item.kind,
                                reason: "not_found".into(),
                            });
                            continue;
                        };
                        if created_at.as_str() >= cutoff_at {
                            skipped.push(InvestigationRetentionSkip {
                                id,
                                kind: item.kind,
                                reason: "newer_than_cutoff".into(),
                            });
                            continue;
                        }
                        if protected.contains(item.id) {
                            skipped.push(InvestigationRetentionSkip {
                                id,
                                kind: item.kind,
                                reason: "referenced_by_task_history".into(),
                            });
                            continue;
                        }
                        let changed = tx.execute(
                            "DELETE FROM investigation_evidence
                             WHERE id = ?1 AND task_id = ?2 AND collected_at < ?3",
                            params![item.id, task_id, cutoff_at],
                        )?;
                        if changed == 1 {
                            deleted.push(InvestigationRetentionItem {
                                id,
                                task_id: owner,
                                kind: item.kind,
                                created_at,
                                bytes: nonnegative_bytes(bytes)?,
                                reason: "unreferenced_evidence".into(),
                            });
                        } else {
                            skipped.push(InvestigationRetentionSkip {
                                id,
                                kind: item.kind,
                                reason: "changed_during_prune".into(),
                            });
                        }
                    }
                    InvestigationRetentionKind::Artifact => {
                        let row = tx
                            .query_row(
                                "SELECT id, task_id, updated_at,
                                        length(CAST(content AS BLOB)), status
                                 FROM investigation_artifacts
                                 WHERE id = ?1 AND task_id = ?2",
                                params![item.id, task_id],
                                |row| {
                                    Ok((
                                        row.get::<_, String>(0)?,
                                        row.get::<_, String>(1)?,
                                        row.get::<_, String>(2)?,
                                        row.get::<_, i64>(3)?,
                                        row.get::<_, String>(4)?,
                                    ))
                                },
                            )
                            .optional()?;
                        let Some((id, owner, updated_at, bytes, status)) = row else {
                            skipped.push(InvestigationRetentionSkip {
                                id: item.id.to_string(),
                                kind: item.kind,
                                reason: "not_found".into(),
                            });
                            continue;
                        };
                        if status != "superseded" {
                            skipped.push(InvestigationRetentionSkip {
                                id,
                                kind: item.kind,
                                reason: "artifact_not_superseded".into(),
                            });
                            continue;
                        }
                        if updated_at.as_str() >= cutoff_at {
                            skipped.push(InvestigationRetentionSkip {
                                id,
                                kind: item.kind,
                                reason: "newer_than_cutoff".into(),
                            });
                            continue;
                        }
                        let changed = tx.execute(
                            "DELETE FROM investigation_artifacts
                             WHERE id = ?1 AND task_id = ?2 AND status = 'superseded'
                               AND updated_at < ?3",
                            params![item.id, task_id, cutoff_at],
                        )?;
                        if changed == 1 {
                            deleted.push(InvestigationRetentionItem {
                                id,
                                task_id: owner,
                                kind: item.kind,
                                created_at: updated_at,
                                bytes: nonnegative_bytes(bytes)?,
                                reason: "superseded_artifact".into(),
                            });
                        } else {
                            skipped.push(InvestigationRetentionSkip {
                                id,
                                kind: item.kind,
                                reason: "changed_during_prune".into(),
                            });
                        }
                    }
                }
            }
            let description = format!(
                "retention prune task={} cutoff={} deleted_evidence={} deleted_artifacts={} skipped={}",
                task_id,
                cutoff_at,
                deleted
                    .iter()
                    .filter(|item| item.kind == InvestigationRetentionKind::Evidence)
                    .count(),
                deleted
                    .iter()
                    .filter(|item| item.kind == InvestigationRetentionKind::Artifact)
                    .count(),
                skipped.len(),
            );
            tx.execute(
                "INSERT INTO activities (
                    id, server_id, workspace_id, type, title, description, source, actor,
                    reason, outcome, trace_id, created_at
                 )
                 SELECT ?1, server_id, workspace_id, 'agent_action',
                        '清理排查任务历史数据', ?2, 'user', 'user',
                        'explicit_retention_prune', 'success', ?1, ?3
                 FROM investigation_tasks WHERE id = ?4",
                params![audit_id, description, now, task_id],
            )?;
            tx.commit()?;
            Ok(InvestigationRetentionPruneResult {
                task_id: task_id.to_string(),
                cutoff_at: cutoff_at.to_string(),
                deleted,
                skipped,
            })
        })
    }
}

fn validate_cutoff(value: &str) -> Result<()> {
    if yukinal_time::parse_iso8601_utc(value).is_none() {
        return Err(DatabaseError::Validation(
            "retention cutoff must be a UTC ISO-8601 timestamp".into(),
        ));
    }
    Ok(())
}

fn validate_limit(limit: usize) -> Result<()> {
    if !(1..=128).contains(&limit) {
        return Err(DatabaseError::Validation(
            "retention preview limit must be between 1 and 128".into(),
        ));
    }
    Ok(())
}

fn validate_text(value: &str, label: &str, max_chars: usize) -> Result<()> {
    if value.trim().is_empty()
        || value.chars().count() > max_chars
        || value.chars().any(char::is_control)
    {
        return Err(DatabaseError::Validation(format!(
            "{label} is empty, too long or contains control characters"
        )));
    }
    Ok(())
}

fn ensure_terminal_task(connection: &rusqlite::Connection, task_id: &str) -> Result<()> {
    let status = connection
        .query_row(
            "SELECT status FROM investigation_tasks WHERE id = ?1",
            params![task_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    match status.as_deref() {
        Some("completed" | "failed" | "stopped" | "expired") => Ok(()),
        Some(_) => Err(DatabaseError::Validation(
            "retention is available only for terminal investigation tasks".into(),
        )),
        None => Err(DatabaseError::NotFound),
    }
}

fn ensure_terminal_task_tx(tx: &Transaction<'_>, task_id: &str) -> Result<()> {
    let status = tx
        .query_row(
            "SELECT status FROM investigation_tasks WHERE id = ?1",
            params![task_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    match status.as_deref() {
        Some("completed" | "failed" | "stopped" | "expired") => Ok(()),
        Some(_) => Err(DatabaseError::Validation(
            "retention is available only for terminal investigation tasks".into(),
        )),
        None => Err(DatabaseError::NotFound),
    }
}

fn count_protected_old_evidence(
    connection: &rusqlite::Connection,
    task_id: &str,
    cutoff_at: &str,
    protected: &HashSet<String>,
) -> Result<u32> {
    let mut statement = connection.prepare(
        "SELECT id FROM investigation_evidence
         WHERE task_id = ?1 AND collected_at < ?2",
    )?;
    let rows = statement.query_map(params![task_id, cutoff_at], |row| row.get::<_, String>(0))?;
    let mut count = 0_u32;
    for row in rows {
        if protected.contains(&row?) {
            count = count.saturating_add(1);
        }
    }
    Ok(count)
}

fn candidate_bytes(
    connection: &rusqlite::Connection,
    task_id: &str,
    cutoff_at: &str,
    protected: &HashSet<String>,
) -> Result<u64> {
    let mut total = 0_u64;
    let mut evidence = connection.prepare(
        "SELECT id, length(CAST(content AS BLOB))
         FROM investigation_evidence
         WHERE task_id = ?1 AND collected_at < ?2",
    )?;
    let evidence_rows = evidence.query_map(params![task_id, cutoff_at], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in evidence_rows {
        let (id, bytes) = row?;
        if !protected.contains(&id) {
            total = total.saturating_add(nonnegative_bytes(bytes)?);
        }
    }
    let mut artifacts = connection.prepare(
        "SELECT length(CAST(content AS BLOB))
         FROM investigation_artifacts
         WHERE task_id = ?1 AND status = 'superseded' AND updated_at < ?2",
    )?;
    let artifact_rows =
        artifacts.query_map(params![task_id, cutoff_at], |row| row.get::<_, i64>(0))?;
    for row in artifact_rows {
        total = total.saturating_add(nonnegative_bytes(row?)?);
    }
    Ok(total)
}

fn count_candidates(
    connection: &rusqlite::Connection,
    task_id: &str,
    cutoff_at: &str,
    protected: &HashSet<String>,
) -> Result<usize> {
    let mut count = 0_usize;
    let mut evidence = connection.prepare(
        "SELECT id FROM investigation_evidence
         WHERE task_id = ?1 AND collected_at < ?2",
    )?;
    let evidence_rows =
        evidence.query_map(params![task_id, cutoff_at], |row| row.get::<_, String>(0))?;
    for row in evidence_rows {
        if !protected.contains(&row?) {
            count = count.saturating_add(1);
        }
    }
    let artifact_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM investigation_artifacts
         WHERE task_id = ?1 AND status = 'superseded' AND updated_at < ?2",
        params![task_id, cutoff_at],
        |row| row.get(0),
    )?;
    count = count.saturating_add(usize::try_from(artifact_count).unwrap_or(usize::MAX));
    Ok(count)
}

fn query_candidates(
    connection: &rusqlite::Connection,
    task_id: &str,
    cutoff_at: &str,
    protected: &HashSet<String>,
    limit: usize,
) -> Result<Vec<InvestigationRetentionItem>> {
    let mut candidates = Vec::new();
    let mut evidence = connection.prepare(
        "SELECT id, task_id, collected_at, length(CAST(content AS BLOB))
         FROM investigation_evidence
         WHERE task_id = ?1 AND collected_at < ?2",
    )?;
    let evidence_rows = evidence.query_map(params![task_id, cutoff_at], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    for row in evidence_rows {
        let (id, owner, created_at, bytes) = row?;
        if !protected.contains(&id) {
            candidates.push(InvestigationRetentionItem {
                id,
                task_id: owner,
                kind: InvestigationRetentionKind::Evidence,
                created_at,
                bytes: nonnegative_bytes(bytes)?,
                reason: "unreferenced_evidence".into(),
            });
        }
    }
    let mut artifacts = connection.prepare(
        "SELECT id, task_id, updated_at, length(CAST(content AS BLOB))
         FROM investigation_artifacts
         WHERE task_id = ?1 AND status = 'superseded' AND updated_at < ?2",
    )?;
    let artifact_rows = artifacts.query_map(params![task_id, cutoff_at], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    for row in artifact_rows {
        let (id, owner, updated_at, bytes) = row?;
        candidates.push(InvestigationRetentionItem {
            id,
            task_id: owner,
            kind: InvestigationRetentionKind::Artifact,
            created_at: updated_at,
            bytes: nonnegative_bytes(bytes)?,
            reason: "superseded_artifact".into(),
        });
    }
    candidates.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.kind.as_str().cmp(right.kind.as_str()))
            .then_with(|| left.id.cmp(&right.id))
    });
    candidates.truncate(limit);
    Ok(candidates)
}

fn collect_protected_evidence(
    connection: &rusqlite::Connection,
    task_id: &str,
) -> Result<HashSet<String>> {
    let mut protected = HashSet::new();
    collect_json_arrays(
        connection,
        "SELECT evidence_ids FROM investigation_findings WHERE task_id = ?1",
        task_id,
        &mut protected,
    )?;
    collect_json_arrays(
        connection,
        "SELECT evidence_ids FROM investigation_steps WHERE task_id = ?1",
        task_id,
        &mut protected,
    )?;
    collect_json_arrays(
        connection,
        "SELECT evidence_ids FROM investigation_artifacts WHERE task_id = ?1",
        task_id,
        &mut protected,
    )?;
    let mut briefs = connection
        .prepare("SELECT options FROM investigation_decision_briefs WHERE task_id = ?1")?;
    let brief_rows = briefs.query_map(params![task_id], |row| row.get::<_, String>(0))?;
    for raw in brief_rows {
        let raw = raw?;
        let Ok(Value::Array(options)) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        for option in options {
            if let Some(ids) = option.get("evidenceIds") {
                collect_json_value_array(ids, &mut protected);
            }
        }
    }
    let mut plans =
        connection.prepare("SELECT steps FROM investigation_plans WHERE task_id = ?1")?;
    let plan_rows = plans.query_map(params![task_id], |row| row.get::<_, String>(0))?;
    for raw in plan_rows {
        let raw = raw?;
        let Ok(Value::Array(steps)) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        for step in steps {
            if let Some(ids) = step.get("evidenceIds") {
                collect_json_value_array(ids, &mut protected);
            }
        }
    }
    Ok(protected)
}

fn collect_json_arrays(
    connection: &rusqlite::Connection,
    sql: &str,
    task_id: &str,
    protected: &mut HashSet<String>,
) -> Result<()> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(params![task_id], |row| row.get::<_, String>(0))?;
    for raw in rows {
        let raw = raw?;
        if let Ok(value) = serde_json::from_str::<Value>(&raw) {
            collect_json_value_array(&value, protected);
        }
    }
    Ok(())
}

fn collect_json_value_array(value: &Value, protected: &mut HashSet<String>) {
    if let Value::Array(values) = value {
        for value in values {
            if let Some(id) = value.as_str() {
                protected.insert(id.to_string());
            }
        }
    }
}

fn nonnegative_bytes(value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| DatabaseError::Decode("negative content length".into()))
}
