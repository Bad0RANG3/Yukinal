//! `servers` rows. The row struct mirrors the shared `Server` contract shape; this
//! file is the flat-column <-> struct mapping.

use rusqlite::{params, OptionalExtension, Row};

use crate::models::{Environment, ServerStatus};
use crate::{optional_json, Database, DatabaseError, Result, Server};

/// `None` maps to a real SQL NULL, never to the JSON literal `"null"`.
fn json_or_null<T: serde::Serialize>(value: &Option<T>) -> Result<Option<String>> {
    value
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(DatabaseError::from)
}

pub struct ServersRepository<'a> {
    db: &'a Database,
}

impl<'a> ServersRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn insert(&self, server: &Server) -> Result<()> {
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO servers (
                    id, name, host, port, username, identity_id, group_id, capabilities,
                    status, environment, region, hostname, os, tags, workspace_ids,
                    created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                params![
                    server.id,
                    server.name,
                    server.connection.host,
                    i64::from(server.connection.port),
                    server.connection.username,
                    server.connection.identity_id,
                    server.group_id,
                    serde_json::to_string(&server.capabilities)?,
                    server.status.as_str(),
                    server.metadata.environment.as_str(),
                    server.metadata.region,
                    server.metadata.hostname,
                    server.metadata.os,
                    json_or_null(&server.metadata.tags)?,
                    json_or_null(&server.metadata.workspace_ids)?,
                    server.created_at,
                    server.updated_at,
                ],
            )?;
            Ok(())
        })
    }

    /// Full-row overwrite; the caller supplies the new `updated_at`.
    pub fn update(&self, server: &Server) -> Result<()> {
        self.db.with(|connection| {
            let changed = connection.execute(
                "UPDATE servers SET
                    name = ?2, host = ?3, port = ?4, username = ?5, identity_id = ?6,
                    group_id = ?7, capabilities = ?8, status = ?9, environment = ?10,
                    region = ?11, hostname = ?12, os = ?13, tags = ?14, workspace_ids = ?15,
                    updated_at = ?16
                 WHERE id = ?1",
                params![
                    server.id,
                    server.name,
                    server.connection.host,
                    i64::from(server.connection.port),
                    server.connection.username,
                    server.connection.identity_id,
                    server.group_id,
                    serde_json::to_string(&server.capabilities)?,
                    server.status.as_str(),
                    server.metadata.environment.as_str(),
                    server.metadata.region,
                    server.metadata.hostname,
                    server.metadata.os,
                    json_or_null(&server.metadata.tags)?,
                    json_or_null(&server.metadata.workspace_ids)?,
                    server.updated_at,
                ],
            )?;
            if changed == 0 {
                return Err(DatabaseError::NotFound);
            }
            Ok(())
        })
    }

    /// Delete the server row and everything that cascades from it (snapshots,
    /// services, `server_identities` via foreign keys).
    pub fn delete(&self, id: &str) -> Result<()> {
        self.db.with(|connection| {
            let changed = connection.execute("DELETE FROM servers WHERE id = ?1", params![id])?;
            if changed == 0 {
                return Err(DatabaseError::NotFound);
            }
            Ok(())
        })
    }

    pub fn get(&self, id: &str) -> Result<Server> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, name, host, port, username, identity_id, group_id, capabilities,
                            status, environment, region, hostname, os, tags, workspace_ids,
                            created_at, updated_at
                     FROM servers WHERE id = ?1",
                    params![id],
                    row_to_server,
                )
                .optional()
                .map_err(DatabaseError::from)?
                .ok_or(DatabaseError::NotFound)
        })
    }

    pub fn list(&self) -> Result<Vec<Server>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, name, host, port, username, identity_id, group_id, capabilities,
                        status, environment, region, hostname, os, tags, workspace_ids,
                        created_at, updated_at
                 FROM servers ORDER BY name",
            )?;
            let rows = statement.query_map([], row_to_server)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
        })
    }
}

fn row_to_server(row: &Row<'_>) -> rusqlite::Result<Server> {
    let capabilities = row.get::<_, String>(7)?;
    let status = row.get::<_, String>(8)?;
    let environment = row.get::<_, String>(9)?;
    let tags = optional_json(row.get::<_, Option<String>>(13)?);
    let workspace_ids = optional_json(row.get::<_, Option<String>>(14)?);

    Ok(Server {
        id: row.get(0)?,
        name: row.get(1)?,
        connection: crate::models::ServerConnection {
            host: row.get(2)?,
            port: row
                .get::<_, i64>(3)?
                .try_into()
                .map_err(|_| decode_error(3, "port out of u16 range"))?,
            username: row.get(4)?,
            identity_id: row.get(5)?,
        },
        group_id: row.get(6)?,
        capabilities: decode_json(&capabilities, 7)?,
        status: ServerStatus::from_db(&status)
            .ok_or_else(|| decode_error(8, "unknown server status"))?,
        metadata: crate::models::ServerMetadata {
            environment: Environment::from_db(&environment)
                .ok_or_else(|| decode_error(9, "unknown environment"))?,
            region: row.get(10)?,
            hostname: row.get(11)?,
            os: row.get(12)?,
            tags: tags.map_err(|error| decode_error_with(13, error))?,
            workspace_ids: workspace_ids.map_err(|error| decode_error_with(14, error))?,
        },
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
    })
}

fn decode_json<T: serde::de::DeserializeOwned>(raw: &str, index: usize) -> rusqlite::Result<T> {
    serde_json::from_str(raw).map_err(|error| decode_error_with(index, error))
}

fn decode_error(index: usize, message: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, message.into())
}

fn decode_error_with(index: usize, error: impl std::fmt::Display) -> rusqlite::Error {
    decode_error(index, &error.to_string())
}
