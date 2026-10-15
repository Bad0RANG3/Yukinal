//! Cross-language test: the **real** Rust supervisor launching the **real** Python
//! sidecar over stdio (ADR 0001/0006).
//!
//! The Python integration needs two inputs, which `pnpm check` always provides:
//!   YUKINAL_TEST_PYTHON        path to the Python executable
//!   YUKINAL_TEST_AGENT_SOURCE  path to agent/src
//! YUKINAL_TEST_NODE is used only by the protocol supervisor's Node fixture tests.
//!
//! Without them the test skips (so a bare `cargo test` on a machine without Python dependencies
//! still passes). With `YUKINAL_TEST_REQUIRED=1` a missing input is a **failure**:
//! a green CI must not be able to mean "the cross-language test never ran".

use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use yukinal_core::sidecar::{self, SidecarConfig, SidecarEvent, PROTOCOL_VERSION};
use yukinal_core::supervisor::{RestartPolicy, Supervisor};

fn env_file(key: &str) -> Option<PathBuf> {
    std::env::var(key)
        .ok()
        .map(PathBuf::from)
        .filter(|path| path.is_file())
}

fn env_directory(key: &str) -> Option<PathBuf> {
    std::env::var(key)
        .ok()
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
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
        std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 0 }} else {{ exit 1 }}"
                ),
            ])
            .status()
            .map(|status| status.success())
            .unwrap_or(true)
    }
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|status| status.success())
    }
}

fn config() -> Option<SidecarConfig> {
    let python = std::env::var_os("YUKINAL_TEST_PYTHON").map(PathBuf::from);
    let source = env_directory("YUKINAL_TEST_AGENT_SOURCE");
    if python.is_none() || source.is_none() {
        skip_or_fail("YUKINAL_TEST_PYTHON / YUKINAL_TEST_AGENT_SOURCE");
    }
    let python = python?;
    let source = source?;
    // Each call gets a private data directory: the provider store and checkpoints
    // must not leak between the cross-layer tests, and the real desktop always
    // passes YUKINAL_DATA_DIR the same way.
    static DATA_DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);
    let data_dir = std::env::temp_dir().join(format!(
        "yukinal-sidecar-test-{}-{}",
        std::process::id(),
        DATA_DIR_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&data_dir).ok()?;
    let data_dir = data_dir.display().to_string();
    Some(SidecarConfig {
        program: python,
        args: vec!["-m".into(), "yukinal_agent.sidecar".into()],
        env: vec![
            ("PYTHONPATH".into(), source.display().to_string()),
            (
                "YUKINAL_SSH_API_TOKEN".into(),
                "integration-smoke-token-0123456789abcdef".into(),
            ),
            ("YUKINAL_SSH_API_URL".into(), "http://127.0.0.1:9".into()),
            ("YUKINAL_DATA_DIR".into(), data_dir.clone()),
        ],
        request_timeout: Duration::from_secs(10),
        entry_label: String::from("-m yukinal_agent.sidecar"),
        client_version: String::from("test"),
        data_dir,
        is_python_agent: true,
    })
}

#[tokio::test]
async fn handshakes_pings_and_reports_the_real_python_sidecar() {
    let Some(config) = config() else { return };

    let launched = sidecar::launch(&config)
        .await
        .expect("launch + initialize + describe must succeed");
    assert_eq!(launched.protocol_version, PROTOCOL_VERSION);
    assert!(
        launched.tool_count >= 1,
        "Python Agent tools must be registered"
    );
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
    let agent_pid = pong
        .get("agentPid")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .expect("the Agent reports its process id");
    let agent_parent_pid = pong.get("agentParentPid").and_then(Value::as_u64);
    assert!(
        agent_pid == pid || agent_parent_pid == Some(u64::from(pid)),
        "the Agent process must be the process Rust launched or its direct child: rust={pid}, agent={agent_pid}, parent={agent_parent_pid:?}"
    );

    // tools.list must expose the Python server tool and its JSON Schema.
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
    assert!(
        names.contains(&"server.info"),
        "host-backed server.info missing: {names:?}"
    );
    assert!(
        names.contains(&"ssh.service_status"),
        "C++-backed ssh.service_status missing: {names:?}"
    );
    assert!(
        names.contains(&"server.logs"),
        "host-backed server.logs missing: {names:?}"
    );
    assert!(
        names.contains(&"server.services"),
        "host-backed server.services missing: {names:?}"
    );
    assert!(
        names.contains(&"docker.ps"),
        "host-backed docker.ps missing: {names:?}"
    );
    assert!(
        names.contains(&"docker.logs"),
        "host-backed docker.logs missing: {names:?}"
    );
    assert!(
        names.contains(&"docker.inspect"),
        "host-backed docker.inspect missing: {names:?}"
    );
    assert!(
        names.contains(&"systemd.inspect"),
        "host-backed systemd.inspect missing: {names:?}"
    );
    assert!(
        names.contains(&"systemd.restart"),
        "host-backed systemd.restart missing: {names:?}"
    );
    assert!(
        names.contains(&"package.inspect"),
        "host-backed package.inspect missing: {names:?}"
    );
    assert!(
        names.contains(&"package.install"),
        "host-backed package.install missing: {names:?}"
    );
    assert!(
        names.contains(&"filesystem.read"),
        "host-backed filesystem.read missing: {names:?}"
    );
    for tool in [
        "filesystem.write",
        "filesystem.edit",
        "filesystem.backup",
        "filesystem.backup.list",
        "filesystem.backup.retention",
        "filesystem.backup.cleanup",
        "filesystem.restore",
    ] {
        assert!(
            names.contains(&tool),
            "host-backed {tool} missing: {names:?}"
        );
    }
    assert!(
        names.contains(&"server.exec"),
        "approval-gated server.exec missing: {names:?}"
    );
    assert!(
        names.contains(&"investigation.evidence.search"),
        "task-scoped investigation.evidence.search missing: {names:?}"
    );
    assert!(
        names.contains(&"investigation.evidence.compare"),
        "task-scoped investigation.evidence.compare missing: {names:?}"
    );
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
    wait_until_pid_is_gone(pid, Duration::from_secs(5)).await;
    if agent_pid != pid {
        wait_until_pid_is_gone(agent_pid, Duration::from_secs(5)).await;
    }
    assert!(
        !launched.handle.is_running(),
        "sidecar must be gone after shutdown"
    );
}

#[tokio::test]
async fn a_bad_entry_reports_the_build_step_instead_of_hanging() {
    let Some(node) = env_file("YUKINAL_TEST_NODE").or_else(|| {
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
        is_python_agent: false,
    };

    // The test-only Node process exits non-zero without answering: launch must fail fast.
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
async fn dropping_the_supervisor_kills_its_sidecar() {
    let Some(config) = config() else { return };

    let pid = {
        let supervisor = Supervisor::new();
        let outcome = supervisor
            .start(&config)
            .await
            .expect("managed start must succeed");
        assert!(
            pid_is_alive(outcome.runtime.pid),
            "the sidecar must be running before the supervisor is dropped"
        );
        outcome.runtime.pid
    };

    wait_until_pid_is_gone(pid, Duration::from_secs(5)).await;
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
    // `start` and `status` reach `entry` through different code paths — `start` builds a
    // `RuntimeInfo`, `status` reads the stored snapshot — so pin that they agree. This
    // cannot fail today, since both derive from the same `SidecarInfo`; it is here so a
    // future change to either path shows up, not because I could demonstrate a bug.
    assert_eq!(
        status.entry.as_deref(),
        Some(outcome.runtime.entry.as_str())
    );
    assert!(
        status.last_exit.is_none(),
        "a fresh start clears the old crash"
    );

    // A second start must reuse the process instead of forking a second agent.
    let again = supervisor.start(&config).await.expect("reuse");
    assert!(again.already_running);
    assert_eq!(again.runtime.pid, outcome.runtime.pid);
    // The reuse path returns the stored snapshot rather than rebuilding one from the
    // config it was handed, so it is the one that could report a different entry.
    assert_eq!(again.runtime.entry, outcome.runtime.entry);

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

#[tokio::test]
async fn shutdown_fails_pending_requests_and_reaps_the_child_exactly_once() {
    let Some(node) = env_file("YUKINAL_TEST_NODE").or_else(|| {
        skip_or_fail("YUKINAL_TEST_NODE");
        None
    }) else {
        return;
    };
    let script_path = std::env::temp_dir().join(format!(
        "yukinal-sidecar-shutdown-{}.cjs",
        std::process::id()
    ));
    std::fs::write(
        &script_path,
        r#"
const readline = require("node:readline");
const rl = readline.createInterface({ input: process.stdin, terminal: false });
setInterval(() => {}, 1000);
rl.on("line", (line) => {
  let message;
  try {
    message = JSON.parse(line);
  } catch {
    return;
  }
  if (message.method === "initialize") {
    process.stdout.write(JSON.stringify({
      jsonrpc: "2.0",
      id: message.id,
      result: {
        protocolVersion: process.env.YUKINAL_TEST_PROTOCOL,
        agentVersion: "shutdown-fixture",
      },
    }) + "\n");
    return;
  }
  if (message.method === "system.describe") {
    process.stdout.write(JSON.stringify({
      jsonrpc: "2.0",
      id: message.id,
      result: { toolCount: 1, toolNameCollisions: [] },
    }) + "\n");
    return;
  }
  if (message.method === "test.hang") {
    process.stdout.write(JSON.stringify({
      jsonrpc: "2.0",
      method: "test.hang.received",
      params: {},
    }) + "\n");
  }
});
"#,
    )
    .expect("write shutdown fixture");

    let config = SidecarConfig {
        program: node,
        args: vec![script_path.clone().into_os_string()],
        env: vec![(
            "YUKINAL_TEST_PROTOCOL".to_string(),
            PROTOCOL_VERSION.to_string(),
        )],
        request_timeout: Duration::from_secs(5),
        entry_label: "shutdown fixture".to_string(),
        client_version: "test".to_string(),
        data_dir: std::env::temp_dir().display().to_string(),
        is_python_agent: false,
    };
    let launched = sidecar::launch(&config)
        .await
        .expect("fixture handshake must succeed");
    let pid = launched.handle.info().pid;
    let mut events = launched.handle.subscribe();
    let pending = {
        let handle = launched.handle.clone();
        tokio::spawn(async move {
            handle
                .request("test.hang", json!({}), Duration::from_secs(30))
                .await
        })
    };

    let received = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match events.recv().await {
                Ok(SidecarEvent::Frame(frame))
                    if frame.get("method").and_then(Value::as_str)
                        == Some("test.hang.received") =>
                {
                    break;
                }
                Ok(_) => {}
                Err(error) => panic!("event stream closed before request was observed: {error}"),
            }
        }
    })
    .await;
    assert!(
        received.is_ok(),
        "fixture never observed the pending request"
    );

    let shutdown_started = Instant::now();
    launched.handle.shutdown().await;
    assert!(
        shutdown_started.elapsed() < Duration::from_secs(8),
        "shutdown must have a bounded grace period"
    );
    let response = pending.await.expect("pending request task must join");
    assert!(
        matches!(
            response,
            Err(sidecar::SidecarError::Remote(ref message))
                if message.contains("shutting down")
        ),
        "a pending request must fail instead of hanging: {response:?}"
    );
    wait_until_pid_is_gone(pid, Duration::from_secs(2)).await;

    let mut exits = 0_usize;
    while let Ok(event) = events.try_recv() {
        if matches!(event, SidecarEvent::Exited { .. }) {
            exits += 1;
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    while let Ok(event) = events.try_recv() {
        if matches!(event, SidecarEvent::Exited { .. }) {
            exits += 1;
        }
    }
    assert_eq!(exits, 1, "the reaper must publish one terminal exit event");

    let _ = std::fs::remove_file(script_path);
}

/// Kill `pid` from outside the process, which is what a crash looks like to the supervisor:
/// nothing told it to expect this exit.
fn kill_process(pid: u32) {
    #[cfg(windows)]
    let status = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .status();
    #[cfg(unix)]
    let status = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status();
    let status = status.expect("the platform kill command must be runnable");
    assert!(status.success(), "killing pid {pid} must succeed");
}

fn fast_restart_policy(base_delay_ms: u64) -> RestartPolicy {
    RestartPolicy {
        enabled: true,
        max_attempts: 3,
        base_delay: Duration::from_millis(base_delay_ms),
        max_delay: Duration::from_millis(base_delay_ms * 4),
        healthy_after: Duration::from_secs(60),
    }
}

/// Read one bounded HTTP request from the loopback Provider fixture.  The fixture only needs to
/// consume the body before answering; it deliberately does not parse model prompts or tool
/// schemas, because the deterministic response sequence below is the assertion surface.
async fn read_http_request(stream: &mut TcpStream) -> std::io::Result<()> {
    const HEADER_END: &[u8] = b"\r\n\r\n";
    const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
    let mut buffer = Vec::with_capacity(8 * 1024);
    let mut chunk = [0_u8; 8 * 1024];
    let (header_end, content_length) = loop {
        if let Some(index) = buffer
            .windows(HEADER_END.len())
            .position(|window| window == HEADER_END)
        {
            let header_length = index + HEADER_END.len();
            let headers = String::from_utf8_lossy(&buffer[..index]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| value.trim())
                })
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            break (header_length, content_length);
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > MAX_REQUEST_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "provider fixture request exceeded its bound",
            ));
        }
    };
    while buffer.len() < header_end.saturating_add(content_length) {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > MAX_REQUEST_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "provider fixture request exceeded its bound",
            ));
        }
    }
    Ok(())
}

fn provider_response_body(request_index: usize) -> String {
    let message = match request_index {
        0 => json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "call_plan",
                "type": "function",
                "function": {
                    "name": "investigation__plan",
                    "arguments": "{\"steps\":[{\"id\":\"step-plan-1\",\"ordinal\":0,\"kind\":\"evidence\",\"title\":\"Collect server health\",\"purpose\":\"Read one fresh server health snapshot\",\"allowedTools\":[\"server.info\"],\"riskLevel\":\"read\",\"successCriteria\":[\"A fresh server snapshot is persisted\"],\"requiresApproval\":false,\"maxAttempts\":1}]}"
                }
            }]
        }),
        1 => json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "call_server_info",
                "type": "function",
                "function": { "name": "server__info", "arguments": "{}" }
            }]
        }),
        _ => json!({ "role": "assistant", "content": "cross-layer fixture completed" }),
    };
    json!({
        "id": "chatcmpl-fixture",
        "object": "chat.completion",
        "created": 1_790_000_000_u64,
        "model": "fixture-model",
        "choices": [{ "index": 0, "message": message, "finish_reason": if request_index < 2 { "tool_calls" } else { "stop" } }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
    })
    .to_string()
}

async fn serve_provider_fixture(
    listener: TcpListener,
    request_count: Arc<AtomicUsize>,
) -> Result<(), String> {
    // The real AgentLoop asks the Provider once for the plan, once for the host-backed tool,
    // and once for the final response. Closing each connection makes the fixture independent of
    // HTTP keep-alive details while exercising the actual OpenAI-compatible response parser.
    for _ in 0..3 {
        let (mut stream, _) = listener.accept().await.map_err(|error| error.to_string())?;
        read_http_request(&mut stream)
            .await
            .map_err(|error| error.to_string())?;
        let index = request_count.fetch_add(1, Ordering::SeqCst);
        let body = provider_response_body(index);
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body,
        );
        stream
            .write_all(response.as_bytes())
            .await
            .map_err(|error| error.to_string())?;
        stream.shutdown().await.map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// This is the cross-language task proof that the smoke test intentionally does not attempt:
/// a real bundled sidecar receives a real OpenAI-compatible response, calls a host-backed tool, asks
/// the Rust side to persist the resulting evidence, and returns a terminal completion event.
/// The Provider is a loopback fixture, so the test never creates a shared or bridged network.
#[tokio::test]
async fn real_sidecar_runs_a_durable_read_through_provider_and_host() {
    let Some(config) = config() else { return };
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback provider fixture must bind");
    let port = listener
        .local_addr()
        .expect("loopback provider fixture has an address")
        .port();
    let provider_requests = Arc::new(AtomicUsize::new(0));
    let provider_task = tokio::spawn(serve_provider_fixture(
        listener,
        Arc::clone(&provider_requests),
    ));

    let launched = sidecar::launch(&config)
        .await
        .expect("real sidecar must launch before the durable run");
    let handle = launched.handle.clone();
    let responder_handle = handle.clone();
    let mut events = handle.subscribe();
    let provider_requests_for_events = Arc::clone(&provider_requests);
    let event_task = tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let mut frames = Vec::new();
        let mut logs = Vec::new();
        let mut host_methods = Vec::new();
        let mut tool_requests = 0_usize;
        let mut active_plan: Option<Value> = None;
        loop {
            let event = tokio::time::timeout_at(deadline, events.recv())
                .await
                .map_err(|_| {
                    format!(
                        "timed out waiting for the durable sidecar event; provider requests={}, logs={logs:?}",
                        provider_requests_for_events.load(Ordering::SeqCst)
                    )
                })?
                .map_err(|error| format!("sidecar event stream failed: {error}"))?;
            match event {
                SidecarEvent::Request { id, method, params } => {
                    host_methods.push(method.clone());
                    let response = match method.as_str() {
                        "host.mcp.catalog" => json!({ "servers": [], "failures": [] }),
                        "host.context.fetch" => json!({ "status": "not_found" }),
                        "host.investigation.plan.record" => {
                            let mut plan = params
                                .get("plan")
                                .cloned()
                                .ok_or_else(|| "plan.record did not carry a plan".to_string())?;
                            // The real host activates the first step and assigns a revision; the fixture
                            // has to do the same or the Python handler rejects the saved plan.
                            if let Some(first_step) = plan
                                .get("steps")
                                .and_then(Value::as_array)
                                .and_then(|steps| steps.first())
                                .and_then(|step| step.get("id"))
                                .and_then(Value::as_str)
                            {
                                plan["currentStepId"] = json!(first_step);
                            }
                            plan["revision"] = json!(1);
                            active_plan = Some(plan.clone());
                            json!({ "recorded": true, "plan": plan })
                        }
                        "host.investigation.plan.check" => {
                            let plan = active_plan.as_ref().ok_or_else(|| {
                                "plan.check arrived before plan.record".to_string()
                            })?;
                            let tool_name = params.get("toolName").and_then(Value::as_str);
                            if tool_name != Some("server.info") {
                                return Err(format!("durable fixture expected a server.info plan check, got {tool_name:?}"));
                            }
                            let current_step_id = plan
                                .get("currentStepId")
                                .and_then(Value::as_str)
                                .ok_or_else(|| "plan has no current step".to_string())?;
                            let step = plan
                                .get("steps")
                                .and_then(Value::as_array)
                                .and_then(|steps| {
                                    steps.iter().find(|step| {
                                        step.get("id").and_then(Value::as_str)
                                            == Some(current_step_id)
                                    })
                                })
                                .ok_or_else(|| "plan current step is missing".to_string())?;
                            json!({
                                "status": "allowed",
                                "planId": plan.get("id").and_then(Value::as_str).unwrap_or("plan_crosslayer"),
                                "stepId": current_step_id,
                                "stepKind": step.get("kind").and_then(Value::as_str).unwrap_or("evidence"),
                                "evidenceIds": step.get("evidenceIds").cloned().unwrap_or_else(|| json!([])),
                                "requiresApproval": step.get("requiresApproval").and_then(Value::as_bool).unwrap_or(false)
                            })
                        }
                        "host.investigation.plan.step_result" => {
                            let mut plan = active_plan.take().ok_or_else(|| {
                                "plan.step_result arrived before plan.record".to_string()
                            })?;
                            let step_id = params
                                .get("stepId")
                                .and_then(Value::as_str)
                                .ok_or_else(|| "plan.step_result has no step id".to_string())?;
                            if params.get("status").and_then(Value::as_str) != Some("success") {
                                return Err(
                                    "durable fixture expected a successful plan step".to_string()
                                );
                            }
                            let object = plan
                                .as_object_mut()
                                .ok_or_else(|| "plan must be an object".to_string())?;
                            let steps = object
                                .get_mut("steps")
                                .and_then(Value::as_array_mut)
                                .ok_or_else(|| "plan steps must be an array".to_string())?;
                            let step = steps
                                .iter_mut()
                                .find(|step| {
                                    step.get("id").and_then(Value::as_str) == Some(step_id)
                                })
                                .ok_or_else(|| {
                                    "plan step result references an unknown step".to_string()
                                })?;
                            step["status"] = json!("succeeded");
                            step["attempts"] = json!(1);
                            step["endedAt"] = json!("2026-09-20T00:00:01.000Z");
                            object.remove("currentStepId");
                            object.insert("status".to_string(), json!("completed"));
                            object
                                .insert("updatedAt".to_string(), json!("2026-09-20T00:00:01.000Z"));
                            active_plan = Some(plan.clone());
                            json!({ "recorded": true, "plan": plan })
                        }
                        "host.investigation.artifact.record" => {
                            let artifact = params.get("artifact").cloned().ok_or_else(|| {
                                "artifact.record did not carry an artifact".to_string()
                            })?;
                            json!({ "recorded": true, "artifact": artifact })
                        }
                        "host.tool.execute" => {
                            tool_requests += 1;
                            if params.get("toolName").and_then(Value::as_str) != Some("server.info")
                            {
                                return Err(format!(
                                    "durable fixture expected server.info, got {:?}",
                                    params.get("toolName")
                                ));
                            }
                            json!({
                                "status": "success",
                                "output": {
                                    "id": "snapshot_crosslayer",
                                    "serverId": "srv_crosslayer",
                                    "collectedAt": "2026-09-20T00:00:00.000Z",
                                    "health": "healthy",
                                    "capabilities": { "linux": true, "docker": false }
                                }
                            })
                        }
                        "host.investigation.evidence.record" => {
                            // The Python runtime records durable plans and executes host-backed reads, but
                            // evidence/artifact persistence for investigations is still deferred; a call
                            // here now is a regression, not an expected step.
                            return Err(
                                "the Python runtime called evidence.record before that lifecycle is implemented"
                                    .to_string(),
                            );
                        }
                        other => {
                            return Err(format!(
                                "unexpected host request from real sidecar: {other}"
                            ));
                        }
                    };
                    responder_handle
                        .respond(id, Ok(response))
                        .await
                        .map_err(|error| format!("could not answer {method}: {error}"))?;
                }
                SidecarEvent::Frame(frame) => {
                    if frame.get("method").and_then(Value::as_str) != Some("agent.stream") {
                        continue;
                    }
                    let params = frame.get("params").cloned().unwrap_or(Value::Null);
                    let event_type = params.get("type").and_then(Value::as_str);
                    let completed = event_type == Some("agent.completed");
                    let failed = event_type == Some("agent.failed");
                    frames.push(params);
                    if completed {
                        return Ok((frames, host_methods, tool_requests, active_plan));
                    }
                    if failed {
                        let trace = frames
                            .iter()
                            .map(|frame| {
                                json!({
                                    "type": frame.get("type").and_then(Value::as_str),
                                    "toolName": frame.get("toolName").and_then(Value::as_str),
                                    "status": frame.get("status").and_then(Value::as_str),
                                    "errorCode": frame.get("errorCode").and_then(Value::as_str),
                                })
                            })
                            .collect::<Vec<_>>();
                        return Err(format!(
                            "Agent run failed before completion: {}; provider requests={}, logs={logs:?}, trace={trace:?}",
                            frames.last().unwrap_or(&Value::Null),
                            provider_requests_for_events.load(Ordering::SeqCst)
                        ));
                    }
                }
                SidecarEvent::Log(line) => logs.push(line),
                SidecarEvent::Exited { code, signal } => {
                    return Err(format!(
                        "sidecar exited before completion: code={code:?} signal={signal:?}"
                    ));
                }
            }
        }
    });

    // The Agent resolves the model through its provider store (the desktop `yukinal.db`).
    // Seed the loopback fixture there instead of a request-scoped override, so the test
    // exercises the real provider path end to end.
    {
        let database = yukinal_database::Database::open(
            std::path::PathBuf::from(&config.data_dir).join("yukinal.db"),
        )
        .expect("the integration provider database must open");
        database
            .providers()
            .upsert_ai(&yukinal_database::models::AiProviderConfig {
                id: "loopback-fixture".into(),
                kind: yukinal_database::models::AiProviderKind::OpenaiCompatible,
                label: "loopback fixture".into(),
                base_url: format!("http://127.0.0.1:{port}/v1"),
                model: "fixture-model".into(),
                wire_api: Some("chat".into()),
                api_key_credential_ref: None,
                enabled: true,
                custom_headers: None,
                api_version: None,
                max_input_tokens: None,
                models: None,
                created_at: "2026-09-20T00:00:00.000Z".into(),
                updated_at: "2026-09-20T00:00:00.000Z".into(),
            })
            .expect("the loopback provider must be stored for the Agent");
    }

    let start = handle
        .request(
            "agent.run.start",
            json!({
                "runId": "run_crosslayer",
                "sessionId": "session_crosslayer",
                "messageId": "message_crosslayer",
                "taskId": "task_crosslayer",
                "prompt": "读取服务器健康快照并报告结果",
                "delivery": "sync",
                "resume": true,
                "mode": "readonly",
                "permissionMode": "ask",
                "target": {
                    "host": "remote",
                    "serverId": "srv_crosslayer",
                    "environment": "staging"
                },
                "taskBudget": {
                    "maxSteps": 8,
                    "maxRunMs": 20_000,
                    "maxAttempts": 1
                },
                "providerId": "loopback-fixture",
                "model": "fixture-model"
            }),
            Duration::from_secs(30),
        )
        .await
        .expect("real sidecar durable run must answer");
    let (frames, host_methods, tool_requests, active_plan) = event_task
        .await
        .expect("durable event task must not panic")
        .expect("durable event task must reach completion");

    assert_eq!(
        start.get("runId").and_then(Value::as_str),
        Some("run_crosslayer")
    );
    assert_eq!(
        start
            .get("result")
            .and_then(|result| result.get("state"))
            .and_then(Value::as_str),
        Some("completed")
    );
    assert_eq!(
        start
            .get("result")
            .and_then(|result| result.get("text"))
            .and_then(Value::as_str),
        Some("cross-layer fixture completed")
    );
    assert_eq!(provider_requests.load(Ordering::SeqCst), 3);
    assert_eq!(tool_requests, 1);
    assert_eq!(
        active_plan
            .as_ref()
            .and_then(|plan| plan.get("currentStepId"))
            .and_then(Value::as_str),
        Some("step-plan-1"),
        "the durable plan must be recorded with its active step bound"
    );
    assert!(host_methods
        .iter()
        .any(|method| method == "host.tool.execute"));
    assert!(frames.iter().any(|frame| {
        frame.get("type").and_then(Value::as_str) == Some("agent.tool_call")
            && frame.get("toolName").and_then(Value::as_str) == Some("server.info")
    }));
    assert!(frames.iter().any(|frame| {
        frame.get("type").and_then(Value::as_str) == Some("agent.tool_result")
            && frame.get("toolName").and_then(Value::as_str) == Some("server.info")
            && frame.get("status").and_then(Value::as_str) == Some("success")
    }));
    assert!(frames.iter().any(|frame| {
        frame.get("type").and_then(Value::as_str) == Some("agent.completed")
            && frame
                .get("result")
                .and_then(|result| result.get("state"))
                .and_then(Value::as_str)
                == Some("completed")
    }));

    let provider_result = tokio::time::timeout(Duration::from_secs(5), provider_task)
        .await
        .expect("provider fixture must finish after three calls")
        .expect("provider fixture task must not panic");
    provider_result.expect("provider fixture must serve both SSE responses");
    handle.shutdown().await;
}

/// The claim "a crashed sidecar is brought back automatically" is only worth making if it is
/// observed: this kills the real child and waits for a *different* pid to report running.
#[tokio::test]
async fn a_crashed_sidecar_is_restarted_and_the_restart_is_reported() {
    let Some(config) = config() else { return };
    let supervisor = Supervisor::with_restart_policy(fast_restart_policy(150));

    let first = supervisor.start(&config).await.expect("first start");
    let first_pid = first.runtime.pid;
    kill_process(first_pid);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut revived = None;
    while tokio::time::Instant::now() < deadline {
        let status = supervisor.status().await;
        if status.running {
            if let Some(pid) = status.pid.filter(|pid| *pid != first_pid) {
                revived = Some((pid, status));
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let (pid, status) = revived.expect("an unexpected exit must bring the agent back");
    assert_ne!(
        pid, first_pid,
        "a restart means a new process, not a revived pid"
    );
    assert!(
        status.last_exit.is_some(),
        "the crash must stay visible after the restart, not be erased by it"
    );
    let record = status
        .restart
        .expect("an automatic restart has to be reported, or it looks like nothing happened");
    assert_eq!(record.attempt, 1);
    assert!(!record.exhausted);

    let logs = supervisor.logs().await;
    assert!(
        logs.iter().any(|line| line.contains("restart 1/3")),
        "the tail must explain the restart: {logs:?}"
    );

    let _ = supervisor.stop().await;
}

/// The asked-for exit is not an outage. If a user's Stop could be answered with a restart,
/// the stop button would be a lie, and the attempt budget would be spent on nothing.
#[tokio::test]
async fn a_requested_stop_is_not_restarted() {
    let Some(config) = config() else { return };
    let supervisor = Supervisor::with_restart_policy(fast_restart_policy(50));

    let started = supervisor.start(&config).await.expect("start");
    assert!(started.runtime.pid > 0);
    assert!(supervisor.stop().await);

    // Several base delays: long enough that a restart triggered by the stop would be visible.
    tokio::time::sleep(Duration::from_millis(400)).await;

    let status = supervisor.status().await;
    assert!(
        !status.running,
        "a stopped agent must stay stopped: {status:?}"
    );
    assert!(
        status.restart.is_none(),
        "a stop the user asked for is not an outage, and must not appear as one: {status:?}"
    );
    let logs = supervisor.logs().await;
    assert!(
        !logs
            .iter()
            .any(|line| line.starts_with("[supervisor]") && line.contains("restart")),
        "no restart may be attempted after a requested stop: {logs:?}"
    );
}
