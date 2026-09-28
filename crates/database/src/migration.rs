//! Pre-migration backup and rollback.
//!
//! Opening a database that carries old rows is the one moment where a bug in a
//! schema change can destroy data the user cannot recreate. The migration
//! transactions in [`crate::schema`] already make each version all-or-nothing,
//! but a transaction is not a recovery point: a later migration can still fail
//! after an earlier one committed, and a partially applied file is exactly the
//! state a user must not be left with.
//!
//! So before the first pending migration runs, [`crate::Database::open`] takes
//! one bounded copy of the database as it exists on disk and writes a small
//! manifest next to it (source version, target version, SHA-256, timestamp). If
//! any migration then fails, the copy is restored over the database and the
//! WAL/SHM sidecars are removed, so the next open sees the exact pre-migration
//! file. The copy is deliberately kept after a *successful* migration too: it
//! is the previous-version recovery point the release evidence points at.
//!
//! The copy is produced with SQLite's `VACUUM INTO`, which reads through the live
//! connection: uncheckpointed WAL pages are included and the result is a single
//! consistent file, rather than a byte copy of a main file whose recent writes
//! only exist in the `-wal` sidecar.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::{schema, DatabaseError, Result};

/// One retained pre-migration copy. The suffix is fixed, so the recovery point
/// is bounded to a single file per database and never grows with release count.
const BACKUP_SUFFIX: &str = ".pre-migration.bak";
const MANIFEST_SUFFIX: &str = ".pre-migration.json";

pub(crate) struct PreMigrationBackup {
    path: PathBuf,
}

impl PreMigrationBackup {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Copy `database` through `connection` and write the checksum manifest.
    ///
    /// The previous copy and manifest are removed first: when this returns, the
    /// recovery point describes the database just before *this* upgrade.
    pub(crate) fn create(
        connection: &Connection,
        database: &Path,
        source_version: i64,
    ) -> Result<Self> {
        let path = with_suffix(database, BACKUP_SUFFIX);
        let manifest = with_suffix(database, MANIFEST_SUFFIX);
        remove_if_present(&path).map_err(|error| backup_error(&path, error))?;
        remove_if_present(&manifest).map_err(|error| backup_error(&path, error))?;

        connection
            .execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()])
            .map_err(|error| backup_error(&path, error))?;

        let bytes = fs::read(&path).map_err(|error| backup_error(&path, error))?;
        let record = serde_json::json!({
            "sourceVersion": source_version,
            "targetVersion": schema::supported_version(),
            "sha256": format!("{:x}", Sha256::digest(&bytes)),
            "bytes": bytes.len(),
            "createdAt": yukinal_time::iso8601_now(),
        });
        let encoded =
            serde_json::to_vec_pretty(&record).map_err(|error| backup_error(&path, error))?;
        fs::write(&manifest, encoded).map_err(|error| backup_error(&path, error))?;

        Ok(Self { path })
    }

    /// Put the pre-migration copy back and drop the WAL/SHM sidecars. Deleting
    /// the sidecars matters: they belong to the failed migration's connection,
    /// and replaying them onto the restored main file would reintroduce exactly
    /// the half-applied state the copy exists to undo.
    pub(crate) fn restore(&self, database: &Path) -> Result<()> {
        fs::copy(&self.path, database).map_err(|error| backup_error(&self.path, error))?;
        remove_if_present(&with_suffix(database, "-wal"))
            .map_err(|error| backup_error(&self.path, error))?;
        remove_if_present(&with_suffix(database, "-shm"))
            .map_err(|error| backup_error(&self.path, error))?;
        Ok(())
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn backup_error(path: &Path, error: impl std::fmt::Display) -> DatabaseError {
    DatabaseError::MigrationBackup {
        path: path.display().to_string(),
        message: error.to_string(),
    }
}
