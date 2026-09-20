//! Host-owned idempotency records for sidecar tool requests.
//!
//! The row is an intent/terminal-state ledger, not an audit copy of the tool
//! output. Keeping the response out of SQLite avoids turning the safety guard
//! into a second raw-output store. The desktop host may replay a bounded,
//! in-memory response while it is alive; a later process fails closed.

use rusqlite::{params, OptionalExtension};

use crate::{Database, DatabaseError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostToolCallStatus {
    Running,
    Success,
    Failed,
    Cancelled,
    Uncertain,
}

impl HostToolCallStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Success => "success",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Uncertain => "uncertain",
        }
    }

    fn from_db(raw: &str) -> Option<Self> {
        match raw {
            "running" => Some(Self::Running),
            "success" => Some(Self::Success),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            "uncertain" => Some(Self::Uncertain),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostToolCallClaim {
    /// This `(trace_id, call_id)` was inserted by this caller and may execute.
    Claimed,
    /// The key already exists. The caller must compare the fingerprint before
    /// considering a replay; a same key with different input is a hard refusal.
    Existing {
        request_fingerprint: String,
        status: HostToolCallStatus,
    },
}

pub struct HostToolCallInput<'a> {
    pub trace_id: &'a str,
    pub call_id: &'a str,
    pub task_id: Option<&'a str>,
    pub plan_id: Option<&'a str>,
    pub plan_step_id: Option<&'a str>,
    pub tool_name: &'a str,
    pub request_fingerprint: &'a str,
    pub action_fingerprint: Option<&'a str>,
    pub started_at: &'a str,
}

pub struct HostToolCallsRepository<'a> {
    db: &'a Database,
}

impl<'a> HostToolCallsRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Atomically reserve a call key from the database connection's serialized
    /// critical section. The unique primary key handles concurrent event tasks;
    /// no check-then-insert race is left for the host layer to solve.
    pub fn claim(&self, input: HostToolCallInput<'_>) -> Result<HostToolCallClaim> {
        self.db.with(|connection| {
            // A resumed durable plan may produce a new provider call ID for
            // the same logical action. Keep failed rows retryable, but block
            // an action that is still running, already succeeded, or whose
            // remote effect is uncertain.
            if let (Some(task_id), Some(plan_id), Some(plan_step_id), Some(action_fingerprint)) = (
                input.task_id,
                input.plan_id,
                input.plan_step_id,
                input.action_fingerprint,
            ) {
                let existing: Option<(String, String)> = connection
                    .query_row(
                        "SELECT request_fingerprint, status
                         FROM host_tool_calls
                         WHERE task_id = ?1 AND plan_id = ?2 AND plan_step_id = ?3
                           AND action_fingerprint = ?4
                           AND status IN ('running', 'success', 'uncertain')
                         ORDER BY started_at DESC LIMIT 1",
                        params![task_id, plan_id, plan_step_id, action_fingerprint],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                if let Some((fingerprint, raw_status)) = existing {
                    let status = HostToolCallStatus::from_db(&raw_status).ok_or_else(|| {
                        DatabaseError::Decode(format!(
                            "unknown host tool call status `{raw_status}`"
                        ))
                    })?;
                    return Ok(HostToolCallClaim::Existing {
                        request_fingerprint: fingerprint,
                        status,
                    });
                }
            }
            let inserted = connection.execute(
                "INSERT OR IGNORE INTO host_tool_calls (
                    trace_id, call_id, task_id, plan_id, plan_step_id, tool_name,
                    request_fingerprint, action_fingerprint, status, started_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'running', ?9)",
                params![
                    input.trace_id,
                    input.call_id,
                    input.task_id,
                    input.plan_id,
                    input.plan_step_id,
                    input.tool_name,
                    input.request_fingerprint,
                    input.action_fingerprint,
                    input.started_at,
                ],
            )?;
            if inserted == 1 {
                return Ok(HostToolCallClaim::Claimed);
            }

            let (fingerprint, raw_status): (String, String) = connection.query_row(
                "SELECT request_fingerprint, status
                 FROM host_tool_calls WHERE trace_id = ?1 AND call_id = ?2",
                params![input.trace_id, input.call_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let status = HostToolCallStatus::from_db(&raw_status).ok_or_else(|| {
                DatabaseError::Decode(format!("unknown host tool call status `{raw_status}`"))
            })?;
            Ok(HostToolCallClaim::Existing {
                request_fingerprint: fingerprint,
                status,
            })
        })
    }

    /// Close a previously claimed call. A row is never reopened: if an action
    /// was observed as uncertain, a later retry must reconcile it rather than
    /// silently turn it back into an executable request.
    pub fn finish(
        &self,
        trace_id: &str,
        call_id: &str,
        status: HostToolCallStatus,
        ended_at: &str,
    ) -> Result<()> {
        self.db.with(|connection| {
            let changed = connection.execute(
                "UPDATE host_tool_calls
                 SET status = ?3, ended_at = ?4
                 WHERE trace_id = ?1 AND call_id = ?2 AND status = 'running'",
                params![trace_id, call_id, status.as_str(), ended_at],
            )?;
            if changed != 1 {
                return Err(DatabaseError::Validation(format!(
                    "host tool call `{trace_id}/{call_id}` was not running"
                )));
            }
            Ok(())
        })
    }
}
