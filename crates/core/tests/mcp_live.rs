use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use yukinal_core::mcp::{McpHttpConfig, McpStdioConfig, McpSupervisor, McpTransportConfig};

const LIVE_FLAG: &str = "YUKINAL_MCP_LIVE";
const MATRIX_PATH: &str = "YUKINAL_MCP_LIVE_MATRIX";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize)]
struct MatrixFile {
    servers: Vec<MatrixServer>,
}

#[derive(Debug, Deserialize)]
struct MatrixServer {
    id: String,
    label: String,
    implementation: String,
    version: String,
    transport: String,
    program: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    url: Option<String>,
    auth_header: Option<AuthHeader>,
    tool: Option<String>,
    arguments: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct AuthHeader {
    name: String,
    env: String,
}

fn matrix() -> Option<MatrixFile> {
    if std::env::var(LIVE_FLAG).ok().as_deref() != Some("1") {
        eprintln!("skipped: set {LIVE_FLAG}=1 and {MATRIX_PATH} to run MCP interoperability tests");
        return None;
    }
    let path = std::env::var(MATRIX_PATH)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("{MATRIX_PATH} is required when {LIVE_FLAG}=1"));
    let contents = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read MCP live matrix {}: {error}", path.display()));
    serde_json::from_str(&contents)
        .unwrap_or_else(|error| panic!("cannot parse MCP live matrix {}: {error}", path.display()))
}

fn validate_matrix(matrix: &MatrixFile) {
    assert!(
        matrix.servers.len() >= 2,
        "the live matrix must contain at least an official and an independent server"
    );
    let mut ids = HashSet::new();
    assert!(
        matrix
            .servers
            .iter()
            .all(|server| ids.insert(server.id.as_str())),
        "live matrix server ids must be unique"
    );
    assert!(
        matrix
            .servers
            .iter()
            .any(|server| server.transport == "stdio"),
        "the live matrix must contain a stdio server"
    );
    assert!(
        matrix
            .servers
            .iter()
            .any(|server| server.transport == "http"),
        "the live matrix must contain a Streamable HTTP server"
    );
    assert!(
        matrix.servers.iter().any(|server| {
            let implementation = server.implementation.to_ascii_lowercase();
            implementation.contains("official")
                && (implementation.contains("typescript") || implementation.contains("python"))
        }),
        "the live matrix must contain an official TypeScript or Python implementation"
    );
    assert!(
        matrix.servers.iter().any(|server| server
            .implementation
            .to_ascii_lowercase()
            .contains("independent")),
        "the live matrix must contain an independent implementation"
    );
    assert!(
        matrix.servers.iter().all(|server| {
            !server.implementation.trim().is_empty() && !server.version.trim().is_empty()
        }),
        "every live matrix entry must name its implementation and selected version"
    );
}

fn config(server: &MatrixServer) -> McpTransportConfig {
    match server.transport.as_str() {
        "stdio" => {
            let program = server
                .program
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| panic!("stdio entry {} needs program", server.id));
            let config = McpStdioConfig::new(&server.id, &server.label, program, REQUEST_TIMEOUT)
                .unwrap_or_else(|error| panic!("invalid stdio entry {}: {error}", server.id));
            McpTransportConfig::Stdio(config.with_args(server.args.iter().map(OsString::from)))
        }
        "http" => {
            let url = server
                .url
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| panic!("http entry {} needs url", server.id));
            let mut config = McpHttpConfig::new(&server.id, &server.label, url, REQUEST_TIMEOUT)
                .unwrap_or_else(|error| panic!("invalid HTTP entry {}: {error}", server.id));
            if let Some(auth) = &server.auth_header {
                let value = std::env::var(&auth.env).unwrap_or_else(|_| {
                    panic!("missing {} for live server {}", auth.env, server.id)
                });
                config = config
                    .with_auth_header(&auth.name, &value)
                    .unwrap_or_else(|error| {
                        panic!("invalid auth header for {}: {error}", server.id)
                    });
            }
            McpTransportConfig::Http(config)
        }
        other => panic!("unsupported live MCP transport {other:?} for {}", server.id),
    }
}

#[tokio::test]
async fn configured_mcp_matrix_exercises_handshake_tools_call_and_shutdown() {
    let Some(matrix) = matrix() else { return };
    validate_matrix(&matrix);

    let supervisor = McpSupervisor::new();
    let mut results = Vec::with_capacity(matrix.servers.len());
    for server in &matrix.servers {
        let transport = config(server);
        let started = supervisor
            .start_transport(&transport)
            .await
            .unwrap_or_else(|error| panic!("{} failed to initialize: {error}", server.id));
        let handshake = started
            .info
            .handshake
            .clone()
            .unwrap_or_else(|| panic!("{} returned no initialize result", server.id));
        let tools = supervisor.tools(&server.id).await;
        assert!(
            !tools.is_empty(),
            "{} must expose at least one tool",
            server.id
        );
        if let Some(tool) = &server.tool {
            assert!(
                tools.iter().any(|descriptor| descriptor.name == *tool),
                "{} does not expose configured tool {}",
                server.id,
                tool
            );
            let result = supervisor
                .call(
                    &server.id,
                    tool,
                    server.arguments.clone().unwrap_or_else(|| json!({})),
                )
                .await
                .unwrap_or_else(|error| panic!("{} tool call failed: {error}", server.id));
            assert!(
                !result.is_error,
                "{} reported a tool error: {}",
                server.id,
                result.text()
            );
        }
        results.push(json!({
            "id": server.id,
            "implementation": server.implementation,
            "declaredVersion": server.version,
            "transport": server.transport,
            "serverName": handshake.server_name,
            "serverVersion": handshake.server_version,
            "toolCount": tools.len(),
            "toolNames": tools.iter().map(|descriptor| descriptor.name.clone()).collect::<Vec<_>>(),
            "calledTool": server.tool,
        }));
    }

    for server in &matrix.servers {
        let report = supervisor
            .shutdown(&server.id)
            .await
            .unwrap_or_else(|| panic!("{} was not tracked after start", server.id));
        assert!(
            report.was_running,
            "{} was not running before shutdown",
            server.id
        );
    }
    println!(
        "{}",
        serde_json::to_string(&json!({ "mcpLive": results })).expect("serialize report")
    );
}
