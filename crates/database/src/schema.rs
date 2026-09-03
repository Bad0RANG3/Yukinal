//! Versioned schema. Migration N is applied when `PRAGMA user_version < N`.
//! Never edit an applied migration: add a new one at the end (see `MIGRATIONS`).

use rusqlite::Connection;

use crate::{DatabaseError, Result};

const MIGRATIONS: &[&str] = &[
    // 1 — initial schema (tables per the local database plan).
    r#"
    CREATE TABLE servers (
        id          TEXT PRIMARY KEY,
        name        TEXT NOT NULL,
        host        TEXT NOT NULL,
        port        INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
        username    TEXT NOT NULL,
        identity_id TEXT,
        group_id    TEXT,
        capabilities TEXT NOT NULL DEFAULT '{}',   -- JSON, camelCase keys
        status      TEXT NOT NULL CHECK (status IN ('connecting','connected','disconnected','error')),
        environment TEXT NOT NULL CHECK (environment IN ('local','development','staging','production','unknown')),
        region      TEXT,
        hostname    TEXT,
        os          TEXT,
        tags        TEXT,                          -- JSON array
        workspace_ids TEXT,                        -- JSON array
        created_at  TEXT NOT NULL,
        updated_at  TEXT NOT NULL
    );

    CREATE TABLE groups (
        id   TEXT PRIMARY KEY,
        name TEXT NOT NULL
    );

    CREATE TABLE workspaces (
        id                  TEXT PRIMARY KEY,
        name                TEXT NOT NULL,
        server_ids          TEXT NOT NULL DEFAULT '[]',   -- JSON array
        repositories        TEXT NOT NULL DEFAULT '[]',   -- JSON array
        provider_ids        TEXT NOT NULL DEFAULT '[]',   -- JSON array
        default_environment TEXT NOT NULL CHECK (default_environment IN ('local','development','staging','production','unknown'))
    );

    CREATE TABLE identities (
        id            TEXT PRIMARY KEY,
        label         TEXT NOT NULL,
        method        TEXT NOT NULL CHECK (method IN ('password','privateKey','agent')),
        credential_ref TEXT NOT NULL,
        created_at    TEXT NOT NULL
    );
    -- References only: secret material lives in the OS keychain, never in SQLite.

    CREATE TABLE server_identities (
        server_id  TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
        identity_id TEXT NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
        PRIMARY KEY (server_id, identity_id)
    );

    CREATE TABLE snapshots (
        id          TEXT PRIMARY KEY,
        server_id   TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
        collected_at TEXT NOT NULL,
        health      TEXT NOT NULL CHECK (health IN ('healthy','warning','critical','unknown')),
        payload     TEXT NOT NULL   -- full JSON snapshot (camelCase)
    );
    CREATE INDEX idx_snapshots_server_time ON snapshots (server_id, collected_at DESC);

    CREATE TABLE services (
        id         TEXT PRIMARY KEY,
        server_id  TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
        kind       TEXT NOT NULL,
        name       TEXT NOT NULL,
        state      TEXT NOT NULL,
        status     TEXT,
        details    TEXT,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );

    CREATE TABLE activities (
        id           TEXT PRIMARY KEY,
        server_id    TEXT,
        workspace_id TEXT,
        type         TEXT NOT NULL CHECK (type IN ('connection','authentication','configuration','deployment','service','container','file_change','agent_action','approval','health')),
        title        TEXT NOT NULL,
        description  TEXT,
        source       TEXT NOT NULL CHECK (source IN ('agent','user','system','docker','git','cloud')),
        actor        TEXT NOT NULL,
        reason       TEXT,
        outcome      TEXT CHECK (outcome IN ('success','failure','cancelled','denied')),
        trace_id     TEXT,
        created_at   TEXT NOT NULL
    );
    CREATE INDEX idx_activities_created ON activities (created_at DESC);
    CREATE INDEX idx_activities_server ON activities (server_id);

    CREATE TABLE chat_sessions (
        id           TEXT PRIMARY KEY,
        workspace_id TEXT,
        server_id    TEXT,
        title        TEXT NOT NULL,
        created_at   TEXT NOT NULL,
        updated_at   TEXT NOT NULL
    );

    CREATE TABLE chat_messages (
        id         TEXT PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
        role       TEXT NOT NULL CHECK (role IN ('user','assistant','tool','system')),
        content    TEXT NOT NULL,
        trace_id   TEXT,
        created_at TEXT NOT NULL
    );
    CREATE INDEX idx_chat_messages_session ON chat_messages (session_id, created_at);

    CREATE TABLE tool_executions (
        trace_id    TEXT NOT NULL,
        step_id     TEXT NOT NULL,
        call_id     TEXT NOT NULL,
        tool_name   TEXT NOT NULL,
        server_id   TEXT,
        environment TEXT NOT NULL CHECK (environment IN ('local','development','staging','production','unknown')),
        risk_level  TEXT NOT NULL CHECK (risk_level IN ('read','low','medium','high','critical')),
        decision    TEXT NOT NULL CHECK (decision IN ('auto','ask','deny')),
        approved_by TEXT CHECK (approved_by IN ('user','policy')),
        status      TEXT NOT NULL CHECK (status IN ('pending','running','waiting_approval','success','failed','cancelled')),
        input       TEXT NOT NULL,   -- JSON
        output      TEXT,            -- JSON
        error       TEXT,
        started_at  TEXT NOT NULL,
        ended_at    TEXT,
        duration_ms INTEGER,
        PRIMARY KEY (trace_id, step_id)
    );
    CREATE INDEX idx_tool_executions_trace ON tool_executions (trace_id);
    CREATE INDEX idx_tool_executions_server ON tool_executions (server_id);

    CREATE TABLE provider_configs (
        id                    TEXT PRIMARY KEY,
        family                TEXT NOT NULL CHECK (family IN ('ai','infra')),
        kind                  TEXT NOT NULL,
        label                 TEXT NOT NULL,
        base_url              TEXT,
        model                 TEXT,
        api_key_credential_ref TEXT,
        credential_ref      TEXT,   -- infrastructure providers
        enabled               INTEGER NOT NULL DEFAULT 1,
        custom_headers        TEXT,
        max_input_tokens      INTEGER,
        settings              TEXT,
        created_at            TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
        updated_at            TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    );

    CREATE TABLE mcp_servers (
        id           TEXT PRIMARY KEY,
        label        TEXT NOT NULL,
        transport    TEXT NOT NULL CHECK (transport IN ('stdio','http')),
        command      TEXT,
        args         TEXT,   -- JSON array
        url          TEXT,
        enabled      INTEGER NOT NULL DEFAULT 1,
        allowed_tools TEXT NOT NULL DEFAULT '[]',   -- JSON array
        trust_level  TEXT NOT NULL CHECK (trust_level IN ('reviewed','unreviewed'))
    );
    "#,
];

const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// Apply pending migrations in order. Each migration runs in its own transaction;
/// `PRAGMA user_version` is bumped only after the statements succeed.
pub(crate) fn migrate(connection: &Connection) -> Result<()> {
    let current: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if current > SCHEMA_VERSION {
        return Err(DatabaseError::NewerSchema {
            schema: current,
            app: SCHEMA_VERSION,
        });
    }
    for (index, sql) in MIGRATIONS.iter().enumerate() {
        let version = (index + 1) as i64;
        if version <= current {
            continue;
        }
        let tx = connection.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
    }
    Ok(())
}
