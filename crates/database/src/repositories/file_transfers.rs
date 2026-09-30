//! Durable, metadata-only history for host-owned file transfers.
//!
//! The JSON snapshot is the public progress contract. `staging_json` is private
//! recovery inventory and must never be sent across IPC. It may contain local
//! staging paths, so all access stays behind this repository and the Rust host.

use rusqlite::{params, OptionalExtension, Row};
use serde_json::Value;

use crate::{Database, DatabaseError, Result};

pub const MAX_FILE_TRANSFER_PAGE: usize = 100;
const MAX_RETAINED_FILE_TRANSFERS: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTransferRecord {
    pub transfer_id: String,
    pub server_id: String,
    pub direction: String,
    pub status: String,
    pub started_at_epoch_ms: u64,
    pub updated_at_epoch_ms: u64,
    pub snapshot_json: String,
    pub staging_json: String,
}

pub struct FileTransfersRepository<'a> {
    db: &'a Database,
}

impl<'a> FileTransfersRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Insert the host's first snapshot. A duplicate identifier is an error rather
    /// than an upsert so transfer IDs cannot silently claim an earlier task.
    pub fn insert(&self, record: &FileTransferRecord) -> Result<()> {
        validate_record(record)?;
        self.db.with(|connection| {
            let tx = connection.unchecked_transaction()?;
            tx.execute(
                "INSERT INTO file_transfers (
                    transfer_id, server_id, direction, status, started_at_epoch_ms,
                    updated_at_epoch_ms, snapshot_json, staging_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    record.transfer_id,
                    record.server_id,
                    record.direction,
                    record.status,
                    checked_i64(record.started_at_epoch_ms)?,
                    checked_i64(record.updated_at_epoch_ms)?,
                    record.snapshot_json,
                    record.staging_json,
                ],
            )?;
            prune_terminal_history(&tx)?;
            tx.commit()?;
            Ok(())
        })
    }

    /// Persist one latest snapshot and the host-only staging ownership inventory.
    pub fn update(&self, record: &FileTransferRecord) -> Result<()> {
        validate_record(record)?;
        self.db.with(|connection| {
            let changed = connection.execute(
                "UPDATE file_transfers SET
                    server_id = ?2, direction = ?3, status = ?4,
                    started_at_epoch_ms = ?5, updated_at_epoch_ms = ?6,
                    snapshot_json = ?7, staging_json = ?8
                 WHERE transfer_id = ?1",
                params![
                    record.transfer_id,
                    record.server_id,
                    record.direction,
                    record.status,
                    checked_i64(record.started_at_epoch_ms)?,
                    checked_i64(record.updated_at_epoch_ms)?,
                    record.snapshot_json,
                    record.staging_json,
                ],
            )?;
            if changed == 0 {
                return Err(DatabaseError::NotFound);
            }
            prune_terminal_history(connection)?;
            Ok(())
        })
    }

    pub fn get(&self, transfer_id: &str) -> Result<Option<FileTransferRecord>> {
        if transfer_id.trim().is_empty() {
            return Err(DatabaseError::Validation(
                "file transfer id is required".into(),
            ));
        }
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT transfer_id, server_id, direction, status,
                            started_at_epoch_ms, updated_at_epoch_ms,
                            snapshot_json, staging_json
                     FROM file_transfers WHERE transfer_id = ?1",
                    [transfer_id],
                    row_to_record,
                )
                .optional()
                .map_err(DatabaseError::from)
        })
    }

    /// Fetch one newest-first page. The caller may request fewer rows, but can never
    /// turn the history endpoint into an unbounded table scan or response.
    pub fn list(
        &self,
        server_id: Option<&str>,
        before_updated_at_epoch_ms: Option<u64>,
        limit: usize,
    ) -> Result<(Vec<FileTransferRecord>, bool)> {
        if limit == 0 || limit > MAX_FILE_TRANSFER_PAGE {
            return Err(DatabaseError::Validation(format!(
                "file transfer history limit must be between 1 and {MAX_FILE_TRANSFER_PAGE}"
            )));
        }
        if server_id.is_some_and(|value| value.trim().is_empty()) {
            return Err(DatabaseError::Validation(
                "file transfer server id cannot be empty".into(),
            ));
        }
        let before = before_updated_at_epoch_ms.map(checked_i64).transpose()?;
        let requested = i64::try_from(limit + 1)
            .map_err(|_| DatabaseError::Validation("file transfer limit is too large".into()))?;
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT transfer_id, server_id, direction, status,
                        started_at_epoch_ms, updated_at_epoch_ms,
                        snapshot_json, staging_json
                 FROM file_transfers
                 WHERE (?1 IS NULL OR server_id = ?1)
                   AND (?2 IS NULL OR updated_at_epoch_ms < ?2)
                 ORDER BY updated_at_epoch_ms DESC, transfer_id DESC
                 LIMIT ?3",
            )?;
            let rows = statement.query_map(params![server_id, before, requested], row_to_record)?;
            let mut records = rows.collect::<std::result::Result<Vec<_>, _>>()?;
            let truncated = records.len() > limit;
            if truncated {
                records.truncate(limit);
            }
            Ok((records, truncated))
        })
    }

    /// Mark every persisted nonterminal task interrupted before any history is
    /// returned after process startup. A previous `running` snapshot is never
    /// replayed as a success, even if a final UI event was lost during a crash.
    /// The snapshot rewrite and indexed status update happen in one transaction.
    pub fn interrupt_nonterminal(&self, now_epoch_ms: u64) -> Result<Vec<FileTransferRecord>> {
        let now = checked_i64(now_epoch_ms)?;
        self.db.with(|connection| {
            let tx = connection.unchecked_transaction()?;
            let mut statement = tx.prepare(
                "SELECT transfer_id, server_id, direction, status,
                        started_at_epoch_ms, updated_at_epoch_ms,
                        snapshot_json, staging_json
                 FROM file_transfers
                 WHERE status IN ('queued','running','waitingConflict')
                    OR staging_json != '[]'
                 ORDER BY updated_at_epoch_ms DESC, transfer_id DESC",
            )?;
            let records = statement
                .query_map([], row_to_record)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            drop(statement);

            let mut interrupted = Vec::with_capacity(records.len());
            for mut record in records {
                let nonterminal = matches!(
                    record.status.as_str(),
                    "queued" | "running" | "waitingConflict"
                );
                if !nonterminal && record.staging_json == "[]" {
                    continue;
                }
                let mut snapshot: Value = serde_json::from_str(&record.snapshot_json)
                    .map_err(|error| DatabaseError::Decode(error.to_string()))?;
                if nonterminal {
                    let object = snapshot.as_object_mut().ok_or_else(|| {
                        DatabaseError::Decode("file transfer snapshot must be a JSON object".into())
                    })?;
                    object.insert("status".into(), Value::String("interrupted".into()));
                    object.insert("updatedAtEpochMs".into(), Value::from(now_epoch_ms));
                    object.insert("activeConflict".into(), Value::Null);
                    object.insert("currentItem".into(), Value::Null);
                    object.insert("currentItemBytes".into(), Value::from(0));
                    object.insert("currentItemTotalBytes".into(), Value::Null);
                    record.status = "interrupted".into();
                    record.updated_at_epoch_ms = now_epoch_ms;
                    record.snapshot_json = serde_json::to_string(&snapshot)
                        .map_err(|error| DatabaseError::Decode(error.to_string()))?;
                    tx.execute(
                        "UPDATE file_transfers SET status = 'interrupted',
                            updated_at_epoch_ms = ?2, snapshot_json = ?3
                         WHERE transfer_id = ?1
                           AND status IN ('queued','running','waitingConflict')",
                        params![record.transfer_id, now, record.snapshot_json],
                    )?;
                }
                interrupted.push(record);
            }
            tx.commit()?;
            Ok(interrupted)
        })
    }

    pub fn staging_json(&self, transfer_id: &str) -> Result<Option<String>> {
        if transfer_id.trim().is_empty() {
            return Err(DatabaseError::Validation(
                "file transfer id is required".into(),
            ));
        }
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT staging_json FROM file_transfers WHERE transfer_id = ?1",
                    [transfer_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(DatabaseError::from)
        })
    }
}

fn validate_record(record: &FileTransferRecord) -> Result<()> {
    if record.transfer_id.trim().is_empty()
        || record.server_id.trim().is_empty()
        || !matches!(record.direction.as_str(), "upload" | "download")
        || !matches!(
            record.status.as_str(),
            "queued"
                | "running"
                | "waitingConflict"
                | "completed"
                | "partial"
                | "failed"
                | "cancelled"
                | "interrupted"
        )
    {
        return Err(DatabaseError::Validation(
            "file transfer record has invalid identifiers, direction, or status".into(),
        ));
    }
    if record.snapshot_json.len() > 524_288 || record.staging_json.len() > 65_536 {
        return Err(DatabaseError::Validation(
            "file transfer metadata exceeds its storage limit".into(),
        ));
    }
    let snapshot: Value = serde_json::from_str(&record.snapshot_json)
        .map_err(|error| DatabaseError::Decode(error.to_string()))?;
    let staging: Value = serde_json::from_str(&record.staging_json)
        .map_err(|error| DatabaseError::Decode(error.to_string()))?;
    if !snapshot.is_object() || !staging.is_array() {
        return Err(DatabaseError::Validation(
            "file transfer snapshot must be an object and staging metadata an array".into(),
        ));
    }
    Ok(())
}

fn prune_terminal_history(connection: &rusqlite::Connection) -> Result<()> {
    let max = i64::try_from(MAX_RETAINED_FILE_TRANSFERS)
        .map_err(|_| DatabaseError::Validation("retention limit is invalid".into()))?;
    connection.execute(
        "DELETE FROM file_transfers
         WHERE status IN ('completed','partial','failed','cancelled','interrupted')
           AND transfer_id NOT IN (
               SELECT transfer_id FROM file_transfers
               WHERE status IN ('completed','partial','failed','cancelled','interrupted')
               ORDER BY updated_at_epoch_ms DESC, transfer_id DESC
               LIMIT ?1
           )",
        [max],
    )?;
    Ok(())
}

fn checked_i64(value: u64) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| DatabaseError::Validation("file transfer timestamp is too large".into()))
}

fn row_to_record(row: &Row<'_>) -> rusqlite::Result<FileTransferRecord> {
    let started: i64 = row.get(4)?;
    let updated: i64 = row.get(5)?;
    Ok(FileTransferRecord {
        transfer_id: row.get(0)?,
        server_id: row.get(1)?,
        direction: row.get(2)?,
        status: row.get(3)?,
        started_at_epoch_ms: started.max(0) as u64,
        updated_at_epoch_ms: updated.max(0) as u64,
        snapshot_json: row.get(6)?,
        staging_json: row.get(7)?,
    })
}
