//! yukinal-database — SQLite local storage (offline-first).
//!
//! Tables: servers / groups / workspaces / identities / server_identities /
//! snapshots / services / activities / chat_sessions / chat_messages /
//! tool_executions / provider_configs / mcp_servers / credential_cleanup_queue /
//! filesystem_backups.
//!
//! Rules:
//! - Only `credential_ref` is stored. Secret material (key material, passwords,
//!   API keys) lives in the OS keychain, never in this crate.
//! - All writes that matter for audit (tool executions, activities) go through
//!   the repository layer, so the audit chain is one code path.
//! - Schema is versioned (`schema::migrate`); a schema newer than this binary is
//!   refused, never half-migrated.
//!
//! The connection is synchronous and guarded by a mutex: desktop-local writes are
//! short and bounded. Row types in `models` serialise as camelCase, which is the
//! wire shape of the IPC contract, so command layers can pass them through.
//!
//! On threading, stated accurately rather than as an aspiration: **no caller uses
//! `spawn_blocking`.** The command modules that are synchronous (`chat`, `execution`,
//! `host`, `workspace`) do not need it — Tauri runs non-async commands on a blocking
//! pool. But the async ones (`server`, `provider`, `terminal`, `agent_run`,
//! `activity`, `commands/mod`) call repository methods inline, so each query occupies
//! an async worker thread for its duration.
//!
//! That is a deliberate trade for local SQLite: the queries are single-row reads and
//! small writes against a file on the same disk, and the alternative — wrapping every
//! call site in `spawn_blocking` — adds a task hop, a `Send` bound on every return
//! type, and an error mapping that obscures the call. The honest caveat is that this
//! reasoning is load-bearing: if a query here ever becomes long (a full-table scan,
//! an unindexed join, a migration run inline), it will stall an async worker rather
//! than merely being slow, and that is the moment to reach for `spawn_blocking`.
//! A previous version of this comment described callers doing that already.
//!
//! Audit note (2026-09-16): the widest current query is conversation search, but every
//! chat command is a synchronous Tauri command and therefore already runs on Tauri's
//! blocking pool. Schema migrations and credential-cleanup reconciliation run during
//! startup assembly, before async command work begins. No current async command performs
//! an unbounded SQLite scan. If one starts to, this crate should expose a blocking adapter
//! rather than running that query inline.

mod migration;
pub mod models;
pub mod repositories;
mod schema;

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

pub use models::{
    AddServerInput, AuthenticationInput, Server, ServerStatus, UpdateServerInput, Workspace,
};

pub type Result<T> = std::result::Result<T, DatabaseError>;

#[derive(Debug, thiserror::Error)]
pub enum DatabaseError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("malformed row: {0}")]
    Decode(String),
    #[error("json: {0}")]
    SerdeJson(#[from] serde_json::Error),
    #[error("row not found")]
    NotFound,
    #[error("validation failed: {0}")]
    Validation(String),
    /// The on-disk schema is newer than this binary understands.
    #[error("database schema is version {schema}, this build supports {app}")]
    NewerSchema { schema: i64, app: i64 },
    /// A single migration failed. The message names the version so a half-upgraded
    /// database is attributable to one step instead of a bare sqlite error.
    #[error("migration to version {version} failed: {message}")]
    Migration { version: i64, message: String },
    /// The pre-migration copy could not be made. Migrations are not attempted
    /// without a recovery point, so this is a fail-closed startup error.
    #[error("could not prepare a pre-migration backup at {path}: {message}")]
    MigrationBackup { path: String, message: String },
    /// A migration failed and the database was restored from the retained copy.
    #[error("migration failed and was rolled back from {backup}: {message}")]
    MigrationRolledBack { message: String, backup: String },
    /// A migration failed and the rollback itself failed. The retained copy at
    /// `backup` is the manual recovery point; the database is in an unknown state.
    #[error(
        "migration failed and rolling back from {backup} also failed: {rollback} (migration: {message})"
    )]
    MigrationRollbackFailed {
        message: String,
        backup: String,
        rollback: String,
    },
    #[error("failed to create database directory {0}: {1}")]
    Io(String, #[source] std::io::Error),
}

/// The SQLITE3 API supports a `SQLITE_THREADSAFE` build mode where the connection
/// must not be shared; guarding it makes the mode irrelevant (1 mutex, no cross
/// thread access), so the app behaves the same however it was compiled.
#[derive(Debug)]
pub struct Database {
    connection: Mutex<Connection>,
}

impl Database {
    /// Open (creating when needed) a database file, apply pending migrations and
    /// wire the per-connection pragmas the schema relies on.
    ///
    /// When migrations are pending on an existing file, one bounded pre-migration
    /// copy is written first (see the `migration` module). A failed migration restores
    /// that copy and returns [`DatabaseError::MigrationRolledBack`], so the caller
    /// never observes a half-upgraded database.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| DatabaseError::Io(parent.display().to_string(), error))?;
            }
        }
        // Captured before `Connection::open`, which creates a 0-byte file for a
        // brand-new database. There is nothing to preserve in that case.
        let existing = path.metadata().map(|m| m.len() > 0).unwrap_or(false);
        let mut connection = Connection::open(path)?;
        Self::prepare(&mut connection)?;

        let backup = if existing && schema::has_pending(&connection)? {
            let source = schema::on_disk_version(&connection)?;
            Some(migration::PreMigrationBackup::create(
                &connection,
                path,
                source,
            )?)
        } else {
            None
        };

        match schema::migrate(&connection) {
            Ok(()) => Ok(Self {
                connection: Mutex::new(connection),
            }),
            Err(error) => {
                drop(connection);
                match backup {
                    Some(backup) => match backup.restore(path) {
                        Ok(()) => Err(DatabaseError::MigrationRolledBack {
                            message: error.to_string(),
                            backup: backup.path().display().to_string(),
                        }),
                        Err(rollback) => Err(DatabaseError::MigrationRollbackFailed {
                            message: error.to_string(),
                            backup: backup.path().display().to_string(),
                            rollback: rollback.to_string(),
                        }),
                    },
                    None => Err(error),
                }
            }
        }
    }

    /// Test/sandbox handle; `:memory:` connections have no file to re-open.
    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        let mut connection = Connection::open_in_memory()?;
        Self::prepare(&mut connection)?;
        schema::migrate(&connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn prepare(connection: &mut Connection) -> Result<()> {
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        Ok(())
    }

    pub(crate) fn with<R>(&self, f: impl FnOnce(&Connection) -> Result<R>) -> Result<R> {
        let connection = self.connection.lock().map_err(|_| {
            DatabaseError::Io(
                "database mutex poisoned".into(),
                std::io::Error::other("poisoned"),
            )
        })?;
        f(&connection)
    }

    // -- repository views -----------------------------------------------------

    pub fn servers(&self) -> repositories::ServersRepository<'_> {
        repositories::ServersRepository::new(self)
    }

    pub fn workspaces(&self) -> repositories::WorkspacesRepository<'_> {
        repositories::WorkspacesRepository::new(self)
    }

    pub fn identities(&self) -> repositories::IdentitiesRepository<'_> {
        repositories::IdentitiesRepository::new(self)
    }

    pub fn providers(&self) -> repositories::ProviderConfigsRepository<'_> {
        repositories::ProviderConfigsRepository::new(self)
    }

    pub fn mcp_servers(&self) -> repositories::McpServersRepository<'_> {
        repositories::McpServersRepository::new(self)
    }

    pub fn app_settings(&self) -> repositories::AppSettingsRepository<'_> {
        repositories::AppSettingsRepository::new(self)
    }

    pub fn credential_cleanup(&self) -> repositories::CredentialCleanupRepository<'_> {
        repositories::CredentialCleanupRepository::new(self)
    }

    pub fn snapshots(&self) -> repositories::SnapshotsRepository<'_> {
        repositories::SnapshotsRepository::new(self)
    }

    pub fn executions(&self) -> repositories::ToolExecutionsRepository<'_> {
        repositories::ToolExecutionsRepository::new(self)
    }

    pub fn host_tool_calls(&self) -> repositories::HostToolCallsRepository<'_> {
        repositories::HostToolCallsRepository::new(self)
    }

    pub fn filesystem_backups(&self) -> repositories::FilesystemBackupsRepository<'_> {
        repositories::FilesystemBackupsRepository::new(self)
    }

    pub fn activities(&self) -> repositories::ActivitiesRepository<'_> {
        repositories::ActivitiesRepository::new(self)
    }

    pub fn investigations(&self) -> repositories::InvestigationsRepository<'_> {
        repositories::InvestigationsRepository::new(self)
    }

    pub fn investigation_retention(&self) -> repositories::InvestigationRetentionRepository<'_> {
        repositories::InvestigationRetentionRepository::new(self)
    }

    pub fn chat(&self) -> repositories::ChatRepository<'_> {
        repositories::ChatRepository::new(self)
    }
}

/// Helper: parse a `NOT NULL` JSON column into a serde model.
pub(crate) fn json_column<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T> {
    serde_json::from_str(raw).map_err(|error| DatabaseError::Decode(error.to_string()))
}

/// Helper: `Option<T>` from a nullable JSON-ish column (empty string = None).
pub(crate) fn optional_json<T: serde::de::DeserializeOwned>(
    raw: Option<String>,
) -> Result<Option<T>> {
    match raw {
        None => Ok(None),
        // JSON `null` written by a serde round-trip of `None` should read back as None.
        Some(s) if s.trim().is_empty() || s.trim() == "null" => Ok(None),
        Some(s) => serde_json::from_str(&s)
            .map(Some)
            .map_err(|error| DatabaseError::Decode(error.to_string())),
    }
}
