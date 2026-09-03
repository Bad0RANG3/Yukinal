//! yukinal-database — SQLite local storage (offline-first).
//!
//! Tables: servers / groups / workspaces / identities / server_identities /
//! snapshots / services / activities / chat_sessions / chat_messages /
//! tool_executions / provider_configs / mcp_servers.
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
//! short and bounded, and callers that need async run the repository call inside
//! `spawn_blocking`. Row types in `models` serialise as camelCase, which is the
//! wire shape of the IPC contract, so command layers can pass them through.

pub mod models;
pub mod repositories;
mod schema;

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

pub use models::{AddServerInput, AuthenticationInput, Server, ServerStatus, Workspace};

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
    /// The on-disk schema is newer than this binary understands.
    #[error("database schema is version {schema}, this build supports {app}")]
    NewerSchema { schema: i64, app: i64 },
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
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| DatabaseError::Io(parent.display().to_string(), error))?;
            }
        }
        let mut connection = Connection::open(path)?;
        Self::init(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    /// Test/sandbox handle; `:memory:` connections have no file to re-open.
    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        let mut connection = Connection::open_in_memory()?;
        Self::init(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn init(connection: &mut Connection) -> Result<()> {
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        schema::migrate(connection)?;
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

    pub fn snapshots(&self) -> repositories::SnapshotsRepository<'_> {
        repositories::SnapshotsRepository::new(self)
    }

    pub fn executions(&self) -> repositories::ToolExecutionsRepository<'_> {
        repositories::ToolExecutionsRepository::new(self)
    }

    pub fn activities(&self) -> repositories::ActivitiesRepository<'_> {
        repositories::ActivitiesRepository::new(self)
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
