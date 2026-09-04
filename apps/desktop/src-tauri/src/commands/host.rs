//! Host-side execution for the small set of read-only Agent tools.
//!
//! The sidecar can describe and request a tool, but it cannot open SSH sessions or
//! resolve credentials. This module is the narrow, deny-by-default bridge from the
//! sidecar request to Rust-owned state.

use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use yukinal_database::models::{ContainerInfo, Environment};
use yukinal_ssh::SshBackend;

use crate::commands::terminal::ensure_session;
use crate::state::AppState;

const HOST_TOOL_EXECUTE: &str = "host.tool.execute";
const SERVER_INFO: &str = "server.info";
const DOCKER_PS: &str = "docker.ps";
const MAX_CONTAINERS: usize = 200;
const DOCKER_PS_COMMAND: &str = "docker ps --format '{{json .}}' 2>/dev/null";
const DOCKER_PS_ALL_COMMAND: &str = "docker ps -a --format '{{json .}}' 2>/dev/null";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostToolExecuteRequest {
    #[allow(dead_code)]
    call_id: String,
    #[allow(dead_code)]
    trace_id: String,
    tool_name: String,
    input: Value,
    target: HostToolTarget,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostToolTarget {
    host: String,
    server_id: Option<String>,
    workspace_id: Option<String>,
    environment: Environment,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerRow {
    names: Option<String>,
    image: Option<String>,
    state: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DockerPsInput {
    all: Option<bool>,
}

/// Handle one sidecar-originated request. Expected tool failures are returned as a
/// successful JSON-RPC result containing the shared `{status,error}` envelope; only
/// malformed/unknown bridge requests use a JSON-RPC error.
pub(crate) async fn handle_sidecar_request(
    state: &AppState,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    if method != HOST_TOOL_EXECUTE {
        return Err(format!("unknown host method `{method}`"));
    }

    let request = serde_json::from_value::<HostToolExecuteRequest>(params)
        .map_err(|error| format!("invalid host tool request: {error}"))?;
    let Some(server_id) = request.target.server_id.as_deref() else {
        return Ok(failed(
            "invalid_input",
            "remote host tools require a concrete serverId",
            false,
            None,
        ));
    };
    if request.target.host != "remote" || !server_id.starts_with("srv_") {
        return Ok(failed(
            "denied_by_policy",
            "host tools only accept a resolved remote srv_ target",
            false,
            None,
        ));
    }

    let server = match state.database.servers().get(server_id) {
        Ok(server) => server,
        Err(error) => {
            return Ok(failed(
                "not_found",
                format!("target server `{server_id}` was not found: {error}"),
                false,
                None,
            ))
        }
    };
    if server.metadata.environment != request.target.environment {
        return Ok(failed(
            "denied_by_policy",
            "tool target environment does not match the registered server",
            false,
            Some(json!({
                "serverEnvironment": server.metadata.environment,
                "targetEnvironment": request.target.environment,
            })),
        ));
    }
    if let Some(workspace_id) = request.target.workspace_id.as_deref() {
        let belongs = server
            .metadata
            .workspace_ids
            .as_ref()
            .is_some_and(|ids| ids.iter().any(|id| id == workspace_id));
        if !belongs {
            return Ok(failed(
                "denied_by_policy",
                "tool target workspace is not attached to the registered server",
                false,
                None,
            ));
        }
    }

    match request.tool_name.as_str() {
        SERVER_INFO => server_info(state, server_id, &request.input).await,
        DOCKER_PS => docker_ps(state, server_id, &request.input).await,
        other => Ok(failed(
            "not_found",
            format!("host tool `{other}` is not enabled"),
            false,
            None,
        )),
    }
}

async fn server_info(state: &AppState, server_id: &str, input: &Value) -> Result<Value, String> {
    if !is_empty_object(input) {
        return Ok(failed(
            "invalid_input",
            "server.info accepts an empty object",
            true,
            None,
        ));
    }

    if let Err(error) = ensure_session(state, server_id).await {
        return Ok(failed("transport", error, true, None));
    }
    let session = match state.terminals.cached_session(server_id) {
        Ok(session) => session,
        Err(error) => return Ok(failed("transport", error.to_string(), true, None)),
    };
    let collected_at = yukinal_core::sidecar::iso8601_now();
    let (snapshot, _) = match yukinal_core::collector::collect_snapshot(
        &state.ssh,
        &session,
        server_id,
        &collected_at,
    )
    .await
    {
        Ok(result) => result,
        Err(error) => return Ok(failed("execution_failed", error.to_string(), true, None)),
    };

    if let Err(error) = state.database.snapshots().insert(&snapshot) {
        return Ok(failed("internal", error.to_string(), false, None));
    }
    let output = serde_json::to_value(snapshot).map_err(|error| error.to_string())?;
    Ok(success(output))
}

async fn docker_ps(state: &AppState, server_id: &str, input: &Value) -> Result<Value, String> {
    let input = match serde_json::from_value::<DockerPsInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("docker.ps input is invalid: {error}"),
                true,
                None,
            ))
        }
    };
    if let Err(error) = ensure_session(state, server_id).await {
        return Ok(failed("transport", error, true, None));
    }
    let session = match state.terminals.cached_session(server_id) {
        Ok(session) => session,
        Err(error) => return Ok(failed("transport", error.to_string(), true, None)),
    };
    let command = if input.all.unwrap_or(false) {
        DOCKER_PS_ALL_COMMAND
    } else {
        DOCKER_PS_COMMAND
    };
    let result = match state
        .ssh
        .execute(
            &session,
            command,
            Some(std::time::Duration::from_secs(10)),
            &CancellationToken::new(),
        )
        .await
    {
        Ok(result) => result,
        Err(error) => return Ok(failed("transport", error.to_string(), true, None)),
    };

    // A missing Docker binary or a non-Docker host is a valid, structured answer.
    if result.exit_code != 0 {
        return Ok(success(json!({ "available": false, "containers": [] })));
    }
    Ok(success(json!({
        "available": true,
        "containers": parse_docker_ps(&result.stdout_lossy()),
    })))
}

fn parse_docker_ps(raw: &str) -> Vec<ContainerInfo> {
    raw.lines()
        .filter_map(|line| serde_json::from_str::<DockerRow>(line.trim()).ok())
        .filter_map(|row| {
            let name = row.names?.split(',').next()?.trim().to_string();
            let image = row.image?.trim().to_string();
            let state = row.state?.trim().to_string();
            let status = row.status?.trim().to_string();
            if name.is_empty() || image.is_empty() || state.is_empty() || status.is_empty() {
                return None;
            }
            Some(ContainerInfo {
                name,
                image,
                state,
                status,
                // `docker ps` is intentionally bounded to listing; restartCount
                // belongs to the later inspect tool and is not guessed here.
                restart_count: 0,
            })
        })
        .take(MAX_CONTAINERS)
        .collect()
}

fn is_empty_object(value: &Value) -> bool {
    value.as_object().is_some_and(serde_json::Map::is_empty)
}

fn success(output: Value) -> Value {
    json!({ "status": "success", "output": output })
}

fn failed(code: &str, message: impl Into<String>, retryable: bool, detail: Option<Value>) -> Value {
    let mut error = json!({ "code": code, "message": message.into(), "retryable": retryable });
    if let Some(detail) = detail {
        error["detail"] = detail;
    }
    json!({ "status": "failed", "error": error })
}

#[cfg(test)]
mod tests {
    use super::parse_docker_ps;

    #[test]
    fn parses_docker_json_lines_into_bounded_structured_rows() {
        let rows = parse_docker_ps(
            r#"{"Names":"web,web-old","Image":"nginx:1.27","State":"running","Status":"Up 3 hours"}
{"Names":"db","Image":"postgres:16","State":"exited","Status":"Exited (0) 2 days ago"}
not-json
"#,
        );

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "web");
        assert_eq!(rows[0].restart_count, 0);
        assert_eq!(rows[1].state, "exited");
    }

    #[test]
    fn drops_rows_missing_required_fields() {
        let rows = parse_docker_ps(
            r#"{"Names":"","Image":"nginx","State":"running","Status":"Up"}
{"Names":"ok","Image":"","State":"running","Status":"Up"}
"#,
        );
        assert!(rows.is_empty());
    }
}
