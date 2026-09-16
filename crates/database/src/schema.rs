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
    // 2 — provider wire dialect (codex `responses` vs `chat` completions).
    r#"ALTER TABLE provider_configs ADD COLUMN wire_api TEXT NOT NULL DEFAULT 'chat';"#,
    // 3 — record explicit Agent delegation in the execution audit.
    r#"
    DROP INDEX IF EXISTS idx_tool_executions_trace;
    DROP INDEX IF EXISTS idx_tool_executions_server;
    ALTER TABLE tool_executions RENAME TO tool_executions_legacy;
    CREATE TABLE tool_executions (
        trace_id    TEXT NOT NULL,
        step_id     TEXT NOT NULL,
        call_id     TEXT NOT NULL,
        tool_name   TEXT NOT NULL,
        server_id   TEXT,
        environment TEXT NOT NULL CHECK (environment IN ('local','development','staging','production','unknown')),
        risk_level  TEXT NOT NULL CHECK (risk_level IN ('read','low','medium','high','critical')),
        decision    TEXT NOT NULL CHECK (decision IN ('auto','ask','deny')),
        approved_by TEXT CHECK (approved_by IN ('user','policy','agent')),
        status      TEXT NOT NULL CHECK (status IN ('pending','running','waiting_approval','success','failed','cancelled')),
        input       TEXT NOT NULL,
        output      TEXT,
        error       TEXT,
        started_at  TEXT NOT NULL,
        ended_at    TEXT,
        duration_ms INTEGER,
        PRIMARY KEY (trace_id, step_id)
    );
    INSERT INTO tool_executions (
        trace_id, step_id, call_id, tool_name, server_id, environment, risk_level,
        decision, approved_by, status, input, output, error, started_at, ended_at, duration_ms
    )
    SELECT trace_id, step_id, call_id, tool_name, server_id, environment, risk_level,
           decision, approved_by, status, input, output, error, started_at, ended_at, duration_ms
      FROM tool_executions_legacy;
    DROP TABLE tool_executions_legacy;
    CREATE INDEX idx_tool_executions_trace ON tool_executions (trace_id);
    CREATE INDEX idx_tool_executions_server ON tool_executions (server_id);
    "#,
    // 4 — archive state for durable Agent conversation history.
    r#"
    CREATE TABLE IF NOT EXISTS chat_sessions (
        id           TEXT PRIMARY KEY,
        workspace_id TEXT,
        server_id    TEXT,
        title        TEXT NOT NULL,
        created_at   TEXT NOT NULL,
        updated_at   TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS chat_messages (
        id         TEXT PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
        role       TEXT NOT NULL CHECK (role IN ('user','assistant','tool','system')),
        content    TEXT NOT NULL,
        trace_id   TEXT,
        created_at TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_chat_messages_session ON chat_messages (session_id, created_at);
    ALTER TABLE chat_sessions ADD COLUMN archived_at TEXT;
    CREATE INDEX idx_chat_sessions_updated ON chat_sessions (updated_at DESC);
    CREATE INDEX idx_chat_sessions_archived ON chat_sessions (archived_at, updated_at DESC);
    "#,
    // 5 — 加密私钥的口令引用。
    //
    // 与 `credential_ref` 同一条规则：SQLite 里只放**引用**，口令材料在 OS
    // keychain。之所以是独立的一列而不是把口令塞进 `credential_ref` 的条目里：
    // 一条条目解析失败就会连私钥一起丢掉，而且私钥条目的 account 必须保持旧版本
    // 代码读得懂。可空（`NULL` = 这个身份没有口令：明文 key / 密码 / ssh-agent），
    // 写法沿用迁移 2 的 `ALTER TABLE … ADD COLUMN`（追加列，不改写既有行）。
    r#"ALTER TABLE identities ADD COLUMN passphrase_ref TEXT;"#,
    // 6 — Anthropic's dated protocol header is user-configurable.
    r#"ALTER TABLE provider_configs ADD COLUMN api_version TEXT;"#,
    // 7 — OpenSSH user-certificate identities. SQLite cannot widen a CHECK constraint
    // in place, so both identity tables are rebuilt while preserving all rows.
    r#"
    ALTER TABLE server_identities RENAME TO server_identities_legacy;
    ALTER TABLE identities RENAME TO identities_legacy;

    CREATE TABLE identities (
        id            TEXT PRIMARY KEY,
        label         TEXT NOT NULL,
        method        TEXT NOT NULL CHECK (method IN ('password','privateKey','certificate','agent')),
        credential_ref TEXT NOT NULL,
        passphrase_ref TEXT,
        private_key_path TEXT,
        certificate_path TEXT,
        created_at    TEXT NOT NULL
    );

    INSERT INTO identities (
        id, label, method, credential_ref, passphrase_ref, private_key_path,
        certificate_path, created_at
    )
    SELECT
        id, label, method, credential_ref, passphrase_ref, NULL, NULL, created_at
      FROM identities_legacy;

    CREATE TABLE server_identities (
        server_id  TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
        identity_id TEXT NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
        PRIMARY KEY (server_id, identity_id)
    );

    INSERT INTO server_identities (server_id, identity_id)
    SELECT server_id, identity_id FROM server_identities_legacy;

    DROP TABLE server_identities_legacy;
    DROP TABLE identities_legacy;
    "#,
    // 8 鈥?durable multimodal prompt parts for a user message. Keeping the
    // already-validated JSON intact avoids a second, lossy schema for image data.
    r#"ALTER TABLE chat_messages ADD COLUMN parts_json TEXT;"#,
    // 9 鈥?optional explicit OpenSSH host-certificate trust root. Both values are
    // public: the CA public key and the allowed host-principal patterns.
    r#"
    ALTER TABLE servers ADD COLUMN host_ca_public_key TEXT;
    ALTER TABLE servers ADD COLUMN host_principals TEXT;
    "#,
    // 10 — optional local OpenSSH KRL for host-certificate revocation.
    r#"ALTER TABLE servers ADD COLUMN host_krl_path TEXT;"#,
    // 11 — optional static HTTP authentication for MCP. The value stays in keychain.
    r#"
    ALTER TABLE mcp_servers ADD COLUMN http_auth_header TEXT;
    ALTER TABLE mcp_servers ADD COLUMN http_credential_ref TEXT;
    "#,
    // 12 — optional online OpenSSH KRL source for host certificate revocation.
    r#"ALTER TABLE servers ADD COLUMN host_krl_url TEXT;"#,
    // 13 — ordered multiple static HTTP authentication headers for MCP.
    // Names are public; credential references stay opaque and point at the OS
    // credential store. The legacy single-header columns remain readable.
    r#"ALTER TABLE mcp_servers ADD COLUMN http_auth_headers TEXT;"#,
    // 14 — OAuth discovery/client metadata and the opaque credential-store
    // reference for the refreshable token bundle. Access/refresh tokens never
    // enter SQLite.
    r#"ALTER TABLE mcp_servers ADD COLUMN oauth TEXT;"#,
    // 15 — public signing keys trusted for host-certificate KRLs, independent
    // of the host CA. Multiple keys support rotation without a flag day.
    r#"ALTER TABLE servers ADD COLUMN host_krl_signers TEXT;"#,
    // 16 — application-level network settings (ADR 0022): one row per setting, JSON
    // value. The proxy credential itself stays in the OS credential store; this table
    // only ever holds its reference.
    r#"
    CREATE TABLE app_settings (
        key        TEXT PRIMARY KEY,
        value      TEXT NOT NULL,   -- JSON
        updated_at TEXT NOT NULL
    );
    "#,
    // 17 — retryable cleanup for credentials whose configuration row is already gone.
    // Only opaque references are stored; secret material never enters SQLite.
    r#"
    CREATE TABLE credential_cleanup_queue (
        reference  TEXT PRIMARY KEY,
        created_at TEXT NOT NULL,
        attempts   INTEGER NOT NULL DEFAULT 0,
        last_error TEXT NOT NULL
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
