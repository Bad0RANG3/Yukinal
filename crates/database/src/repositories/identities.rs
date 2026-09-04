//! `identities` + `server_identities`. Identities carry only a reference into the
//! OS keychain (`credential_ref`); the secret itself never passes through here.

use rusqlite::{params, OptionalExtension, Row};

use crate::models::Identity;
use crate::{Database, DatabaseError, Result};

pub struct IdentitiesRepository<'a> {
    db: &'a Database,
}

impl<'a> IdentitiesRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn insert(&self, identity: &Identity) -> Result<()> {
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO identities (id, label, method, credential_ref, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    identity.id,
                    identity.label,
                    identity.method,
                    identity.credential_ref,
                    identity.created_at,
                ],
            )?;
            Ok(())
        })
    }

    pub fn update(&self, identity: &Identity) -> Result<()> {
        self.db.with(|connection| {
            let changed = connection.execute(
                "UPDATE identities SET label = ?2, method = ?3, credential_ref = ?4 WHERE id = ?1",
                params![
                    identity.id,
                    identity.label,
                    identity.method,
                    identity.credential_ref
                ],
            )?;
            if changed == 0 {
                return Err(DatabaseError::NotFound);
            }
            Ok(())
        })
    }

    pub fn get(&self, id: &str) -> Result<Identity> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, label, method, credential_ref, created_at
                     FROM identities WHERE id = ?1",
                    params![id],
                    row_to_identity,
                )
                .optional()
                .map_err(DatabaseError::from)?
                .ok_or(DatabaseError::NotFound)
        })
    }

    pub fn list(&self) -> Result<Vec<Identity>> {
        self.db.with(|connection| {
            let mut statement = connection
                .prepare("SELECT id, label, method, credential_ref, created_at FROM identities ORDER BY label")?;
            let rows = statement.query_map([], row_to_identity)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
        })
    }

    /// Delete the identity row. The caller is responsible for reclaiming the
    /// underlying credential in the OS keychain (see the credentials crate).
    /// `server_identities` rows cascade.
    pub fn delete(&self, id: &str) -> Result<()> {
        self.db.with(|connection| {
            let changed =
                connection.execute("DELETE FROM identities WHERE id = ?1", params![id])?;
            if changed == 0 {
                return Err(DatabaseError::NotFound);
            }
            Ok(())
        })
    }

    // -- server_identities ---------------------------------------------------

    pub fn attach_to_server(&self, server_id: &str, identity_id: &str) -> Result<()> {
        self.db.with(|connection| {
            connection.execute(
                "INSERT OR IGNORE INTO server_identities (server_id, identity_id) VALUES (?1, ?2)",
                params![server_id, identity_id],
            )?;
            Ok(())
        })
    }

    pub fn detach_from_server(&self, server_id: &str, identity_id: &str) -> Result<()> {
        self.db.with(|connection| {
            connection.execute(
                "DELETE FROM server_identities WHERE server_id = ?1 AND identity_id = ?2",
                params![server_id, identity_id],
            )?;
            Ok(())
        })
    }

    /// Identity ids currently attached to a server.
    pub fn ids_for_server(&self, server_id: &str) -> Result<Vec<String>> {
        self.db.with(|connection| {
            let mut statement = connection
                .prepare("SELECT identity_id FROM server_identities WHERE server_id = ?1")?;
            let rows = statement.query_map(params![server_id], |row| row.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
        })
    }

    pub fn attached_to_other_server(
        &self,
        identity_id: &str,
        excluding_server_id: &str,
    ) -> Result<bool> {
        self.db.with(|connection| {
            let attached: i64 = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM server_identities WHERE identity_id = ?1 AND server_id <> ?2)",
                params![identity_id, excluding_server_id],
                |row| row.get(0),
            )?;
            Ok(attached != 0)
        })
    }
}

fn row_to_identity(row: &Row<'_>) -> rusqlite::Result<Identity> {
    Ok(Identity {
        id: row.get(0)?,
        label: row.get(1)?,
        method: row.get(2)?,
        credential_ref: row.get(3)?,
        created_at: row.get(4)?,
    })
}
