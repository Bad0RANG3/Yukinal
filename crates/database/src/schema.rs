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
    // 18 — durable investigation tasks and evidence-backed decision material.
    // Evidence is bounded and already redacted before it reaches this layer.
    r#"
    CREATE TABLE investigation_tasks (
        id                TEXT PRIMARY KEY,
        workspace_id      TEXT,
        server_id         TEXT,
        objective         TEXT NOT NULL,
        success_criteria  TEXT NOT NULL, -- JSON array
        scope             TEXT NOT NULL, -- JSON ToolTarget
        mode              TEXT NOT NULL CHECK (mode IN ('goal','plan','readonly')),
        permission_mode   TEXT NOT NULL CHECK (permission_mode IN ('ask','auto')),
        automation_level  TEXT NOT NULL CHECK (automation_level IN ('readonly','propose','execute')),
        status            TEXT NOT NULL CHECK (status IN ('pending','investigating','waiting_user','executing','verifying','completed','failed','stopped','expired')),
        max_steps         INTEGER NOT NULL CHECK (max_steps > 0),
        max_run_ms        INTEGER NOT NULL CHECK (max_run_ms > 0),
        created_at        TEXT NOT NULL,
        updated_at        TEXT NOT NULL,
        completed_at      TEXT
    );
    CREATE INDEX idx_investigation_tasks_status ON investigation_tasks (status, updated_at DESC);
    CREATE INDEX idx_investigation_tasks_server ON investigation_tasks (server_id, updated_at DESC);

    CREATE TABLE investigation_evidence (
        id                TEXT PRIMARY KEY,
        task_id           TEXT NOT NULL REFERENCES investigation_tasks(id) ON DELETE CASCADE,
        scope             TEXT NOT NULL, -- JSON ToolTarget
        kind              TEXT NOT NULL CHECK (kind IN ('snapshot','log','service','container','file','tool_result','failure')),
        source_tool       TEXT NOT NULL,
        collected_at      TEXT NOT NULL,
        input_summary     TEXT NOT NULL,
        content_type      TEXT NOT NULL CHECK (content_type IN ('json','text')),
        content           TEXT NOT NULL, -- bounded JSON value
        content_hash      TEXT NOT NULL CHECK (length(content_hash) = 64),
        truncated         INTEGER NOT NULL CHECK (truncated IN (0,1)),
        redaction_status  TEXT NOT NULL CHECK (redaction_status IN ('clean','redacted','unknown'))
    );
    CREATE INDEX idx_investigation_evidence_task ON investigation_evidence (task_id, collected_at DESC);

    CREATE TABLE investigation_findings (
        id                TEXT PRIMARY KEY,
        task_id           TEXT NOT NULL REFERENCES investigation_tasks(id) ON DELETE CASCADE,
        title             TEXT NOT NULL,
        kind              TEXT NOT NULL CHECK (kind IN ('fact','inference','unknown')),
        statement         TEXT NOT NULL,
        evidence_ids      TEXT NOT NULL, -- JSON array
        confidence         TEXT NOT NULL CHECK (confidence IN ('high','medium','low')),
        next_verification TEXT,
        created_at        TEXT NOT NULL
    );
    CREATE INDEX idx_investigation_findings_task ON investigation_findings (task_id, created_at DESC);

    CREATE TABLE investigation_decision_briefs (
        id                  TEXT PRIMARY KEY,
        task_id             TEXT NOT NULL REFERENCES investigation_tasks(id) ON DELETE CASCADE,
        generated_at        TEXT NOT NULL,
        status              TEXT NOT NULL CHECK (status IN ('draft','presented','selected','dismissed')),
        finding_ids         TEXT NOT NULL, -- JSON array
        options             TEXT NOT NULL, -- JSON array of DecisionOption
        selected_option_id  TEXT
    );
    CREATE INDEX idx_investigation_briefs_task ON investigation_decision_briefs (task_id, generated_at DESC);
    "#,
    // 19 — durable task attempts, checkpoints and bounded step outcomes. A sidecar run is
    // not the task itself: these rows let the host distinguish a retry, an interruption and a
    // completed objective after the sidecar or UI has been restarted.
    r#"
    ALTER TABLE investigation_tasks ADD COLUMN max_attempts INTEGER NOT NULL DEFAULT 3 CHECK (max_attempts > 0);
    ALTER TABLE investigation_tasks ADD COLUMN created_by TEXT NOT NULL DEFAULT 'user';
    ALTER TABLE investigation_tasks ADD COLUMN phase TEXT NOT NULL DEFAULT 'investigating'
        CHECK (phase IN ('investigating','decision','execution','verification','recovery','completed'));
    ALTER TABLE investigation_tasks ADD COLUMN active_run_id TEXT;
    ALTER TABLE investigation_tasks ADD COLUMN last_failure TEXT;

    CREATE TABLE investigation_runs (
        id            TEXT PRIMARY KEY,
        task_id       TEXT NOT NULL REFERENCES investigation_tasks(id) ON DELETE CASCADE,
        session_id    TEXT,
        message_id    TEXT,
        trace_id      TEXT,
        attempt       INTEGER NOT NULL CHECK (attempt > 0),
        phase         TEXT NOT NULL CHECK (phase IN ('investigating','decision','execution','verification','recovery','completed')),
        status        TEXT NOT NULL CHECK (status IN ('admitted','running','waiting_user','completed','failed','cancelled','interrupted')),
        started_at    TEXT NOT NULL,
        updated_at    TEXT NOT NULL,
        ended_at      TEXT,
        checkpoint    TEXT,
        failure       TEXT
    );
    CREATE INDEX idx_investigation_runs_task ON investigation_runs (task_id, updated_at DESC);
    CREATE INDEX idx_investigation_runs_active ON investigation_runs (task_id, status);

    CREATE TABLE investigation_steps (
        id              TEXT PRIMARY KEY,
        task_id         TEXT NOT NULL REFERENCES investigation_tasks(id) ON DELETE CASCADE,
        run_id          TEXT NOT NULL REFERENCES investigation_runs(id) ON DELETE CASCADE,
        ordinal         INTEGER NOT NULL CHECK (ordinal >= 0),
        kind            TEXT NOT NULL CHECK (kind IN ('plan','evidence','decision','action','verification','recovery')),
        title           TEXT NOT NULL,
        status          TEXT NOT NULL CHECK (status IN ('pending','running','waiting_user','succeeded','failed','skipped')),
        attempt         INTEGER NOT NULL CHECK (attempt > 0),
        tool_name       TEXT,
        target          TEXT,
        input_summary   TEXT,
        output_summary  TEXT,
        evidence_ids    TEXT NOT NULL,
        started_at      TEXT,
        ended_at        TEXT,
        failure         TEXT
    );
    CREATE INDEX idx_investigation_steps_task ON investigation_steps (task_id, ordinal, id);
    CREATE INDEX idx_investigation_steps_run ON investigation_steps (run_id, ordinal, id);
    "#,
    // 20 — versioned task plans. The host compares every task tool call with the active
    // revision before execution; old revisions remain visible as superseded audit records.
    r#"
    CREATE TABLE investigation_plans (
        id              TEXT PRIMARY KEY,
        task_id         TEXT NOT NULL REFERENCES investigation_tasks(id) ON DELETE CASCADE,
        revision        INTEGER NOT NULL CHECK (revision > 0),
        status          TEXT NOT NULL CHECK (status IN ('draft','active','superseded','completed')),
        created_at      TEXT NOT NULL,
        updated_at      TEXT NOT NULL,
        current_step_id TEXT,
        steps           TEXT NOT NULL
    );
    CREATE INDEX idx_investigation_plans_task ON investigation_plans (task_id, revision DESC, id DESC);
    CREATE UNIQUE INDEX idx_investigation_plans_active ON investigation_plans (task_id)
        WHERE status IN ('draft','active');
    "#,
    // 21 — bind observed tool steps to the host-validated plan revision and step.
    r#"
    ALTER TABLE investigation_steps ADD COLUMN plan_id TEXT;
    ALTER TABLE investigation_steps ADD COLUMN plan_step_id TEXT;
    CREATE INDEX idx_investigation_steps_plan ON investigation_steps (plan_id, plan_step_id);
    "#,
    // 22 — durable phase artifacts. Execution, verification and failure reports are
    // task-owned records rather than transient model text, so a reopened task can
    // explain what was attempted and what remains undecided.
    r#"
    CREATE TABLE investigation_artifacts (
        id          TEXT PRIMARY KEY,
        task_id     TEXT NOT NULL REFERENCES investigation_tasks(id) ON DELETE CASCADE,
        run_id      TEXT REFERENCES investigation_runs(id) ON DELETE SET NULL,
        phase       TEXT NOT NULL CHECK (phase IN ('investigating','decision','execution','verification','recovery','completed')),
        kind        TEXT NOT NULL CHECK (kind IN ('investigation_plan','baseline','evidence_set','decision_brief','change_plan','execution','verification','failure')),
        status      TEXT NOT NULL CHECK (status IN ('draft','ready','succeeded','failed','superseded')),
        title       TEXT NOT NULL,
        summary     TEXT NOT NULL,
        content     TEXT NOT NULL,
        evidence_ids TEXT NOT NULL,
        created_at  TEXT NOT NULL,
        updated_at  TEXT NOT NULL
    );
    CREATE INDEX idx_investigation_artifacts_task ON investigation_artifacts (task_id, updated_at DESC, id DESC);
    CREATE INDEX idx_investigation_artifacts_run ON investigation_artifacts (run_id, updated_at DESC);
    "#,
    // 23 — bind artifacts to the host-validated plan revision and step that produced them.
    // This lets a required baseline be rejected when it belongs to an older plan.
    r#"
    ALTER TABLE investigation_artifacts ADD COLUMN plan_id TEXT;
    ALTER TABLE investigation_artifacts ADD COLUMN plan_step_id TEXT;
    CREATE INDEX idx_investigation_artifacts_plan ON investigation_artifacts (task_id, plan_id, kind, updated_at DESC);
    "#,
    // 24 — bind a decision brief to the plan revision it explains. A proposal-mode
    // action may only be unlocked by selecting an option from the current revision.
    r#"
    ALTER TABLE investigation_decision_briefs ADD COLUMN plan_id TEXT;
    CREATE INDEX idx_investigation_briefs_plan ON investigation_decision_briefs (task_id, plan_id, generated_at DESC);
    "#,
    // 25 — persist the user/policy approval that unlocks a proposal-mode plan.
    r#"
    ALTER TABLE investigation_plans ADD COLUMN approval TEXT;
    "#,
    // 26 — host-owned post-change observation state. The plan keeps the bounded
    // configuration and sample ledger together so reopening the app cannot skip the
    // observation window or infer completion from a transient Agent response.
    r#"
    ALTER TABLE investigation_plans ADD COLUMN observation_window TEXT;
    "#,
    // 27 — durable local read-only schedule triggers and their claimed runs. A
    // schedule references a task only; provider credentials and execution handles stay
    // outside SQLite, and the unique dedupe key makes retries/restarts idempotent.
    r#"
    CREATE TABLE investigation_schedules (
        id                    TEXT PRIMARY KEY,
        task_id               TEXT NOT NULL REFERENCES investigation_tasks(id) ON DELETE CASCADE,
        status                TEXT NOT NULL CHECK (status IN ('active','paused','revoked')),
        interval_seconds      INTEGER NOT NULL CHECK (interval_seconds > 0),
        cooldown_seconds      INTEGER NOT NULL CHECK (cooldown_seconds >= 0),
        dedupe_window_seconds INTEGER NOT NULL CHECK (dedupe_window_seconds > 0),
        max_concurrent_runs  INTEGER NOT NULL CHECK (max_concurrent_runs > 0),
        budget                TEXT NOT NULL,
        notification_policy   TEXT NOT NULL CHECK (notification_policy IN ('silent','on_change','always','failed_runs_only')),
        next_run_at           TEXT NOT NULL,
        last_run_at           TEXT,
        last_outcome          TEXT,
        last_error            TEXT,
        created_at            TEXT NOT NULL,
        updated_at            TEXT NOT NULL
    );
    CREATE INDEX idx_investigation_schedules_due ON investigation_schedules (status, next_run_at, id);
    CREATE INDEX idx_investigation_schedules_task ON investigation_schedules (task_id, status);

    CREATE TABLE investigation_schedule_runs (
        id            TEXT PRIMARY KEY,
        schedule_id   TEXT NOT NULL REFERENCES investigation_schedules(id) ON DELETE CASCADE,
        task_id       TEXT NOT NULL REFERENCES investigation_tasks(id) ON DELETE CASCADE,
        status        TEXT NOT NULL CHECK (status IN ('queued','claimed','running','succeeded','failed','skipped','interrupted')),
        scheduled_at  TEXT NOT NULL,
        claimed_at    TEXT,
        started_at    TEXT,
        finished_at   TEXT,
        dedupe_key    TEXT NOT NULL,
        outcome       TEXT,
        error         TEXT
    );
    CREATE UNIQUE INDEX idx_investigation_schedule_runs_dedupe
        ON investigation_schedule_runs (schedule_id, dedupe_key);
    CREATE INDEX idx_investigation_schedule_runs_schedule
        ON investigation_schedule_runs (schedule_id, scheduled_at DESC, id DESC);
    CREATE INDEX idx_investigation_schedule_runs_active
        ON investigation_schedule_runs (schedule_id, status);
    "#,
    // 28 — bind evidence to the host-owned investigation run that collected it. The
    // sidecar may suggest a value, but the host overwrites it from the active run before
    // persistence; this makes scheduled samples comparable without trusting model text.
    r#"
    ALTER TABLE investigation_evidence ADD COLUMN run_id TEXT REFERENCES investigation_runs(id) ON DELETE SET NULL;
    CREATE INDEX idx_investigation_evidence_run ON investigation_evidence (run_id, collected_at DESC, id DESC);
    "#,
    // 29 — host-owned action idempotency ledger. A sidecar may resend the same
    // call after losing a response (or after its own restart), but an unsafe
    // remote action must never be run twice. The response is deliberately kept
    // out of SQLite: it can contain file contents or third-party tool output.
    // The live host keeps a bounded replay cache; after a desktop restart the
    // ledger fails closed and asks for reconciliation instead of guessing.
    r#"
    CREATE TABLE host_tool_calls (
        trace_id            TEXT NOT NULL,
        call_id             TEXT NOT NULL,
        task_id             TEXT,
        plan_id             TEXT,
        plan_step_id        TEXT,
        tool_name           TEXT NOT NULL,
        request_fingerprint TEXT NOT NULL,
        status              TEXT NOT NULL CHECK (status IN ('running','success','failed','cancelled','uncertain')),
        started_at          TEXT NOT NULL,
        ended_at            TEXT,
        PRIMARY KEY (trace_id, call_id)
    );
    CREATE INDEX idx_host_tool_calls_task ON host_tool_calls (task_id, started_at DESC);
    CREATE INDEX idx_host_tool_calls_status ON host_tool_calls (status, started_at DESC);
    "#,
    // 30 — action fingerprints let a resumed durable plan reject the same
    // logical write even when a restarted sidecar generates a new call ID.
    // Rows created by migration 29 remain exact-call protected; nullable is
    // intentional for those historical rows.
    r#"
    ALTER TABLE host_tool_calls ADD COLUMN action_fingerprint TEXT;
    CREATE INDEX idx_host_tool_calls_action
        ON host_tool_calls (task_id, plan_id, plan_step_id, action_fingerprint, status);
    "#,
    // 31 — host-owned remote filesystem backup ledger. The bytes stay on the
    // target server; SQLite stores only enough metadata to prove that a later
    // restore refers to a backup this host created for the same target. This
    // also gives a future cleanup pass a durable, non-arbitrary inventory.
    r#"
    CREATE TABLE filesystem_backups (
        id              TEXT PRIMARY KEY,
        server_id       TEXT NOT NULL,
        task_id         TEXT,
        plan_id         TEXT,
        plan_step_id    TEXT,
        trace_id        TEXT,
        call_id         TEXT,
        path            TEXT NOT NULL,
        backup_path     TEXT NOT NULL,
        revision        TEXT NOT NULL CHECK (length(revision) = 64),
        bytes_backed_up INTEGER NOT NULL CHECK (bytes_backed_up >= 0),
        status          TEXT NOT NULL CHECK (status IN ('available','restored','deleted')),
        created_at      TEXT NOT NULL,
        updated_at      TEXT NOT NULL,
        restored_at     TEXT,
        deleted_at      TEXT,
        UNIQUE (server_id, backup_path)
    );
    CREATE INDEX idx_filesystem_backups_target
        ON filesystem_backups (server_id, path, status, created_at DESC);
    CREATE INDEX idx_filesystem_backups_task
        ON filesystem_backups (task_id, created_at DESC);
    "#,
    // 32 — evidence search is task-scoped and commonly filtered by source plus
    // collection time. Keep the metadata query bounded without scanning bodies.
    r#"
    CREATE INDEX idx_investigation_evidence_source_time
        ON investigation_evidence (task_id, source_tool, collected_at DESC, id DESC);
    "#,
    // 33 — host-enforced task guardrails. The JSON object is intentionally
    // append-only metadata so older task rows decode to empty boundaries.
    r#"
    ALTER TABLE investigation_tasks ADD COLUMN guardrails TEXT NOT NULL DEFAULT '{}';
    "#,
    // 34 — an explicit successful schedule run may be selected as the
    // comparison baseline. The reference stays optional so existing schedules
    // keep the previous "latest successful run" behavior after migration.
    r#"
    ALTER TABLE investigation_schedules
        ADD COLUMN baseline_run_id TEXT REFERENCES investigation_schedule_runs(id) ON DELETE SET NULL;
    CREATE INDEX idx_investigation_schedules_baseline
        ON investigation_schedules (baseline_run_id);
    "#,
    // 35 — per-server trust of MCP tool annotations (ADR 0074). Default 'none' so
    // existing rows and unknown peers keep the pre-relaxation behaviour: every MCP
    // tool stays critical until a user explicitly trusts that one server.
    r#"ALTER TABLE mcp_servers ADD COLUMN annotation_trust TEXT NOT NULL DEFAULT 'none';"#,
];

const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// Highest schema version this build can open. Used by the pre-migration backup
/// manifest to name the upgrade target it is about to attempt.
pub(crate) fn supported_version() -> i64 {
    SCHEMA_VERSION
}

/// The on-disk schema version, read without applying anything.
pub(crate) fn on_disk_version(connection: &Connection) -> Result<i64> {
    Ok(connection.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

/// Whether opening this database would apply at least one migration.
pub(crate) fn has_pending(connection: &Connection) -> Result<bool> {
    Ok(on_disk_version(connection)? < SCHEMA_VERSION)
}

/// Apply pending migrations in order. Each migration runs in its own transaction;
/// `PRAGMA user_version` is bumped only after the statements succeed.
///
/// A failure is reported as [`DatabaseError::Migration`] naming the version, so a
/// half-upgraded database is attributable to one migration rather than "sqlite
/// error". The caller is responsible for the pre-migration copy and rollback
/// (see [`crate::migration`]); this function only guarantees the transaction
/// boundary.
pub(crate) fn migrate(connection: &Connection) -> Result<()> {
    let current = on_disk_version(connection)?;
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
        apply_migration(connection, version, sql).map_err(|error| DatabaseError::Migration {
            version,
            message: error.to_string(),
        })?;
    }
    Ok(())
}

fn apply_migration(connection: &Connection, version: i64, sql: &str) -> Result<()> {
    let tx = connection.unchecked_transaction()?;
    tx.execute_batch(sql)?;
    tx.pragma_update(None, "user_version", version)?;
    tx.commit()?;
    Ok(())
}
