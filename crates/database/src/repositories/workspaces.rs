//! `workspaces` rows. JSON columns carry the nested collections.

use rusqlite::{params, OptionalExtension, Row};

use crate::models::{Environment, Workspace};
use crate::{json_column, Database, DatabaseError, Result};

pub struct WorkspacesRepository<'a> {
    db: &'a Database,
}

impl<'a> WorkspacesRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn insert(&self, workspace: &Workspace) -> Result<()> {
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO workspaces (id, name, server_ids, repositories, provider_ids, default_environment)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    workspace.id,
                    workspace.name,
                    serde_json::to_string(&workspace.server_ids)?,
                    serde_json::to_string(&workspace.repositories)?,
                    serde_json::to_string(&workspace.provider_ids)?,
                    workspace.default_environment.as_str(),
                ],
            )?;
            Ok(())
        })
    }

    pub fn update(&self, workspace: &Workspace) -> Result<()> {
        self.db.with(|connection| {
            let changed = connection.execute(
                "UPDATE workspaces SET name = ?2, server_ids = ?3, repositories = ?4,
                        provider_ids = ?5, default_environment = ?6
                 WHERE id = ?1",
                params![
                    workspace.id,
                    workspace.name,
                    serde_json::to_string(&workspace.server_ids)?,
                    serde_json::to_string(&workspace.repositories)?,
                    serde_json::to_string(&workspace.provider_ids)?,
                    workspace.default_environment.as_str(),
                ],
            )?;
            if changed == 0 {
                return Err(DatabaseError::NotFound);
            }
            Ok(())
        })
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        self.db.with(|connection| {
            let changed =
                connection.execute("DELETE FROM workspaces WHERE id = ?1", params![id])?;
            if changed == 0 {
                return Err(DatabaseError::NotFound);
            }
            Ok(())
        })
    }

    pub fn get(&self, id: &str) -> Result<Workspace> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, name, server_ids, repositories, provider_ids, default_environment
                     FROM workspaces WHERE id = ?1",
                    params![id],
                    row_to_workspace,
                )
                .optional()
                .map_err(DatabaseError::from)?
                .ok_or(DatabaseError::NotFound)
        })
    }

    pub fn list(&self) -> Result<Vec<Workspace>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, name, server_ids, repositories, provider_ids, default_environment
                 FROM workspaces ORDER BY name",
            )?;
            let rows = statement.query_map([], row_to_workspace)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
        })
    }
}

fn row_to_workspace(row: &Row<'_>) -> rusqlite::Result<Workspace> {
    let server_ids = json_column::<Vec<String>>(&row.get::<_, String>(2)?)
        .map_err(|error| decode_err(2, error))?;
    let repositories =
        json_column::<Vec<crate::models::WorkspaceRepository>>(&row.get::<_, String>(3)?)
            .map_err(|error| decode_err(3, error))?;
    let provider_ids = json_column::<Vec<String>>(&row.get::<_, String>(4)?)
        .map_err(|error| decode_err(4, error))?;
    let default_environment = Environment::from_db(&row.get::<_, String>(5)?)
        .ok_or_else(|| decode_err(5, "unknown environment"))?;

    Ok(Workspace {
        id: row.get(0)?,
        name: row.get(1)?,
        server_ids,
        repositories,
        provider_ids,
        default_environment,
    })
}

fn decode_err(index: usize, error: impl std::fmt::Display) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        error.to_string().into(),
    )
}
