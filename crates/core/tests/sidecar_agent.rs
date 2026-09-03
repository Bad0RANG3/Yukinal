//! Cross-language test: the **real** Rust supervisor launching the **real** Node
//! sidecar over stdio (ADR 0001/0006).
//!
//! Needs two inputs, which `pnpm check` always provides:
//!   YUKINAL_TEST_NODE   path to the node executable
//!   YUKINAL_TEST_ENTRY  path to apps/agent/dist/index.js
//!
//! Without them the test skips (so a bare `cargo test` on a machine without the bundle
//! still passes). With `YUKINAL_TEST_REQUIRED=1` a missing input is a **failure**:
//! a green CI must not be able to mean "the cross-language test never ran".

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};
use yukinal_core::sidecar::{self, SidecarConfig, SidecarEvent, PROTOCOL_VERSION};
use yukinal_core::supervisor::Supervisor;

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var(key).ok().map(PathBuf::from).filter(|path| {
        if key.ends_with("_ENTRY") {
            path.is_file()
        } else {
            true
        }
    })
}

/// True when the runner declares that these tests must actually execute.
fn required() -> bool {
    std::env::var("YUKINAL_TEST_REQUIRED")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn skip_or_fail(what: &str) {
    let message = format!("missing {what}: set it, or unset YUKINAL_TEST_REQUIRED to skip");
    if required() {
        panic!("{message}");
    }
    eprintln!("skipped: {message}");
}

fn config() -> Option<SidecarConfig> {
    let node = env_path("YUKINAL_TEST_NODE");
    let entry = env_path("YUKINAL_TEST_ENTRY");
    if node.is_none() || entry.is_none() {
        skip_or_fail("YUKINAL_TEST_NODE / YUKINAL_TEST_ENTRY");
    }
    let node = node?;
    let entry = entry?;
    Some(SidecarConfig {
        program: node,
        args: vec![entry.into_os_string()],
        env: Vec::new(),
        request_timeout: Duration::from_secs(10),
        entry_label: String::from("integration test bundle"),
        client_version: String::from("test"),
        data_dir: std::env::temp_dir().display().to_string(),
    })
}

#[tokio::test]
async fn handshakes_pings_and_reports_the_real_sidecar() {
    let Some(config) = config() else { return };

    let launched = sidecar::launch(&config)
        .await
        .expect("launch + initialize + describe must succeed");
    assert_eq!(launched.protocol_version, PROTOCOL_VERSION);
    assert!(launched.tool_count >= 1, "system.echo must be registered");
    assert_ne!(launched.agent_version, "unknown");

    let pid = launched.handle.info().pid;
    assert!(pid > 0);

    let pong = launched
        .handle
        .request(
            "system.ping",
            json!({ "echo": "from-rust" }),
            Duration::from_secs(5),
        )
        .await
        .expect("ping must answer");
    assert_eq!(pong.get("pong").and_then(Value::as_str), Some("from-rust"));
    assert_eq!(
        pong.get("agentPid").and_then(Value::as_u64),
        Some(u64::from(pid)),
        "the pid Rust holds must be the pid that answered"
    );

    // tools.list must expose the internal dot name and its JSON Schema.
    let tools = launched
        .handle
        .request("tools.list", json!({}), Duration::from_secs(5))
        .await
        .expect("tools.list must answer");
    let names: Vec<&str> = tools
        .get("tools")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|item| item.get("name").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    assert!(names.contains(&"system.echo"), "{names:?}");
    assert!(
        names.iter().all(|name| !name.contains("__")),
        "the registry must speak internal names only (ADR 0004): {names:?}"
    );

    // A method under construction must surface as an error, never as a hang or a lie.
    let error = launched
        .handle
        .request(
            "agent.run.start",
            json!({ "runId": "r" }),
            Duration::from_secs(5),
        )
        .await
        .expect_err("incomplete run request must fail");
    assert!(
        matches!(error, sidecar::SidecarError::Remote(_)),
        "unexpected error: {error:?}"
    );

    launched.handle.shutdown().await;
    assert!(
        !launched.handle.is_running(),
        "sidecar must be gone after shutdown"
    );
}

#[tokio::test]
async fn a_bad_entry_reports_the_build_step_instead_of_hanging() {
    let Some(node) = env_path("YUKINAL_TEST_NODE").or_else(|| {
        skip_or_fail("YUKINAL_TEST_NODE");
        None
    }) else {
        return;
    };
    let config = SidecarConfig {
        program: node,
        args: vec![PathBuf::from("no-such-file.js").into_os_string()],
        env: Vec::new(),
        request_timeout: Duration::from_secs(5),
        entry_label: String::from("missing bundle"),
        client_version: String::from("test"),
        data_dir: String::new(),
    };

    // node exits non-zero without answering: launch must fail fast, not wait forever.
    let error = sidecar::launch(&config)
        .await
        .expect_err("a missing entry must not look like a healthy start");
    assert!(
        matches!(
            error,
            sidecar::SidecarError::Timeout { .. }
                | sidecar::SidecarError::Remote(_)
                | sidecar::SidecarError::NotRunning
                | sidecar::SidecarError::Frame(_)
        ),
        "unexpected error: {error:?}"
    );
}

#[tokio::test]
async fn the_supervisor_tracks_its_own_child_including_the_exit_record() {
    let Some(config) = config() else { return };

    let supervisor = Supervisor::new();
    assert!(!supervisor.status().await.running);

    let outcome = supervisor
        .start(&config)
        .await
        .expect("managed start must succeed");
    assert!(!outcome.already_running);
    assert_eq!(outcome.runtime.protocol_version, PROTOCOL_VERSION);

    let status = supervisor.status().await;
    assert!(status.running);
    assert_eq!(status.pid, Some(outcome.runtime.pid));
    assert_eq!(status.tool_count, Some(outcome.runtime.tool_count));
    assert!(
        status.last_exit.is_none(),
        "a fresh start clears the old crash"
    );

    // A second start must reuse the process instead of forking a second agent.
    let again = supervisor.start(&config).await.expect("reuse");
    assert!(again.already_running);
    assert_eq!(again.runtime.pid, outcome.runtime.pid);

    // Managed requests go through the supervisor, so commands never touch a handle.
    let described = supervisor
        .request("system.describe", json!({}), Duration::from_secs(5))
        .await
        .expect("describe through supervisor");
    assert!(
        described
            .get("toolCount")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            >= 1
    );

    let logs = supervisor.logs().await;
    assert!(
        logs.iter().any(|line| line.contains("ready")),
        "the sidecar's startup log should be visible in the tail: {logs:?}"
    );

    assert!(supervisor.stop().await);
    assert!(
        !supervisor.stop().await,
        "a second stop has nothing to kill"
    );

    // The exit watcher records the death, and status stops claiming it is alive.
    let mut saw_exit = false;
    let mut receiver = supervisor.subscribe();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, receiver.recv()).await {
            Ok(Ok(SidecarEvent::Exited { .. })) => {
                saw_exit = true;
                break;
            }
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    let final_status = supervisor.status().await;
    assert!(
        !final_status.running,
        "must not claim a dead process is running"
    );
    if !saw_exit {
        // The watcher may already have recorded it before we subscribed; the record is
        // the contract, the event is a convenience.
        assert!(
            final_status.last_exit.is_some(),
            "expected either an Exited event or a recorded lastExit"
        );
    }
}
