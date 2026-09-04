//! Acceptance: restart-persistence for servers/workspaces/provider_configs, write+read
//! for tool_executions, and the camelCase wire shape of the row structs.

use std::path::PathBuf;

use serde_json::json;
use yukinal_database::models::{
    Activity, ActivitySource, ActivityType, AiProviderConfig, AiProviderKind, Environment,
    Identity, InfrastructureProviderConfig, McpServerConfig, PermissionMode, RiskLevel, Server,
    ServerCapabilities, ServerConnection, ServerMetadata, ServerSnapshot, ServerStatus,
    ToolExecutionRecord, ToolExecutionStatus, Workspace, WorkspaceRepository,
};
use yukinal_database::{Database, DatabaseError};

fn sample_server(id: &str) -> Server {
    Server {
        id: id.to_string(),
        name: "Production API".into(),
        connection: ServerConnection {
            host: "api.example.com".into(),
            port: 22,
            username: "deploy".into(),
            identity_id: None,
        },
        group_id: None,
        capabilities: ServerCapabilities {
            linux: Some(true),
            docker: Some(true),
            ..Default::default()
        },
        status: ServerStatus::Connected,
        metadata: ServerMetadata {
            environment: Environment::Production,
            region: Some("Singapore".into()),
            hostname: None,
            os: None,
            tags: Some(vec!["api".into()]),
            workspace_ids: None,
        },
        created_at: "2026-01-01T00:00:00.000Z".into(),
        updated_at: "2026-01-01T00:00:00.000Z".into(),
    }
}

fn sample_snapshot(id: &str, server_id: &str, collected_at: &str) -> ServerSnapshot {
    ServerSnapshot {
        id: id.into(),
        server_id: server_id.into(),
        collected_at: collected_at.into(),
        health: yukinal_database::models::HealthState::Healthy,
        os: Some(
            json!({ "distribution": "Ubuntu", "version": "24.04", "hostname": "api-01", "kernel": "6.8.0", "arch": "x86_64" }),
        ),
        cpu: Some(
            json!({ "model": "Xeon", "cores": 8, "usagePercent": 23.5, "loadAverage": [1.2, 0.9, 0.7] }),
        ),
        memory: None,
        disks: None,
        uptime_seconds: Some(172_800),
        network: None,
        docker: None,
        capabilities: ServerCapabilities {
            linux: Some(true),
            docker: Some(true),
            ..Default::default()
        },
        collectors: None,
    }
}

fn temp_db(tag: &str) -> (PathBuf, Database) {
    let path = std::env::temp_dir().join(format!("yukinal-db-{tag}-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    let db = Database::open(&path).expect("open temp database");
    (path, db)
}

fn cleanup(path: &PathBuf) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}

// ---------------------------------------------------------------------------
// server rows

#[test]
fn server_row_serialises_camel_case_like_the_ipc_contract() {
    let value = serde_json::to_value(sample_server("srv_01abc")).expect("serialize");
    assert_eq!(
        value,
        json!({
            "id": "srv_01abc",
            "name": "Production API",
            "connection": { "host": "api.example.com", "port": 22, "username": "deploy" },
            "capabilities": { "linux": true, "docker": true },
            "status": "connected",
            "metadata": {
                "environment": "production",
                "region": "Singapore",
                "tags": ["api"]
            },
            "createdAt": "2026-01-01T00:00:00.000Z",
            "updatedAt": "2026-01-01T00:00:00.000Z"
        })
    );
}

#[test]
fn server_round_trip_update_delete() {
    let (_path, db) = temp_db("srv");
    let repo = db.servers();
    repo.insert(&sample_server("srv_a")).expect("insert");
    repo.insert(&sample_server("srv_b")).expect("insert");

    let got = repo.get("srv_a").expect("get");
    assert_eq!(got.name, "Production API");
    assert_eq!(got.metadata.region.as_deref(), Some("Singapore"));
    assert_eq!(got.status, ServerStatus::Connected);

    let listed = repo.list().expect("list");
    assert_eq!(listed.len(), 2);

    let mut renamed = sample_server("srv_a");
    renamed.name = "Renamed".into();
    repo.update(&renamed).expect("update");
    assert_eq!(repo.get("srv_a").expect("get").name, "Renamed");

    repo.set_status(
        "srv_a",
        ServerStatus::Disconnected,
        "2026-01-03T00:00:00.000Z",
    )
    .expect("set status");
    let disconnected = repo.get("srv_a").expect("get status");
    assert_eq!(disconnected.status, ServerStatus::Disconnected);
    assert_eq!(disconnected.updated_at, "2026-01-03T00:00:00.000Z");

    repo.delete("srv_a").expect("delete");
    assert!(matches!(repo.get("srv_a"), Err(DatabaseError::NotFound)));
}

#[test]
fn server_survives_reopen() {
    let (path, db) = temp_db("srv-persist");
    db.servers()
        .insert(&sample_server("srv_keep"))
        .expect("insert");
    drop(db);

    let reopened = Database::open(&path).expect("reopen");
    let got = reopened
        .servers()
        .get("srv_keep")
        .expect("get after reopen");
    assert_eq!(got.connection.host, "api.example.com");
    assert_eq!(got.status, ServerStatus::Connected);
    // WAL files are flushed on close; nothing left behind after drop.
    cleanup(&path);
}

// ---------------------------------------------------------------------------
// workspaces

fn sample_workspace(id: &str) -> Workspace {
    Workspace {
        id: id.into(),
        name: "E-commerce Production".into(),
        server_ids: vec!["srv_01abc".into()],
        repositories: vec![WorkspaceRepository {
            id: "repo_1".into(),
            name: "api".into(),
            host: "remote".into(),
            path: Some("/srv/api".into()),
            server_id: Some("srv_01abc".into()),
            git_url: None,
            default_branch: Some("main".into()),
        }],
        provider_ids: vec!["prv_1".into()],
        default_environment: Environment::Production,
    }
}

#[test]
fn workspace_round_trip_and_persistence() {
    let (path, db) = temp_db("ws");
    db.workspaces()
        .insert(&sample_workspace("ws_1"))
        .expect("insert");
    drop(db);

    let reopened = Database::open(&path).expect("reopen");
    let got = reopened.workspaces().get("ws_1").expect("get");
    assert_eq!(got.name, "E-commerce Production");
    assert_eq!(got.repositories.len(), 1);
    assert_eq!(got.repositories[0].path.as_deref(), Some("/srv/api"));
    assert_eq!(got.default_environment, Environment::Production);
    assert_eq!(reopened.workspaces().list().expect("list").len(), 1);
    cleanup(&path);
}

// ---------------------------------------------------------------------------
// provider configs

fn sample_ai_provider(id: &str) -> AiProviderConfig {
    AiProviderConfig {
        id: id.into(),
        kind: AiProviderKind::OpenaiCompatible,
        label: "OpenRouter".into(),
        base_url: "https://openrouter.ai/api/v1".into(),
        model: "anthropic/claude-sonnet".into(),
        api_key_credential_ref: Some("keychain://openrouter".into()),
        enabled: true,
        custom_headers: None,
        max_input_tokens: Some(200_000),
        wire_api: "chat".into(),
        models: None,
        created_at: "2026-01-01T00:00:00.000Z".into(),
        updated_at: "2026-01-01T00:00:00.000Z".into(),
    }
}

fn sample_infra_provider(id: &str) -> InfrastructureProviderConfig {
    InfrastructureProviderConfig {
        id: id.into(),
        kind: "github".into(),
        label: "GitHub".into(),
        credential_ref: Some("keychain://github".into()),
        enabled: true,
        settings: None,
    }
}

#[test]
fn provider_configs_round_trip_and_persistence() {
    let (path, db) = temp_db("prv");
    let repo = db.providers();
    repo.upsert_ai(&sample_ai_provider("prv_ai"))
        .expect("upsert ai");
    repo.upsert_infra(&sample_infra_provider("prv_infra"))
        .expect("upsert infra");
    drop(db);

    let reopened = Database::open(&path).expect("reopen");
    let ai = reopened.providers().get_ai("prv_ai").expect("get ai");
    assert_eq!(ai.base_url, "https://openrouter.ai/api/v1");
    assert_eq!(
        ai.api_key_credential_ref.as_deref(),
        Some("keychain://openrouter")
    );
    assert_eq!(
        reopened.providers().list_infra().expect("list infra").len(),
        1
    );
    assert_eq!(reopened.providers().list_all().expect("list all").len(), 2);
    cleanup(&path);
}

#[test]
fn provider_ai_upsert_is_idempotent() {
    let (_path, db) = temp_db("prv-upsert");
    let repo = db.providers();
    repo.upsert_ai(&sample_ai_provider("prv_ai"))
        .expect("upsert 1");
    let mut edited = sample_ai_provider("prv_ai");
    edited.enabled = false;
    repo.upsert_ai(&edited).expect("upsert 2");
    let got = repo.get_ai("prv_ai").expect("get");
    assert!(!got.enabled);
    assert_eq!(repo.list_ai().expect("list ai").len(), 1);
}

// ---------------------------------------------------------------------------
// tool executions

fn sample_execution(trace: &str, step: u32, second: u32) -> ToolExecutionRecord {
    ToolExecutionRecord {
        trace_id: trace.into(),
        step_id: format!("step_{step}"),
        call_id: format!("call_{trace}_{step}"),
        tool_name: "ssh.execute".into(),
        server_id: Some("srv_01abc".into()),
        environment: Environment::Production,
        risk_level: RiskLevel::Medium,
        decision: PermissionMode::Auto,
        approved_by: Some("policy".into()),
        status: ToolExecutionStatus::Success,
        input: json!({ "command": "uptime" }),
        output: Some(json!({ "stdout": "load 0.1" })),
        error: None,
        started_at: format!("2026-01-01T00:00:{second:02}Z"),
        ended_at: Some(format!("2026-01-01T00:00:{:02}Z", second + 1)),
        duration_ms: Some(900),
    }
}

#[test]
fn tool_executions_write_and_read() {
    let (_path, db) = temp_db("exec");
    let repo = db.executions();
    repo.insert(&sample_execution("trc_1", 1, 1))
        .expect("insert 1");
    repo.insert(&sample_execution("trc_1", 2, 2))
        .expect("insert 2");
    repo.insert(&sample_execution("trc_2", 1, 3))
        .expect("insert 3");

    let trace = repo.list_for_trace("trc_1").expect("by trace");
    assert_eq!(trace.len(), 2);
    assert_eq!(trace[0].step_id, "step_1");
    assert_eq!(
        trace[0].output.as_ref().expect("output")["stdout"],
        "load 0.1"
    );

    let recent = repo.list_recent(2).expect("recent");
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].trace_id, "trc_2");

    let server = repo
        .list_recent_for_server("srv_01abc", 2)
        .expect("by server");
    assert_eq!(server.len(), 2);
    assert_eq!(server[0].trace_id, "trc_2");
}

// ---------------------------------------------------------------------------
// identities + server_identities + cascades

#[test]
fn identity_attach_and_cascade() {
    let (_path, db) = temp_db("idn");
    db.servers()
        .insert(&sample_server("srv_a"))
        .expect("insert server");
    let repo = db.identities();
    repo.insert(&Identity {
        id: "idn_1".into(),
        label: "deploy key".into(),
        method: "privateKey".into(),
        credential_ref: "keychain://ssh/deploy".into(),
        created_at: "2026-01-01T00:00:00.000Z".into(),
    })
    .expect("insert identity");

    repo.attach_to_server("srv_a", "idn_1").expect("attach");
    assert_eq!(
        repo.ids_for_server("srv_a").expect("list"),
        vec!["idn_1".to_string()]
    );

    // Deleting the server reclaims the join rows via cascade...
    db.servers().delete("srv_a").expect("delete server");
    assert!(repo
        .ids_for_server("srv_a")
        .expect("after delete")
        .is_empty());

    // ...and deleting an unattached identity keeps no dangling SQLite rows.
    repo.delete("idn_1").expect("delete identity");
    assert!(matches!(repo.get("idn_1"), Err(DatabaseError::NotFound)));
}

// ---------------------------------------------------------------------------
// snapshots

#[test]
fn snapshot_latest_and_recent_ordering() {
    let (_path, db) = temp_db("snap");
    db.servers()
        .insert(&sample_server("srv_a"))
        .expect("insert server");

    let older = sample_snapshot("snap_01", "srv_a", "2026-01-01T00:00:00.000Z");
    let newer = sample_snapshot("snap_02", "srv_a", "2026-01-02T00:00:00.000Z");

    let snapshots = db.snapshots();
    snapshots.insert(&older).expect("insert older");
    snapshots.insert(&newer).expect("insert newer");

    let latest = snapshots.latest("srv_a").expect("latest").expect("row");
    assert_eq!(latest.collected_at, "2026-01-02T00:00:00.000Z");

    let recent = snapshots.list_recent("srv_a", 10).expect("recent");
    assert_eq!(recent.len(), 2);
    // oldest-last
    assert_eq!(recent[0].collected_at, "2026-01-01T00:00:00.000Z");
}

// ---------------------------------------------------------------------------
// schema versioning

#[test]
fn migrations_are_idempotent_and_future_schemas_are_refused() {
    let (path, db) = temp_db("mig");
    // Applying again on a fresh connection is a no-op, not a second migration.
    drop(db);
    let _reopened = Database::open(&path).expect("reopen applies nothing");

    // Simulate a future (newer) binary having touched this file.
    let wired = rusqlite::Connection::open(&path).expect("open raw");
    wired
        .pragma_update(None, "user_version", 999)
        .expect("bump version");
    drop(wired);

    let err = Database::open(&path).expect_err("newer schema must be refused");
    assert!(matches!(err, DatabaseError::NewerSchema { .. }));
    cleanup(&path);
}

// ---------------------------------------------------------------------------
// activities (audit stream)

#[test]
fn activities_round_trip() {
    let (_path, db) = temp_db("act");
    let repo = db.activities();
    repo.insert(&Activity {
        id: "act_1".into(),
        server_id: Some("srv_a".into()),
        workspace_id: None,
        r#type: ActivityType::Connection,
        title: "Connected".into(),
        description: None,
        source: ActivitySource::System,
        actor: "core".into(),
        reason: None,
        outcome: Some(yukinal_database::models::ActivityOutcome::Success),
        trace_id: None,
        created_at: "2026-01-01T00:00:00.000Z".into(),
    })
    .expect("insert activity");

    let feed = repo.list_recent(10).expect("list");
    assert_eq!(feed.len(), 1);
    assert_eq!(feed[0].title, "Connected");
    assert_eq!(feed[0].source, ActivitySource::System);
}

#[test]
fn activities_can_be_filtered_by_server_and_limited() {
    let (_path, db) = temp_db("act-filter");
    let repo = db.activities();
    for (id, server_id, created_at) in [
        ("act_a", Some("srv_a"), "2026-01-01T00:00:00.000Z"),
        ("act_b", Some("srv_b"), "2026-01-01T00:01:00.000Z"),
        ("act_c", Some("srv_a"), "2026-01-01T00:02:00.000Z"),
    ] {
        repo.insert(&Activity {
            id: id.into(),
            server_id: server_id.map(str::to_string),
            workspace_id: None,
            r#type: ActivityType::Configuration,
            title: id.into(),
            description: None,
            source: ActivitySource::System,
            actor: "core".into(),
            reason: None,
            outcome: None,
            trace_id: None,
            created_at: created_at.into(),
        })
        .expect("insert activity");
    }

    let feed = repo
        .list_recent_for_server("srv_a", 1)
        .expect("filtered feed");
    assert_eq!(feed.len(), 1);
    assert_eq!(feed[0].id, "act_c");
}

// MCP server configs round-trip too (they share the provider lifecycle).
#[test]
fn mcp_server_round_trip() {
    let (_path, db) = temp_db("mcp");
    let repo = db.mcp_servers();
    repo.upsert(&McpServerConfig {
        id: "mcp_1".into(),
        label: "local fs".into(),
        transport: "stdio".into(),
        command: Some("npx".into()),
        args: Some(vec![
            "-y".into(),
            "@modelcontextprotocol/server-filesystem".into(),
        ]),
        url: None,
        enabled: false,
        allowed_tools: vec![],
        trust_level: "unreviewed".into(),
    })
    .expect("upsert mcp");

    let list = repo.list().expect("list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].args.as_ref().expect("args").len(), 2);
    assert!(!list[0].enabled);
}
