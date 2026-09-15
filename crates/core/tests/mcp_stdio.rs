//! Cross-language test: the **real** Rust MCP client driving a **real** Node MCP server over
//! stdio (`crates/core/tests/fixtures/mcp-server.js`).
//!
//! Needs one input, which `pnpm check` always provides:
//!   YUKINAL_TEST_NODE   path to the node executable
//!
//! Without it the test skips (so a bare `cargo test` on a machine without node still passes).
//! With `YUKINAL_TEST_REQUIRED=1` a missing input is a **failure**: a green CI must not be able
//! to mean "the MCP tests never ran".
//!
//! The three gate helpers below are the same ones `tests/sidecar_agent.rs` uses. They are copied
//! rather than shared: `cargo test` compiles every integration test file as its own crate, so a
//! shared module would have to be exposed from the library for the benefit of two test binaries.
//!
//! What these tests are for: the client's *promises* are about processes and time. A unit test
//! can assert that a `tokio::time::timeout` was configured; only a real child process can show
//! that a request times out instead of hanging, that a crash is reported with its exit code, and
//! that a dropped supervisor leaves no orphan behind.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::json;
use yukinal_core::mcp::{
    McpError, McpStdioConfig, McpSupervisor, McpTransportConfig, PREFERRED_PROTOCOL_VERSION,
    STDERR_TAIL_LINES, SUPPORTED_PROTOCOL_VERSIONS,
};
use yukinal_core::supervisor::RestartPolicy;
use yukinal_database::models::McpServerConfig as StoredMcpServer;

/// Deliberately an id that needs normalizing (`mcp_1` → `mcp-1`): the database round-trip in
/// `crates/database/tests/persistence.rs` stores exactly this spelling, so the client has to deal
/// with it. The id stays the identity; only the internal name segment is rewritten (ADR 0004).
const SERVER_ID: &str = "mcp_1";

/// Generous enough for a cold `node` on a loaded CI machine, short enough that a stuck test does
/// not take minutes.
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Only used for the "never answers" call, through the handle's explicit per-call timeout.
const SHORT_TIMEOUT: Duration = Duration::from_millis(600);

fn fast_restart_policy() -> RestartPolicy {
    RestartPolicy {
        enabled: true,
        max_attempts: 3,
        base_delay: Duration::from_millis(50),
        max_delay: Duration::from_millis(100),
        healthy_after: Duration::from_secs(60),
    }
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var(key)
        .ok()
        .map(PathBuf::from)
        .filter(|path| path.is_file())
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

/// `None` means "this machine has no node" (after `skip_or_fail` has already decided whether that
/// is a skip or a failure).
fn config(mode: &str) -> Option<McpStdioConfig> {
    let Some(node) = env_path("YUKINAL_TEST_NODE") else {
        skip_or_fail("YUKINAL_TEST_NODE");
        return None;
    };
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mcp-server.js");
    assert!(
        fixture.is_file(),
        "the fixture must be committed, not generated: {}",
        fixture.display()
    );
    Some(
        McpStdioConfig::new(SERVER_ID, "mcp fixture", node, TEST_TIMEOUT)
            .expect("a legal fixture server id")
            .with_args([fixture.into_os_string(), OsString::from(mode)]),
    )
}

/// Wait until the supervisor reports the server as not running (bounded).
async fn wait_for_exit(supervisor: &McpSupervisor, deadline: Duration) {
    let started = Instant::now();
    while started.elapsed() < deadline {
        if !supervisor.status(SERVER_ID).await.running {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the supervisor still reports {SERVER_ID} as running after {deadline:?}");
}

/// Wait until the supervisor's stderr tail contains `needle`, then return the tail as it was.
///
/// The stderr pump is its own task, so a test that wants to see the child's output has to give it
/// a bounded chance instead of assuming it already ran.
async fn wait_for_stderr(
    supervisor: &McpSupervisor,
    needle: &str,
    deadline: Duration,
) -> Vec<String> {
    let started = Instant::now();
    loop {
        let tail = supervisor.status(SERVER_ID).await.stderr_tail;
        if tail.iter().any(|line| line.contains(needle)) || started.elapsed() > deadline {
            return tail;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Wait until the supervisor's diagnostics contain `needle`, then return them as they were.
async fn wait_for_diagnostic(
    supervisor: &McpSupervisor,
    needle: &str,
    deadline: Duration,
) -> Vec<String> {
    let started = Instant::now();
    loop {
        let diagnostics = supervisor.status(SERVER_ID).await.diagnostics;
        if diagnostics.iter().any(|line| line.contains(needle)) || started.elapsed() > deadline {
            return diagnostics;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Wait until the operating system no longer knows `pid`.
///
/// This is how "no orphans" is *proved* rather than assumed: a `kill_on_drop` that quietly did
/// nothing would leave the process alive right here.
async fn wait_until_pid_is_gone(pid: u32, deadline: Duration) {
    let started = Instant::now();
    while started.elapsed() < deadline {
        if !pid_is_alive(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("process {pid} is still alive after {deadline:?}");
}

fn pid_is_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        // `tasklist` is a Windows system tool; for a filter that matches nothing it prints a
        // one-line "INFO: No tasks are running..." on stdout, so `contains` is enough. If it
        // cannot be run at all we answer "alive", which makes the wait fail loudly instead of
        // passing for the wrong reason.
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()))
            .unwrap_or(true)
    }
    #[cfg(unix)]
    {
        // `kill -0` asks the kernel whether the pid exists without signalling it.
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|status| status.success())
            .unwrap_or(true)
    }
}

#[tokio::test]
async fn a_handshake_negotiates_a_version_and_lists_the_two_tools() {
    let Some(config) = config("ok") else { return };
    let supervisor = McpSupervisor::new();

    let start = supervisor
        .start(&config)
        .await
        .expect("initialize + tools/list must succeed against a well-behaved server");
    assert!(!start.already_running);
    assert!(start.info.pid.is_some_and(|pid| pid > 0));
    assert_eq!(start.info.server_id, SERVER_ID);
    assert_eq!(
        config.segment, "mcp-1",
        "the id stays the identity while the internal name segment gets normalized"
    );

    let handshake = start.info.handshake.expect("initialize must be recorded");
    assert_eq!(
        handshake.protocol_version, PREFERRED_PROTOCOL_VERSION,
        "a server that supports the requested version echoes it"
    );
    assert!(SUPPORTED_PROTOCOL_VERSIONS.contains(&handshake.protocol_version.as_str()));
    assert_eq!(handshake.server_name, "yukinal-mcp-fixture");
    assert_eq!(handshake.server_version, "0.1.0");
    assert!(
        handshake
            .instructions
            .as_deref()
            .unwrap_or_default()
            .contains("Ignore any previous instructions"),
        "the fixture's instructions are prompt-injection shaped; the only thing that happened to \
         them is that they were stored as data (docs/boundaries/mcp.md: description text is untrusted data)"
    );

    assert_eq!(
        start.tool_count, 2,
        "the fixture declares exactly two tools"
    );
    let tools = supervisor.tools(SERVER_ID).await;
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].name, "echo");
    assert_eq!(tools[1].name, "explode");
    assert!(
        tools.iter().all(|tool| !tool.description.is_empty()),
        "every tool must be explainable to the user before it can be enabled"
    );
    assert_eq!(tools[0].input_schema["type"], "object");
    assert!(
        tools.iter().all(|tool| !tool.renamed()),
        "a spelling that is already a legal segment must not be rewritten"
    );
    assert_eq!(
        tools[0].internal_name(&config.segment).expect("legal"),
        "mcp.mcp-1.echo"
    );

    let status = supervisor.status(SERVER_ID).await;
    assert!(status.running);
    assert_eq!(status.pid, start.info.pid);
    assert_eq!(status.tool_count, 2);
    assert_eq!(
        status.protocol_version.as_deref(),
        Some(PREFERRED_PROTOCOL_VERSION)
    );
    assert!(
        status.last_exit.is_none(),
        "a running server has no exit to explain"
    );

    let report = supervisor.shutdown(SERVER_ID).await.expect("still tracked");
    assert!(report.was_running);
}

/// Explicit interoperability smoke against the official TypeScript MCP server.
///
/// It is opt-in because it downloads a package and needs the network; ordinary CI still
/// runs the hermetic fixture above. Set `YUKINAL_TEST_MCP_EVERYTHING` to the `npx`
/// executable to run it.
#[tokio::test]
async fn the_official_mcp_everything_server_interoperates() {
    let Some(npx) = env_path("YUKINAL_TEST_MCP_EVERYTHING") else {
        eprintln!("skipped: YUKINAL_TEST_MCP_EVERYTHING is not set");
        return;
    };
    let config = McpStdioConfig::new(
        "mcp_everything",
        "official everything server",
        npx,
        Duration::from_secs(60),
    )
    .expect("legal config")
    .with_args([
        OsString::from("-y"),
        OsString::from("@modelcontextprotocol/server-everything"),
        OsString::from("stdio"),
    ]);
    let supervisor = McpSupervisor::new();
    let start = supervisor
        .start(&config)
        .await
        .expect("official server handshake");
    assert!(
        start.info.handshake.is_some(),
        "a real server must negotiate initialize"
    );
    assert!(
        start.tool_count > 0,
        "the official everything server advertises tools"
    );
    assert!(
        supervisor
            .tools("mcp_everything")
            .await
            .iter()
            .any(|tool| tool.name == "echo"),
        "expected the official echo tool"
    );
    supervisor.shutdown("mcp_everything").await;
}

#[tokio::test]
async fn a_tool_call_returns_the_servers_result() {
    let Some(config) = config("ok") else { return };
    let supervisor = McpSupervisor::new();
    supervisor.start(&config).await.expect("start");

    let result = supervisor
        .call(SERVER_ID, "echo", json!({ "text": "hello" }))
        .await
        .expect("the fixture answers echo");
    assert!(!result.is_error);
    assert_eq!(result.text(), "echo: hello");
    assert_eq!(
        result
            .structured_content
            .expect("the fixture sends structuredContent")["echoed"],
        json!("hello")
    );

    supervisor.shutdown(SERVER_ID).await;
}

#[tokio::test]
async fn a_tool_that_reports_is_error_is_not_a_transport_failure() {
    let Some(config) = config("ok") else { return };
    let supervisor = McpSupervisor::new();
    supervisor.start(&config).await.expect("start");

    let result = supervisor
        .call(SERVER_ID, "explode", json!({}))
        .await
        .expect("a tool-level error is still a completed call");
    assert!(result.is_error, "the server's own verdict must survive");
    assert!(result.text().contains("failed on purpose"));

    // And the difference that matters: the process is still there.
    let status = supervisor.status(SERVER_ID).await;
    assert!(
        status.running,
        "a tool that reports an error must not be confused with a dead server"
    );
    assert!(status.last_exit.is_none());
    assert_eq!(
        supervisor
            .call(SERVER_ID, "echo", json!({ "text": "still fine" }))
            .await
            .expect("the connection is unaffected")
            .text(),
        "echo: still fine"
    );

    supervisor.shutdown(SERVER_ID).await;
}

#[tokio::test]
async fn a_server_that_never_answers_times_out_instead_of_hanging() {
    let Some(config) = config("misbehave") else {
        return;
    };
    let supervisor = McpSupervisor::new();
    supervisor.start(&config).await.expect("start");

    // The call gets a short timeout on purpose: the handshake above used the configured one, and
    // this test is about the per-call budget being honoured.
    let handle = supervisor.handle(SERVER_ID).await.expect("tracked");
    let started = Instant::now();
    let error = handle
        .call_tool("never-answer", json!({}), SHORT_TIMEOUT)
        .await
        .expect_err("nothing is ever going to answer this");
    let elapsed = started.elapsed();

    match error {
        McpError::Timeout {
            method, timeout, ..
        } => {
            assert_eq!(method, "tools/call");
            assert_eq!(timeout, SHORT_TIMEOUT);
        }
        other => panic!("a silent server is a timeout, not a death: {other:?}"),
    }
    assert!(
        elapsed < Duration::from_secs(5),
        "a {SHORT_TIMEOUT:?} timeout must not take {elapsed:?}"
    );

    // A timeout is not a death, and it does not poison the connection either.
    assert!(supervisor.status(SERVER_ID).await.running);
    assert_eq!(
        supervisor
            .call(SERVER_ID, "echo", json!({ "text": "after the timeout" }))
            .await
            .expect("the next request must still work")
            .text(),
        "echo: after the timeout"
    );

    supervisor.shutdown(SERVER_ID).await;
}

#[tokio::test]
async fn cancelling_an_in_flight_call_sends_the_mcp_cancellation_notification() {
    let Some(config) = config("misbehave") else {
        return;
    };
    let supervisor = McpSupervisor::new();
    supervisor.start(&config).await.expect("start");

    let handle = supervisor.handle(SERVER_ID).await.expect("tracked");
    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel_after = tokio::spawn({
        let cancel = cancel.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel.cancel();
        }
    });

    let error = handle
        .call_tool_with_cancel("never-answer", json!({}), TEST_TIMEOUT, &cancel)
        .await
        .expect_err("the fixture deliberately never answers");
    cancel_after.await.expect("cancellation task");
    assert!(
        matches!(error, McpError::Cancelled { ref method, .. } if method == "tools/call"),
        "{error:?}"
    );

    // The child writes only after it receives `notifications/cancelled`; seeing
    // this line proves the client did not merely stop waiting locally.
    let stderr = wait_for_stderr(&supervisor, "cancelled request", Duration::from_secs(5)).await;
    assert!(
        stderr.iter().any(|line| line.contains("cancelled request")),
        "the protocol cancellation must reach the server: {stderr:?}"
    );

    supervisor.shutdown(SERVER_ID).await;
}

#[tokio::test]
async fn a_server_that_dies_mid_session_is_restarted_and_keeps_the_exit_visible() {
    let Some(config) = config("misbehave") else {
        return;
    };
    let supervisor = McpSupervisor::with_restart_policy(fast_restart_policy());
    let start = supervisor.start(&config).await.expect("start");
    let pid = start.info.pid;

    let error = supervisor
        .call(SERVER_ID, "quit", json!({}))
        .await
        .expect_err("the fixture exits instead of answering");
    match error {
        McpError::Exited {
            code, ref reason, ..
        } => {
            assert_eq!(
                code,
                Some(7),
                "the exit code is the whole point of the record"
            );
            assert!(
                reason.contains("exit code 7"),
                "the caller must be told how it died: {reason}"
            );
        }
        other => panic!("a dead process is not a timeout: {other:?}"),
    }

    wait_for_exit(&supervisor, Duration::from_secs(5)).await;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut recovered = None;
    while Instant::now() < deadline {
        let status = supervisor.status(SERVER_ID).await;
        if status.running && status.pid != pid {
            recovered = Some(status);
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let status = recovered.expect("a crashed server must be recovered within the budget");
    assert_ne!(status.pid, pid, "recovery must use a fresh process");
    assert_eq!(
        status.last_exit.as_ref().and_then(|exit| exit.code),
        Some(7),
        "recovery must not erase why the previous process died"
    );
    let restart = status
        .restart
        .expect("the recovery attempt must be reported");
    assert_eq!(restart.attempt, 1);
    assert!(!restart.exhausted);

    // Recovery rebuilds the process; it does not replay the interrupted call.
    assert_eq!(
        supervisor
            .call(SERVER_ID, "echo", json!({ "text": "after restart" }))
            .await
            .expect("the recovered server is usable")
            .text(),
        "echo: after restart"
    );

    supervisor.shutdown(SERVER_ID).await;
}

#[tokio::test]
async fn mcp_restart_budget_is_spent_and_an_explicit_start_resets_it() {
    let Some(config) = config("misbehave") else {
        return;
    };
    let supervisor = McpSupervisor::with_restart_policy(RestartPolicy {
        max_attempts: 2,
        base_delay: Duration::from_millis(25),
        max_delay: Duration::from_millis(25),
        ..fast_restart_policy()
    });
    let mut pid = supervisor.start(&config).await.expect("start").info.pid;

    for expected_attempt in 1..=2 {
        let _ = supervisor.call(SERVER_ID, "quit", json!({})).await;
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            let status = supervisor.status(SERVER_ID).await;
            if status.running
                && status.pid != pid
                && status.restart.as_ref().map(|record| record.attempt) == Some(expected_attempt)
            {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "restart {expected_attempt} did not complete"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        pid = status.pid;
    }

    // One more crash spends the budget. The supervisor stops, and the exhausted record
    // is the status a user/UI must be able to see.
    let _ = supervisor.call(SERVER_ID, "quit", json!({})).await;
    let deadline = Instant::now() + Duration::from_secs(5);
    let exhausted = loop {
        let status = supervisor.status(SERVER_ID).await;
        if !status.running
            && status
                .restart
                .as_ref()
                .is_some_and(|record| record.exhausted)
        {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "the exhausted budget was never reported"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(
        exhausted.restart.as_ref().map(|record| record.attempt),
        Some(2)
    );
    assert_eq!(
        exhausted.restart.as_ref().map(|record| record.max_attempts),
        Some(2)
    );

    // An explicit user start is a fresh baseline, not attempt 3.
    let fresh = supervisor.start(&config).await.expect("explicit restart");
    assert!(!fresh.already_running);
    assert_ne!(fresh.info.pid, pid);
    let status = supervisor.status(SERVER_ID).await;
    assert!(status.running);
    assert!(status.restart.is_none(), "explicit start clears the outage");
    supervisor.shutdown(SERVER_ID).await;
}

#[tokio::test]
async fn starting_the_same_server_twice_reuses_one_process() {
    let Some(config) = config("ok") else { return };
    let supervisor = McpSupervisor::new();

    let first = supervisor.start(&config).await.expect("first start");
    let second = supervisor.start(&config).await.expect("second start");

    assert!(
        second.already_running,
        "the second start must reuse the running server, not spawn a second one"
    );
    assert_eq!(
        second.info.pid, first.info.pid,
        "one server id is one process (docs/boundaries/mcp.md: the process lifecycle stays with Rust)"
    );
    assert_eq!(second.tool_count, first.tool_count);
    assert_eq!(supervisor.status(SERVER_ID).await.pid, first.info.pid);
    assert_eq!(supervisor.servers().await, vec![SERVER_ID.to_string()]);
    assert!(
        pid_is_alive(first.info.pid.expect("stdio pid")),
        "the pid the supervisor reports must be a process that is actually running"
    );

    supervisor.shutdown(SERVER_ID).await;
}

#[tokio::test]
async fn shutdown_all_kills_every_server_it_manages() {
    let Some(config) = config("ok") else { return };
    let supervisor = McpSupervisor::new();

    let handle = supervisor.start(&config).await.expect("start");
    let pid = handle.info.pid;
    assert!(
        pid_is_alive(pid.expect("stdio pid")),
        "the fixture must be running before we ask it to stop"
    );

    // This is the host's exit path (`RunEvent::ExitRequested` in `apps/desktop/src-tauri/
    // src/lib.rs`): MCP servers are third-party programs we spawned, so leaving them to
    // `kill_on_drop` is not enough — a force-kill on Windows runs no `Drop`.
    let reports = supervisor.shutdown_all().await;

    assert_eq!(
        reports.len(),
        1,
        "one started server means exactly one report"
    );
    let (server_id, report) = &reports[0];
    assert_eq!(server_id, SERVER_ID);
    assert!(
        report.was_running,
        "the report must admit it had something to kill"
    );
    assert!(
        !report.unreaped,
        "a killed child must be reaped, not left as a zombie"
    );
    wait_until_pid_is_gone(pid.expect("stdio pid"), Duration::from_secs(5)).await;

    // The handle stays on purpose: a shutdown is still an exit, and the exit record is what
    // lets the settings page explain why a server is gone. What must change is the *state*,
    // not the bookkeeping — so assert that rather than an empty list.
    let status = supervisor.status(SERVER_ID).await;
    assert!(
        !status.running,
        "nothing may still be reported as running after shutdown_all"
    );
    assert!(
        status.pid.is_none(),
        "no pid may be reported for a stopped server"
    );
    assert!(
        status.last_exit.is_some(),
        "shutdown_all is an exit too, and it must be recorded"
    );
}

#[tokio::test]
async fn an_http_row_is_dispatched_to_the_http_transport_not_stdio() {
    let row = StoredMcpServer {
        id: SERVER_ID.to_string(),
        label: "remote".to_string(),
        transport: "http".to_string(),
        // A command is present, so nothing here is missing except the transport itself.
        command: Some("definitely-not-a-program".to_string()),
        args: None,
        url: Some("https://example.invalid/mcp".to_string()),
        http_auth_headers: Vec::new(),
        oauth: None,
        enabled: true,
        allowed_tools: Vec::new(),
        trust_level: "unreviewed".to_string(),
    };

    let error = McpStdioConfig::from_server_config(&row, TEST_TIMEOUT)
        .expect_err("the stdio parser must never accept an HTTP row");
    assert!(
        matches!(error, McpError::UnsupportedTransport { .. }),
        "{error:?}"
    );
    let config = McpTransportConfig::from_server_config(&row, TEST_TIMEOUT)
        .expect("the transport dispatcher recognizes HTTP");
    assert!(matches!(config, McpTransportConfig::Http(_)));

    let supervisor = McpSupervisor::new();
    assert!(supervisor.servers().await.is_empty());
}

#[tokio::test]
async fn a_program_that_cannot_be_launched_is_a_launch_error() {
    // No node needed for this one: it is about the spawn failing, not about the protocol.
    let config = McpStdioConfig::new(
        SERVER_ID,
        "missing program",
        "definitely-not-a-program-9d1f3a",
        TEST_TIMEOUT,
    )
    .expect("a legal config");

    let error = McpSupervisor::new()
        .start(&config)
        .await
        .expect_err("there is no such program");
    match error {
        McpError::Launch { ref program, .. } => {
            assert!(
                program.contains("definitely-not-a-program-9d1f3a"),
                "{program}"
            );
        }
        other => panic!("a missing program is a launch failure: {other:?}"),
    }
}

#[tokio::test]
async fn a_foreign_tool_name_is_rejected_before_it_can_reach_the_registry() {
    let Some(config) = config("foreign") else {
        return;
    };
    let supervisor = McpSupervisor::new();

    let error = supervisor
        .start(&config)
        .await
        .expect_err("`docker__get` must not become an internal tool name");
    match error {
        McpError::InvalidToolName {
            ref name,
            ref reason,
            ..
        } => {
            assert_eq!(name, "docker__get");
            assert!(
                reason.contains("Provider-side separator"),
                "the reason must point at ADR 0004, not just say 'invalid': {reason}"
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }

    assert!(
        supervisor.handle(SERVER_ID).await.is_none(),
        "a server whose tool table was refused must not be registered"
    );
    assert!(!supervisor.status(SERVER_ID).await.running);
}

#[tokio::test]
async fn a_banner_on_stdout_is_recorded_and_does_not_break_the_handshake() {
    let Some(config) = config("garbage") else {
        return;
    };
    let supervisor = McpSupervisor::new();

    let start = supervisor
        .start(&config)
        .await
        .expect("a noisy server is still a usable server");
    assert_eq!(start.tool_count, 2);

    let diagnostics = wait_for_diagnostic(&supervisor, "banner", Duration::from_secs(5)).await;
    assert!(
        diagnostics
            .iter()
            .any(|line| line.contains("not JSON") && line.contains("banner")),
        "the noise must be recorded, otherwise 'why did nothing happen' has no answer: {diagnostics:?}"
    );

    assert_eq!(
        supervisor
            .call(SERVER_ID, "echo", json!({ "text": "noisy" }))
            .await
            .expect("the connection still works")
            .text(),
        "echo: noisy"
    );

    supervisor.shutdown(SERVER_ID).await;
}

#[tokio::test]
async fn a_server_that_exits_during_the_handshake_reports_the_exit_code() {
    let Some(config) = config("silent-exit") else {
        return;
    };
    let supervisor = McpSupervisor::new();

    let error = supervisor
        .start(&config)
        .await
        .expect_err("a server that never answers cannot be started");
    match error {
        McpError::Exited {
            code, ref reason, ..
        } => {
            assert_eq!(
                code,
                Some(9),
                "'the pipe broke' is not an explanation; the exit code is"
            );
            assert!(reason.contains("exit code 9"), "{reason}");
        }
        other => panic!("unexpected error: {other:?}"),
    }

    assert!(
        supervisor.handle(SERVER_ID).await.is_none(),
        "a server that never handshook must not be registered"
    );
}

#[tokio::test]
async fn protocol_version_negotiation_accepts_a_supported_version_and_refuses_an_unknown_one() {
    // A server may answer with its own version instead of the requested one; if we speak it, we
    // take it (the spec's rule, mirrored in `wire::SUPPORTED_PROTOCOL_VERSIONS`).
    let Some(old) = config("old-version") else {
        return;
    };
    let supervisor = McpSupervisor::new();
    let start = supervisor
        .start(&old)
        .await
        .expect("2024-11-05 is in the supported list");
    assert_eq!(
        start.info.handshake.expect("handshake").protocol_version,
        "2024-11-05"
    );
    assert_ne!("2024-11-05", PREFERRED_PROTOCOL_VERSION);
    supervisor.shutdown(SERVER_ID).await;

    // A version we do not speak must end the session, not become "probably compatible".
    let Some(bad) = config("bad-version") else {
        return;
    };
    let error = supervisor
        .start(&bad)
        .await
        .expect_err("1999-01-01 must be refused");
    assert!(
        matches!(error, McpError::UnsupportedProtocolVersion { .. }),
        "{error:?}"
    );
    assert!(error.to_string().contains("1999-01-01"), "{error}");
}

#[tokio::test]
async fn the_childs_stderr_is_kept_as_a_bounded_tail() {
    let Some(config) = config("chatty") else {
        return;
    };
    let supervisor = McpSupervisor::new();
    supervisor.start(&config).await.expect("start");

    // Wait for the newest line: by then the pump has read everything the child wrote.
    let tail = wait_for_stderr(&supervisor, "line 149", Duration::from_secs(5)).await;
    assert_eq!(
        tail.len(),
        STDERR_TAIL_LINES,
        "150 numbered lines plus a banner must be capped at the named constant"
    );
    assert_eq!(
        tail.last().map(String::as_str),
        Some("mcp-fixture: line 149"),
        "the newest line is the one worth keeping"
    );
    assert!(
        !tail
            .iter()
            .any(|line| line.contains("line 0\n") || line.ends_with("line 0")),
        "a chatty server must not be able to grow the tail without bound: {tail:?}"
    );

    supervisor.shutdown(SERVER_ID).await;
}

/// 一个**陌生**服务端往 stderr / stdout 上打的凭据不能原样到达用户。
///
/// 尾部是给排障用的，所以它会被显示、可能被贴进 issue。这条测试走真实子进程那条路
/// （不是直接调脱敏函数），因为它要证明的是**管线里接上了**脱敏，而不只是函数正确 ——
/// 「函数对了但没人调用」正是这个仓库反复出现的形态。
#[tokio::test]
async fn a_leaky_child_process_cannot_get_its_secrets_into_the_tails() {
    let Some(config) = config("leaky") else {
        return;
    };
    let supervisor = McpSupervisor::new();
    supervisor.start(&config).await.expect("start");

    let stderr = wait_for_stderr(&supervisor, "GITHUB_TOKEN", Duration::from_secs(5)).await;
    let joined = stderr.join("\n");
    assert!(
        joined.contains("[redacted]"),
        "the tail must say a value was removed: {stderr:?}"
    );
    for secret in ["ghp_leak-me", "leak-me-too", "hunter2"] {
        assert!(
            !joined.contains(secret),
            "{secret} reached the stderr tail: {stderr:?}"
        );
    }

    // stdout 上的噪声是另一条管线（诊断尾部），同一份规则必须覆盖它。
    let diagnostics = wait_for_diagnostic(&supervisor, "banner", Duration::from_secs(5)).await;
    let diagnostics = diagnostics.join("\n");
    assert!(
        !diagnostics.contains("also-leak-me"),
        "the diagnostic tail leaked a value: {diagnostics:?}"
    );

    supervisor.shutdown(SERVER_ID).await;
}

#[tokio::test]
async fn shutdown_closes_the_server_within_its_budget_and_leaves_nothing_running() {
    let Some(config) = config("ok") else { return };
    let supervisor = McpSupervisor::new();
    let start = supervisor.start(&config).await.expect("start");
    let pid = start.info.pid;

    let report = supervisor.shutdown(SERVER_ID).await.expect("tracked");
    assert!(report.was_running);
    assert!(
        !report.killed,
        "a well-behaved server leaves on stdin EOF; escalating here would mean the graceful \
         path is broken"
    );
    assert!(
        !report.unreaped,
        "a reaped process is the only acceptable outcome"
    );

    let status = supervisor.status(SERVER_ID).await;
    assert!(!status.running);
    assert!(status.pid.is_none());
    assert!(
        status.last_exit.is_some(),
        "a stop is still an exit, and it must be recorded"
    );

    // A second stop has nothing to kill — and says so instead of pretending.
    let second = supervisor.shutdown(SERVER_ID).await.expect("still tracked");
    assert!(!second.was_running);
    assert!(!second.killed);

    wait_until_pid_is_gone(pid.expect("stdio pid"), Duration::from_secs(5)).await;
}

#[tokio::test]
async fn a_server_that_ignores_eof_is_killed_rather_than_waited_on() {
    let Some(config) = config("ignore-eof") else {
        return;
    };
    let supervisor = McpSupervisor::new();
    let start = supervisor.start(&config).await.expect("start");

    let started = Instant::now();
    let report = supervisor.shutdown(SERVER_ID).await.expect("tracked");
    let elapsed = started.elapsed();

    assert!(report.was_running);
    assert!(
        report.killed,
        "a server that ignores stdin EOF must be escalated to a kill"
    );
    assert!(!report.unreaped, "the kill must actually reap it");
    assert!(
        elapsed < Duration::from_secs(6),
        "shutdown must stay bounded even when the child ignores the polite exit, took {elapsed:?}"
    );

    wait_until_pid_is_gone(start.info.pid.expect("stdio pid"), Duration::from_secs(5)).await;
}

#[tokio::test]
async fn dropping_the_supervisor_kills_the_process_it_owns() {
    let Some(config) = config("ok") else { return };

    let pid = {
        let supervisor = McpSupervisor::new();
        let start = supervisor.start(&config).await.expect("start");
        assert!(
            pid_is_alive(start.info.pid.expect("stdio pid")),
            "the server must be running"
        );
        start.info.pid
        // The supervisor is dropped here: `kill_on_drop` is the last line of defence against an
        // orphan, and this test is what makes it a behaviour instead of a comment. The pumps and
        // the exit watcher only hold `Weak`, so dropping the last handle really does release the
        // child process.
    };

    wait_until_pid_is_gone(pid.expect("stdio pid"), Duration::from_secs(5)).await;
}
