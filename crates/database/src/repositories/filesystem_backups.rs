//! Durable ownership records for host-created remote filesystem backups.
//!
//! The backup bytes remain on the remote server.  This repository stores only
//! bounded metadata, so a restore can prove that its source was created by
//! this host for the same server and target path without copying sensitive
//! file contents into SQLite.

use rusqlite::{params, OptionalExtension};

use crate::{Database, DatabaseError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesystemBackupStatus {
    Available,
    Restored,
    Deleted,
}

impl FilesystemBackupStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Restored => "restored",
            Self::Deleted => "deleted",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "available" => Some(Self::Available),
            "restored" => Some(Self::Restored),
            "deleted" => Some(Self::Deleted),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesystemBackupRecord {
    pub id: String,
    pub server_id: String,
    pub task_id: Option<String>,
    pub plan_id: Option<String>,
    pub plan_step_id: Option<String>,
    pub trace_id: Option<String>,
    pub call_id: Option<String>,
    pub path: String,
    pub backup_path: String,
    pub revision: String,
    pub bytes_backed_up: i64,
    pub status: FilesystemBackupStatus,
    pub created_at: String,
    pub updated_at: String,
    pub restored_at: Option<String>,
    pub deleted_at: Option<String>,
}

pub struct FilesystemBackupsRepository<'a> {
    db: &'a Database,
}

impl<'a> FilesystemBackupsRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn insert(&self, record: &FilesystemBackupRecord) -> Result<()> {
        validate_record(record)?;
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO filesystem_backups (
                    id, server_id, task_id, plan_id, plan_step_id, trace_id, call_id,
                    path, backup_path, revision, bytes_backed_up, status, created_at,
                    updated_at, restored_at, deleted_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    record.id,
                    record.server_id,
                    record.task_id,
                    record.plan_id,
                    record.plan_step_id,
                    record.trace_id,
                    record.call_id,
                    record.path,
                    record.backup_path,
                    record.revision,
                    record.bytes_backed_up,
                    record.status.as_str(),
                    record.created_at,
                    record.updated_at,
                    record.restored_at,
                    record.deleted_at,
                ],
            )?;
            Ok(())
        })
    }

    /// Find an unconsumed backup for an exact server/source/target tuple.
    /// Returning only `available` rows makes a restored or explicitly deleted
    /// copy unusable even if its remote path still happens to exist.
    pub fn find_available(
        &self,
        server_id: &str,
        path: &str,
        backup_path: &str,
    ) -> Result<Option<FilesystemBackupRecord>> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, server_id, task_id, plan_id, plan_step_id, trace_id, call_id,
                            path, backup_path, revision, bytes_backed_up, status, created_at,
                            updated_at, restored_at, deleted_at
                     FROM filesystem_backups
                     WHERE server_id = ?1 AND path = ?2 AND backup_path = ?3
                       AND status = 'available'",
                    params![server_id, path, backup_path],
                    row_to_record,
                )
                .optional()
                .map_err(DatabaseError::from)
        })
    }

    /// List only the host-owned backup ledger rows for one exact task/server pair.
    ///
    /// This is deliberately a metadata-only operation.  It does not probe the remote
    /// filesystem, so callers must not treat an `available` row as proof that the sibling
    /// path still exists.  Fetch one extra row so the caller can tell that the bounded result
    /// was truncated without an unbounded count query.
    pub fn list_for_task(
        &self,
        server_id: &str,
        task_id: &str,
        status: Option<FilesystemBackupStatus>,
        path: Option<&str>,
        limit: usize,
    ) -> Result<(Vec<FilesystemBackupRecord>, bool)> {
        if server_id.trim().is_empty() || task_id.trim().is_empty() {
            return Err(DatabaseError::Validation(
                "filesystem backup listing requires a server and task".into(),
            ));
        }
        if limit == 0 || limit > 128 {
            return Err(DatabaseError::Validation(
                "filesystem backup listing limit must be between 1 and 128".into(),
            ));
        }
        if path.is_some_and(|value| value.trim().is_empty() || value.chars().any(char::is_control))
        {
            return Err(DatabaseError::Validation(
                "filesystem backup listing path is empty or contains control characters".into(),
            ));
        }

        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, server_id, task_id, plan_id, plan_step_id, trace_id, call_id,
                        path, backup_path, revision, bytes_backed_up, status, created_at,
                        updated_at, restored_at, deleted_at
                 FROM filesystem_backups
                 WHERE server_id = ?1 AND task_id = ?2
                   AND (?3 IS NULL OR status = ?3)
                   AND (?4 IS NULL OR path = ?4)
                 ORDER BY created_at DESC, id DESC
                 LIMIT ?5",
            )?;
            let requested = i64::try_from(limit + 1).map_err(|_| {
                DatabaseError::Validation("filesystem backup listing limit is too large".into())
            })?;
            let rows = statement.query_map(
                params![
                    server_id,
                    task_id,
                    status.map(FilesystemBackupStatus::as_str),
                    path,
                    requested,
                ],
                row_to_record,
            )?;
            let mut records = rows.collect::<std::result::Result<Vec<_>, _>>()?;
            let truncated = records.len() > limit;
            if truncated {
                records.truncate(limit);
            }
            Ok((records, truncated))
        })
    }

    /// Consume a backup after a successful guarded restore.  The conditional
    /// update prevents two independent restore requests from both treating the
    /// same recovery copy as available.
    pub fn mark_restored(
        &self,
        server_id: &str,
        path: &str,
        backup_path: &str,
        now: &str,
    ) -> Result<()> {
        self.db.with(|connection| {
            let changed = connection.execute(
                "UPDATE filesystem_backups
                    SET status = 'restored', restored_at = ?4, updated_at = ?4
                  WHERE server_id = ?1 AND path = ?2 AND backup_path = ?3
                    AND status = 'available'",
                params![server_id, path, backup_path, now],
            )?;
            if changed != 1 {
                return Err(DatabaseError::Validation(
                    "filesystem backup is no longer available".into(),
                ));
            }
            Ok(())
        })
    }

    pub fn mark_deleted(
        &self,
        server_id: &str,
        path: &str,
        backup_path: &str,
        now: &str,
    ) -> Result<()> {
        self.db.with(|connection| {
            let changed = connection.execute(
                "UPDATE filesystem_backups
                    SET status = 'deleted', deleted_at = ?4, updated_at = ?4
                  WHERE server_id = ?1 AND path = ?2 AND backup_path = ?3
                    AND status = 'available'",
                params![server_id, path, backup_path, now],
            )?;
            if changed != 1 {
                return Err(DatabaseError::Validation(
                    "filesystem backup is no longer available".into(),
                ));
            }
            Ok(())
        })
    }
}

fn validate_record(record: &FilesystemBackupRecord) -> Result<()> {
    for (label, value) in [
        ("backup id", record.id.as_str()),
        ("server id", record.server_id.as_str()),
        ("path", record.path.as_str()),
        ("backup path", record.backup_path.as_str()),
        ("revision", record.revision.as_str()),
        ("created at", record.created_at.as_str()),
        ("updated at", record.updated_at.as_str()),
    ] {
        if value.trim().is_empty() || value.chars().any(char::is_control) {
            return Err(DatabaseError::Validation(format!(
                "filesystem backup {label} is empty or contains control characters"
            )));
        }
    }
    if record.revision.len() != 64
        || record
            .revision
            .chars()
            .any(|character| !character.is_ascii_hexdigit())
    {
        return Err(DatabaseError::Validation(
            "filesystem backup revision must be a 64-character hex digest".into(),
        ));
    }
    if record.bytes_backed_up < 0 {
        return Err(DatabaseError::Validation(
            "filesystem backup byte count cannot be negative".into(),
        ));
    }
    Ok(())
}

fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<FilesystemBackupRecord> {
    let raw_status: String = row.get(11)?;
    let status = FilesystemBackupStatus::parse(&raw_status).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            11,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(format!(
                "unknown filesystem backup status `{raw_status}`"
            ))),
        )
    })?;
    Ok(FilesystemBackupRecord {
        id: row.get(0)?,
        server_id: row.get(1)?,
        task_id: row.get(2)?,
        plan_id: row.get(3)?,
        plan_step_id: row.get(4)?,
        trace_id: row.get(5)?,
        call_id: row.get(6)?,
        path: row.get(7)?,
        backup_path: row.get(8)?,
        revision: row.get(9)?,
        bytes_backed_up: row.get(10)?,
        status,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
        restored_at: row.get(14)?,
        deleted_at: row.get(15)?,
    })
}

#[cfg(test)]
mod tests {
    use super::{FilesystemBackupRecord, FilesystemBackupStatus};
    use crate::Database;

    fn record(status: FilesystemBackupStatus) -> FilesystemBackupRecord {
        FilesystemBackupRecord {
            id: "backup_1".into(),
            server_id: "srv_1".into(),
            task_id: Some("task_1".into()),
            plan_id: Some("plan_1".into()),
            plan_step_id: Some("step_1".into()),
            trace_id: Some("trace_1".into()),
            call_id: Some("call_1".into()),
            path: "/etc/app.env".into(),
            backup_path: "/etc/.yukinal-backup-0123456789abcdef-0123456789abcdef0123456789abcdef"
                .into(),
            revision: "a".repeat(64),
            bytes_backed_up: 12,
            status,
            created_at: "2026-09-20T00:00:00Z".into(),
            updated_at: "2026-09-20T00:00:00Z".into(),
            restored_at: None,
            deleted_at: None,
        }
    }

    #[test]
    fn restore_lookup_is_exact_and_consumed_once() {
        let db = Database::in_memory().expect("database");
        let repository = db.filesystem_backups();
        repository
            .insert(&record(FilesystemBackupStatus::Available))
            .expect("insert");
        assert!(repository
            .find_available(
                "srv_1",
                "/etc/app.env",
                &record(FilesystemBackupStatus::Available).backup_path
            )
            .expect("lookup")
            .is_some());
        assert!(repository
            .find_available(
                "srv_1",
                "/etc/other.env",
                &record(FilesystemBackupStatus::Available).backup_path
            )
            .expect("mismatched lookup")
            .is_none());
        repository
            .mark_restored(
                "srv_1",
                "/etc/app.env",
                &record(FilesystemBackupStatus::Available).backup_path,
                "2026-09-20T00:01:00Z",
            )
            .expect("consume");
        assert!(repository
            .find_available(
                "srv_1",
                "/etc/app.env",
                &record(FilesystemBackupStatus::Available).backup_path
            )
            .expect("consumed lookup")
            .is_none());
        assert!(repository
            .mark_restored(
                "srv_1",
                "/etc/app.env",
                &record(FilesystemBackupStatus::Available).backup_path,
                "2026-09-20T00:02:00Z",
            )
            .is_err());
    }

    #[test]
    fn listing_is_task_scoped_bounded_and_metadata_only() {
        let db = Database::in_memory().expect("database");
        let repository = db.filesystem_backups();
        let mut first = record(FilesystemBackupStatus::Available);
        first.id = "backup_1".into();
        first.created_at = "2026-09-20T00:02:00Z".into();
        repository.insert(&first).expect("insert first");

        let mut second = record(FilesystemBackupStatus::Available);
        second.id = "backup_2".into();
        second.backup_path.push('2');
        second.created_at = "2026-09-20T00:01:00Z".into();
        repository.insert(&second).expect("insert second");

        let mut other_task = record(FilesystemBackupStatus::Available);
        other_task.id = "backup_other".into();
        other_task.backup_path.push('3');
        other_task.task_id = Some("task_other".into());
        repository.insert(&other_task).expect("insert other task");

        let (rows, truncated) = repository
            .list_for_task("srv_1", "task_1", None, None, 1)
            .expect("list");
        assert!(truncated);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "backup_1");
        assert_eq!(rows[0].status, FilesystemBackupStatus::Available);

        let (filtered, filtered_truncated) = repository
            .list_for_task(
                "srv_1",
                "task_1",
                Some(FilesystemBackupStatus::Available),
                Some("/etc/app.env"),
                8,
            )
            .expect("filtered list");
        assert!(!filtered_truncated);
        assert_eq!(filtered.len(), 2);
        assert!(filtered
            .iter()
            .all(|row| row.task_id.as_deref() == Some("task_1")));
    }
}
