//! `activities` — the audit stream (who/what/where/when/why/result).

use rusqlite::{params, Row};

use crate::models::{Activity, ActivityOutcome, ActivitySource, ActivityType};
use crate::{Database, Result};

pub struct ActivitiesRepository<'a> {
    db: &'a Database,
}

impl<'a> ActivitiesRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn insert(&self, activity: &Activity) -> Result<()> {
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO activities (
                    id, server_id, workspace_id, type, title, description, source, actor,
                    reason, outcome, trace_id, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    activity.id,
                    activity.server_id,
                    activity.workspace_id,
                    activity.r#type.as_str(),
                    activity.title,
                    activity.description,
                    activity.source.as_str(),
                    activity.actor,
                    activity.reason,
                    activity.outcome.map(|outcome| outcome.as_str()),
                    activity.trace_id,
                    activity.created_at,
                ],
            )?;
            Ok(())
        })
    }

    /// Newest-first activity feed.
    pub fn list_recent(&self, limit: usize) -> Result<Vec<Activity>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, server_id, workspace_id, type, title, description, source, actor,
                        reason, outcome, trace_id, created_at
                 FROM activities ORDER BY created_at DESC LIMIT ?1",
            )?;
            let rows = statement.query_map(params![limit as i64], row_to_activity)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(crate::DatabaseError::from)
        })
    }
}

fn row_to_activity(row: &Row<'_>) -> rusqlite::Result<Activity> {
    let r#type = ActivityType::from_db(&row.get::<_, String>(3)?)
        .ok_or_else(|| decode(3, "unknown activity type"))?;
    let source = ActivitySource::from_db(&row.get::<_, String>(6)?)
        .ok_or_else(|| decode(6, "unknown activity source"))?;
    let outcome = row
        .get::<_, Option<String>>(9)?
        .map(|raw| ActivityOutcome::from_db(&raw).ok_or_else(|| decode(9, "unknown outcome")))
        .transpose()?;

    Ok(Activity {
        id: row.get(0)?,
        server_id: row.get(1)?,
        workspace_id: row.get(2)?,
        r#type,
        title: row.get(4)?,
        description: row.get(5)?,
        source,
        actor: row.get(7)?,
        reason: row.get(8)?,
        outcome,
        trace_id: row.get(10)?,
        created_at: row.get(11)?,
    })
}

fn decode(index: usize, error: impl std::fmt::Display) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        error.to_string().into(),
    )
}
