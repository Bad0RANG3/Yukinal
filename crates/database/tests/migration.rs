//! Acceptance for the pre-migration backup and rollback path (implementation
//! plan work package B).
//!
//! The claim under test is narrow and load-bearing: opening an old database
//! either upgrades it with a retained, checksummed copy of the previous version,
//! or fails without changing the data the user already had. "Fails" is not
//! enough — the database must be byte-equivalent at the version it started at,
//! and the error must name both the failing migration and the recovery point.

mod common;
use common::*;

use std::path::{Path, PathBuf};

use rusqlite::OpenFlags;
use serde_json::Value;

fn temp_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("yukinal-db-{tag}-{}.sqlite", std::process::id()))
}

fn backup_path(database: &Path) -> PathBuf {
    PathBuf::from(format!("{}.pre-migration.bak", database.display()))
}

fn manifest_path(database: &Path) -> PathBuf {
    PathBuf::from(format!("{}.pre-migration.json", database.display()))
}

/// Remove the database and every file this feature may leave beside it.
fn cleanup_migration(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(backup_path(path));
    let _ = std::fs::remove_file(manifest_path(path));
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}

#[test]
fn failed_migration_restores_the_pre_migration_file() {
    let path = temp_path("migration-rollback");
    cleanup_migration(&path);
    {
        // A v34 database whose next migration (35) targets `mcp_servers`, which
        // this fixture deliberately does not have: the upgrade must fail.
        let raw = Connection::open(&path).expect("create legacy database");
        raw.execute_batch(
            "CREATE TABLE marker (value TEXT NOT NULL);
             INSERT INTO marker VALUES ('kept');
             PRAGMA user_version = 34;",
        )
        .expect("write legacy schema");
    }

    let error = Database::open(&path).expect_err("migration 35 must fail without mcp_servers");
    let backup = match &error {
        DatabaseError::MigrationRolledBack { backup, message } => {
            assert!(
                message.contains("35"),
                "the error must name the failing migration version: {message}"
            );
            PathBuf::from(backup)
        }
        other => panic!("expected a rolled-back migration, got {other:?}"),
    };

    // The recovery point is retained and the original database is unchanged.
    assert!(backup.exists(), "the pre-migration copy must survive");
    let raw = Connection::open(&path).expect("reopen the database");
    let version: i64 = raw
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("read version");
    assert_eq!(
        version, 34,
        "the failed upgrade must not advance the version"
    );
    let marker: String = raw
        .query_row("SELECT value FROM marker", [], |row| row.get(0))
        .expect("read the row that existed before the migration");
    assert_eq!(marker, "kept");
    drop(raw);

    cleanup_migration(&path);
}

#[test]
fn failed_migration_names_the_recovery_point_and_writes_a_manifest() {
    let path = temp_path("migration-manifest");
    cleanup_migration(&path);
    {
        let raw = Connection::open(&path).expect("create legacy database");
        raw.execute_batch(
            "CREATE TABLE marker (value TEXT NOT NULL);
             INSERT INTO marker VALUES ('kept');
             PRAGMA user_version = 34;",
        )
        .expect("write legacy schema");
    }

    let error = Database::open(&path).expect_err("upgrade must fail");
    let backup = match &error {
        DatabaseError::MigrationRolledBack { backup, .. } => PathBuf::from(backup),
        other => panic!("expected a rolled-back migration, got {other:?}"),
    };

    let manifest: Value = serde_json::from_slice(
        &std::fs::read(manifest_path(&path)).expect("manifest must be written"),
    )
    .expect("manifest is JSON");
    assert_eq!(manifest["sourceVersion"], 34);
    assert!(
        manifest["targetVersion"].as_i64().unwrap() > 34,
        "the manifest must name the version we tried to reach"
    );
    let digest = format!(
        "{:x}",
        Sha256::digest(std::fs::read(&backup).expect("read backup"))
    );
    assert_eq!(
        manifest["sha256"], digest,
        "the manifest checksum must describe the retained copy"
    );

    cleanup_migration(&path);
}

#[test]
fn successful_migration_retains_a_previous_version_copy_with_its_rows() {
    let path = temp_path("migration-upgrade");
    cleanup_migration(&path);
    {
        // Build the current schema, then rewind exactly one migration so the
        // reopen exercises a real upgrade rather than a no-op.
        let db = Database::open(&path).expect("create current database");
        db.servers()
            .insert(&sample_server("srv_backup"))
            .expect("insert a row that must survive into the backup");
        drop(db);
        let raw = Connection::open(&path).expect("open raw");
        raw.execute_batch(
            "ALTER TABLE mcp_servers DROP COLUMN annotation_trust;
             DROP TABLE file_transfers;
             PRAGMA user_version = 35;",
        )
        .expect("rewind to the previous version");
    }

    let db = Database::open(&path).expect("upgrade one version");
    // The upgraded database is usable and still has the row.
    assert_eq!(
        db.servers()
            .get("srv_backup")
            .expect("row after upgrade")
            .name,
        "Production API"
    );
    drop(db);

    // The retained copy is the *previous* version and still carries the row.
    let backup = backup_path(&path);
    let previous = Connection::open_with_flags(&backup, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open backup");
    let version: i64 = previous
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("backup version");
    assert_eq!(version, 35, "the copy must be the pre-migration version");
    let name: String = previous
        .query_row(
            "SELECT name FROM servers WHERE id = 'srv_backup'",
            [],
            |row| row.get(0),
        )
        .expect("the copy must contain the pre-migration rows");
    assert_eq!(name, "Production API");
    drop(previous);

    let manifest: Value = serde_json::from_slice(
        &std::fs::read(manifest_path(&path)).expect("manifest must be written"),
    )
    .expect("manifest is JSON");
    let digest = format!(
        "{:x}",
        Sha256::digest(std::fs::read(&backup).expect("read backup"))
    );
    assert_eq!(manifest["sha256"], digest);

    cleanup_migration(&path);
}

/// The copy must be taken through the live connection, not by reading the main
/// file: a recent write can live only in the `-wal` sidecar, and a byte copy
/// would silently omit it.
#[test]
fn backup_includes_rows_that_exist_only_in_the_wal_sidecar() {
    let path = temp_path("migration-wal");
    cleanup_migration(&path);
    {
        let db = Database::open(&path).expect("create current database");
        drop(db);
        let writer = Connection::open(&path).expect("open writer");
        writer
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA wal_autocheckpoint = 0;
                 ALTER TABLE mcp_servers DROP COLUMN annotation_trust;
                 DROP TABLE file_transfers;",
            )
            .expect("prepare a WAL-only write");
        writer
            .execute(
                "INSERT INTO app_settings (key, value, updated_at) VALUES ('wal.marker', '1', '2026-01-01T00:00:00Z')",
                [],
            )
            .expect("write a row that stays in the WAL");
        writer
            .pragma_update(None, "user_version", 35)
            .expect("rewind version inside the WAL");

        // The writer is still open, so the row above is not in the main file;
        // it can only reach the backup through SQLite's own read path.
        let upgraded = Database::open(&path).expect("upgrade while WAL pages are pending");
        drop(upgraded);
        drop(writer);
    }

    let backup = backup_path(&path);
    let previous = Connection::open_with_flags(&backup, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open backup");
    let marker: String = previous
        .query_row(
            "SELECT value FROM app_settings WHERE key = 'wal.marker'",
            [],
            |row| row.get(0),
        )
        .expect("the WAL-only row must be in the copy");
    assert_eq!(marker, "1");

    cleanup_migration(&path);
}

#[test]
fn a_brand_new_database_has_no_recovery_point() {
    let path = temp_path("migration-new");
    cleanup_migration(&path);
    let db = Database::open(&path).expect("create a new database");
    drop(db);
    assert!(
        !backup_path(&path).exists(),
        "there is nothing to preserve on first creation"
    );
    cleanup_migration(&path);
}

/// A database from a newer build is refused before any migration or copy: the
/// old binary must not "protect" a file it cannot understand by rewriting it.
#[test]
fn a_newer_schema_is_refused_without_creating_a_recovery_point() {
    let path = temp_path("migration-newer");
    cleanup_migration(&path);
    {
        let db = Database::open(&path).expect("create current database");
        drop(db);
        let raw = Connection::open(&path).expect("open raw");
        raw.pragma_update(None, "user_version", 999)
            .expect("simulate a newer binary");
    }

    let error = Database::open(&path).expect_err("a newer schema must be refused");
    assert!(matches!(error, DatabaseError::NewerSchema { .. }));
    assert!(
        !backup_path(&path).exists(),
        "an unreadable newer database must be left alone"
    );
    let raw = Connection::open(&path).expect("reopen the database");
    let version: i64 = raw
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("version");
    assert_eq!(
        version, 999,
        "the refused open must not have changed the file"
    );

    cleanup_migration(&path);
}
