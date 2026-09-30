use std::path::PathBuf;

use yukinal_database::repositories::FileTransferRecord;
use yukinal_database::{Database, DatabaseError};

fn temp_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "yukinal-db-file-transfers-{tag}-{}.sqlite",
        std::process::id()
    ))
}

fn cleanup(path: &PathBuf) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    let _ = std::fs::remove_file(format!("{}.pre-migration.bak", path.display()));
    let _ = std::fs::remove_file(format!("{}.pre-migration.json", path.display()));
}

fn record(id: &str, status: &str, snapshot_status: &str) -> FileTransferRecord {
    FileTransferRecord {
        transfer_id: id.into(),
        server_id: "srv_transfer_test".into(),
        direction: "download".into(),
        status: status.into(),
        started_at_epoch_ms: 100,
        updated_at_epoch_ms: 120,
        snapshot_json: format!(
            r#"{{"transferId":"{id}","serverId":"srv_transfer_test","direction":"download","status":"{snapshot_status}","startedAtEpochMs":100,"updatedAtEpochMs":120,"activeConflict":{{}},"currentItem":"still-working","currentItemBytes":15,"currentItemTotalBytes":20}}"#
        ),
        // This private inventory is intentionally different from the snapshot and must survive
        // reopening so the host can reconcile only its recorded staging files.
        staging_json:
            r#"[{"location":"local","path":"C:\\private\\.file.yukinal-tr_1-100-1.part"}]"#.into(),
    }
}

#[test]
fn transfer_history_survives_reopen_and_is_bounded() {
    let path = temp_path("reopen");
    cleanup(&path);
    {
        let db = Database::open(&path).expect("open database");
        db.file_transfers()
            .insert(&record("tr_active", "running", "running"))
            .expect("insert active transfer");
        let mut done = record("tr_done", "completed", "completed");
        done.staging_json = "[]".into();
        db.file_transfers()
            .insert(&done)
            .expect("insert completed transfer");
        let (page, truncated) = db
            .file_transfers()
            .list(None, None, 1)
            .expect("read bounded page");
        assert_eq!(page.len(), 1);
        assert!(truncated);
    }

    let db = Database::open(&path).expect("reopen database");
    let active = db
        .file_transfers()
        .get("tr_active")
        .expect("lookup after reopen")
        .expect("row survives reopen");
    assert_eq!(active.status, "running");
    assert!(active.staging_json.contains("private"));
    let (all, truncated) = db
        .file_transfers()
        .list(Some("srv_transfer_test"), None, 10)
        .expect("list after reopen");
    assert_eq!(all.len(), 2);
    assert!(!truncated);
    drop(db);
    cleanup(&path);
}

#[test]
fn startup_reconciliation_interrupts_active_rows_but_never_rewrites_terminal_success() {
    let path = temp_path("interrupted");
    cleanup(&path);
    {
        let db = Database::open(&path).expect("open database");
        db.file_transfers()
            .insert(&record("tr_running", "running", "running"))
            .expect("insert running transfer");
        db.file_transfers()
            .insert(&record("tr_waiting", "waitingConflict", "waitingConflict"))
            .expect("insert conflict transfer");
        let mut done = record("tr_done", "completed", "completed");
        done.staging_json = "[]".into();
        db.file_transfers()
            .insert(&done)
            .expect("insert completed transfer");
    }

    let db = Database::open(&path).expect("reopen database");
    let interrupted = db
        .file_transfers()
        .interrupt_nonterminal(500)
        .expect("reconcile interrupted transfers");
    assert_eq!(interrupted.len(), 2);
    for row in &interrupted {
        assert_eq!(row.status, "interrupted");
        let snapshot: serde_json::Value =
            serde_json::from_str(&row.snapshot_json).expect("decode snapshot");
        assert_eq!(snapshot["status"], "interrupted");
        assert_eq!(snapshot["updatedAtEpochMs"], 500);
        assert!(snapshot["activeConflict"].is_null());
        assert!(snapshot["currentItem"].is_null());
        assert_eq!(snapshot["currentItemBytes"], 0);
        assert!(snapshot["currentItemTotalBytes"].is_null());
        assert!(row.staging_json.contains("private"));
    }
    let done = db
        .file_transfers()
        .get("tr_done")
        .expect("query terminal row")
        .expect("terminal row exists");
    assert_eq!(done.status, "completed");
    assert!(done.snapshot_json.contains("\"status\":\"completed\""));

    // The host's cleanup pass removed the recorded stage files; after that it clears
    // only the private staging inventory. The terminal snapshots remain unchanged.
    for mut row in interrupted {
        row.staging_json = "[]".into();
        db.file_transfers()
            .update(&row)
            .expect("persist cleaned staging inventory");
    }
    // A second startup pass has no nonterminal row or stage left to reconcile.
    assert!(db
        .file_transfers()
        .interrupt_nonterminal(900)
        .expect("repeat startup reconciliation")
        .is_empty());
    drop(db);
    cleanup(&path);
}

#[test]
fn history_rejects_unbounded_pages_and_oversized_metadata() {
    let path = temp_path("bounds");
    cleanup(&path);
    let db = Database::open(&path).expect("open database");
    assert!(matches!(
        db.file_transfers().list(None, None, 101),
        Err(DatabaseError::Validation(_))
    ));
    let mut oversized = record("tr_oversized", "queued", "queued");
    oversized.snapshot_json = " ".repeat(524_289);
    assert!(matches!(
        db.file_transfers().insert(&oversized),
        Err(DatabaseError::Validation(_))
    ));
    drop(db);
    cleanup(&path);
}
