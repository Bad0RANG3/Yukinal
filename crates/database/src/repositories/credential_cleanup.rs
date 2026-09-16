use rusqlite::params;

use crate::models::PendingCredentialCleanup;
use crate::Database;
use crate::Result;

pub struct CredentialCleanupRepository<'a> {
    db: &'a Database,
}

impl<'a> CredentialCleanupRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn enqueue(&self, reference: &str, error: &str, now: &str) -> Result<()> {
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO credential_cleanup_queue (reference, created_at, attempts, last_error)
                 VALUES (?1, ?2, 1, ?3)
                 ON CONFLICT(reference) DO UPDATE SET
                    attempts = credential_cleanup_queue.attempts + 1,
                    last_error = excluded.last_error",
                params![reference, now, error],
            )?;
            Ok(())
        })
    }

    pub fn list(&self) -> Result<Vec<PendingCredentialCleanup>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT reference, created_at, attempts, last_error
                 FROM credential_cleanup_queue
                 ORDER BY created_at, reference",
            )?;
            let rows = statement.query_map([], |row| {
                Ok(PendingCredentialCleanup {
                    reference: row.get(0)?,
                    created_at: row.get(1)?,
                    attempts: row.get(2)?,
                    last_error: row.get(3)?,
                })
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        })
    }

    pub fn remove(&self, reference: &str) -> Result<()> {
        self.db.with(|connection| {
            connection.execute(
                "DELETE FROM credential_cleanup_queue WHERE reference = ?1",
                params![reference],
            )?;
            Ok(())
        })
    }
}
