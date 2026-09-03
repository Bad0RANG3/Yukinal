//! `snapshots` (collector output per server).

use rusqlite::{params, OptionalExtension, Row};

use crate::models::ServerSnapshot;
use crate::{Database, DatabaseError, Result};

pub struct SnapshotsRepository<'a> {
    db: &'a Database,
}

impl<'a> SnapshotsRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn insert(&self, snapshot: &ServerSnapshot) -> Result<()> {
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO snapshots (id, server_id, collected_at, health, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    snapshot.id,
                    snapshot.server_id,
                    snapshot.collected_at,
                    snapshot.health.as_str(),
                    serde_json::to_string(snapshot)?,
                ],
            )?;
            Ok(())
        })
    }

    /// Newest snapshot for a server, if any. The full payload is the single source
    /// of truth; `health`/`collected_at` columns exist only for indexing.
    pub fn latest(&self, server_id: &str) -> Result<Option<ServerSnapshot>> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT payload FROM snapshots WHERE server_id = ?1
                     ORDER BY collected_at DESC LIMIT 1",
                    params![server_id],
                    row_to_snapshot,
                )
                .optional()
                .map_err(DatabaseError::from)
        })
    }

    /// Recent snapshots for the attention/trend computation, oldest-last.
    pub fn list_recent(&self, server_id: &str, limit: usize) -> Result<Vec<ServerSnapshot>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT payload FROM snapshots WHERE server_id = ?1
                 ORDER BY collected_at DESC LIMIT ?2",
            )?;
            let rows = statement.query_map(params![server_id, limit as i64], row_to_snapshot)?;
            let mut out: Vec<ServerSnapshot> = rows
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)?;
            out.reverse();
            Ok(out)
        })
    }
}

fn row_to_snapshot(row: &Row<'_>) -> rusqlite::Result<ServerSnapshot> {
    serde_json::from_str(&row.get::<_, String>(0)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            error.to_string().into(),
        )
    })
}
