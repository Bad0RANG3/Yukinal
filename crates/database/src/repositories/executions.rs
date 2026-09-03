//! `tool_executions` — the audit row of every tool call (who decided, what risk).

use rusqlite::{params, Row};

use crate::models::{
    Environment, PermissionMode, RiskLevel, ToolExecutionRecord, ToolExecutionStatus,
};
use crate::{Database, DatabaseError, Result};

pub struct ToolExecutionsRepository<'a> {
    db: &'a Database,
}

impl<'a> ToolExecutionsRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn insert(&self, record: &ToolExecutionRecord) -> Result<()> {
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO tool_executions (
                    trace_id, step_id, call_id, tool_name, server_id, environment, risk_level,
                    decision, approved_by, status, input, output, error, started_at, ended_at, duration_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    record.trace_id,
                    record.step_id,
                    record.call_id,
                    record.tool_name,
                    record.server_id,
                    record.environment.as_str(),
                    record.risk_level.as_str(),
                    record.decision.as_str(),
                    record.approved_by,
                    record.status.as_str(),
                    serde_json::to_string(&record.input)?,
                    record.output.as_ref().map(serde_json::to_string).transpose()?,
                    record.error,
                    record.started_at,
                    record.ended_at,
                    record.duration_ms.map(|ms| ms as i64),
                ],
            )?;
            Ok(())
        })
    }

    /// All steps of one trace, in start order (the AI panel's drill-down).
    pub fn list_for_trace(&self, trace_id: &str) -> Result<Vec<ToolExecutionRecord>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT trace_id, step_id, call_id, tool_name, server_id, environment, risk_level,
                        decision, approved_by, status, input, output, error, started_at, ended_at, duration_ms
                 FROM tool_executions WHERE trace_id = ?1 ORDER BY started_at",
            )?;
            let rows = statement.query_map(params![trace_id], row_to_record)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
        })
    }

    /// Newest executions across traces (activity/audit views).
    pub fn list_recent(&self, limit: usize) -> Result<Vec<ToolExecutionRecord>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT trace_id, step_id, call_id, tool_name, server_id, environment, risk_level,
                        decision, approved_by, status, input, output, error, started_at, ended_at, duration_ms
                 FROM tool_executions ORDER BY started_at DESC LIMIT ?1",
            )?;
            let rows = statement.query_map(params![limit as i64], row_to_record)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
        })
    }
}

fn row_to_record(row: &Row<'_>) -> rusqlite::Result<ToolExecutionRecord> {
    let environment = Environment::from_db(&row.get::<_, String>(5)?)
        .ok_or_else(|| decode(5, "unknown environment"))?;
    let risk_level = RiskLevel::from_db(&row.get::<_, String>(6)?)
        .ok_or_else(|| decode(6, "unknown risk level"))?;
    let decision = PermissionMode::from_db(&row.get::<_, String>(7)?)
        .ok_or_else(|| decode(7, "unknown decision"))?;
    let status = ToolExecutionStatus::from_db(&row.get::<_, String>(9)?)
        .ok_or_else(|| decode(9, "unknown status"))?;

    let input =
        serde_json::from_str(&row.get::<_, String>(10)?).map_err(|error| decode(10, error))?;
    let output =
        optional_value(row.get::<_, Option<String>>(11)?).map_err(|error| decode(11, error))?;

    Ok(ToolExecutionRecord {
        trace_id: row.get(0)?,
        step_id: row.get(1)?,
        call_id: row.get(2)?,
        tool_name: row.get(3)?,
        server_id: row.get(4)?,
        environment,
        risk_level,
        decision,
        approved_by: row.get(8)?,
        status,
        input,
        output,
        error: row.get(12)?,
        started_at: row.get(13)?,
        ended_at: row.get(14)?,
        duration_ms: row.get::<_, Option<i64>>(15)?.map(|ms| ms as u64),
    })
}

fn optional_value(raw: Option<String>) -> Result<Option<serde_json::Value>> {
    raw.map(|value| serde_json::from_str(&value))
        .transpose()
        .map_err(DatabaseError::from)
}

fn decode(index: usize, error: impl std::fmt::Display) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        error.to_string().into(),
    )
}
