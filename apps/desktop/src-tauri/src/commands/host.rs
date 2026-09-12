//! Host-side execution for bounded Agent tools (read-only plus permission-gated writes).
//!
//! The sidecar can describe and request a tool, but it cannot open SSH sessions or
//! resolve credentials. This module is the narrow, deny-by-default bridge from the
//! sidecar request to Rust-owned state.
//!
//! The file tools (`filesystem.read` / `filesystem.write`) delegate to `yukinal-filesystem`:
//! path policy, byte caps and bounded decoding live there, and this module only maps the
//! capability's typed failures onto the host protocol's failure codes
//! (see [`filesystem_failure`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use yukinal_database::models::{ContainerInfo, Environment};
use yukinal_database::DatabaseError;
use yukinal_filesystem::{AgentReadRequest, AgentWriteRequest, Error as FilesystemError};
use yukinal_ssh::SshBackend;

use crate::commands::files::remote_file_service;
use crate::commands::terminal::ensure_session;
use crate::state::AppState;

const HOST_TOOL_EXECUTE: &str = "host.tool.execute";
const HOST_CONTEXT_FETCH: &str = "host.context.fetch";
pub(crate) const HOST_TOOL_CANCEL: &str = "host.tool.cancel";
const SERVER_INFO: &str = "server.info";
const DOCKER_PS: &str = "docker.ps";
const DOCKER_LOGS: &str = "docker.logs";
const DOCKER_INSPECT: &str = "docker.inspect";
const DOCKER_RESTART: &str = "docker.restart";
const FILESYSTEM_READ: &str = "filesystem.read";
const FILESYSTEM_WRITE: &str = "filesystem.write";
const MAX_CONTAINERS: usize = 200;
const DEFAULT_LOG_TAIL: usize = 120;
const MAX_LOG_TAIL: usize = 500;
const MAX_LOG_LINE_CHARS: usize = 4_000;
const DEFAULT_RESTART_TIMEOUT: usize = 10;
const MAX_RESTART_TIMEOUT: usize = 120;
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
struct HostToolCancelRequest {
    request_id: i64,
}

pub(crate) type HostCancellationRegistry = Arc<Mutex<HashMap<i64, CancellationToken>>>;

/// Cancel a host request that is already executing. The response is deliberately
/// best-effort: a request may have completed between the Agent's abort and this
/// frame arriving at the host.
pub(crate) fn cancel_sidecar_request(
    registry: &HostCancellationRegistry,
    params: Value,
) -> Result<Value, String> {
    let request = serde_json::from_value::<HostToolCancelRequest>(params)
        .map_err(|error| format!("invalid host cancellation request: {error}"))?;
    if request.request_id <= 0 {
        return Err("host cancellation request id must be positive".to_string());
    }
    let token = registry
        .lock()
        .map_err(|_| "host cancellation registry is poisoned".to_string())?
        .remove(&request.request_id);
    let cancelled = token.is_some();
    if let Some(token) = token {
        token.cancel();
    }
    Ok(json!({ "cancelled": cancelled }))
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostContextRequest {
    kind: HostContextKind,
    id: String,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum HostContextKind {
    Server,
    Snapshot,
    Workspace,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DockerLogsInput {
    container: String,
    tail: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DockerInspectInput {
    container: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DockerRestartInput {
    container: String,
    timeout_seconds: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FilesystemReadInput {
    path: String,
    max_bytes: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FilesystemWriteInput {
    path: String,
    content: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerInspectRow {
    id: Option<String>,
    name: Option<String>,
    config: Option<DockerConfig>,
    state: Option<DockerState>,
    restart_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerConfig {
    image: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerState {
    status: Option<String>,
    started_at: Option<String>,
    health: Option<DockerHealth>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerHealth {
    status: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DockerLogsResult {
    container: String,
    lines: Vec<String>,
    truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DockerInspectResult {
    id: String,
    name: String,
    image: String,
    state: String,
    status: String,
    restart_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    health: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DockerRestartResult {
    container: String,
    restarted: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FilesystemReadResult {
    path: String,
    content: String,
    truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FilesystemWriteResult {
    path: String,
    bytes_written: usize,
}

/// Host request entry point with a cancellation token owned by the sidecar
/// dispatcher. Keeping the token explicit prevents a user Stop from ending only
/// the Node promise while an SSH operation continues in Rust.
pub(crate) async fn handle_sidecar_request_with_cancel(
    state: &AppState,
    method: &str,
    params: Value,
    cancel: CancellationToken,
) -> Result<Value, String> {
    if method == HOST_CONTEXT_FETCH {
        return handle_context_request(state, params);
    }
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
        SERVER_INFO => server_info(state, server_id, &request.input, &cancel).await,
        DOCKER_PS => docker_ps(state, server_id, &request.input, &cancel).await,
        DOCKER_LOGS => docker_logs(state, server_id, &request.input, &cancel).await,
        DOCKER_INSPECT => docker_inspect(state, server_id, &request.input, &cancel).await,
        DOCKER_RESTART => docker_restart(state, server_id, &request.input, &cancel).await,
        FILESYSTEM_READ => filesystem_read(state, server_id, &request.input, &cancel).await,
        FILESYSTEM_WRITE => filesystem_write(state, server_id, &request.input, &cancel).await,
        other => Ok(failed(
            "not_found",
            format!("host tool `{other}` is not enabled"),
            false,
            None,
        )),
    }
}

fn handle_context_request(state: &AppState, params: Value) -> Result<Value, String> {
    let request = serde_json::from_value::<HostContextRequest>(params)
        .map_err(|error| format!("invalid host context request: {error}"))?;
    if request.id.trim().is_empty() || request.id.len() > 160 {
        return Ok(failed(
            "invalid_input",
            "context id must be between 1 and 160 characters",
            true,
            None,
        ));
    }
    if matches!(
        request.kind,
        HostContextKind::Server | HostContextKind::Snapshot
    ) && !request.id.starts_with("srv_")
    {
        return Ok(failed(
            "invalid_input",
            "server context requires an opaque srv_ id",
            false,
            None,
        ));
    }

    match request.kind {
        HostContextKind::Server => context_row(state.database.servers().get(&request.id)),
        HostContextKind::Snapshot => match state.database.snapshots().latest(&request.id) {
            Ok(Some(snapshot)) => context_success(snapshot),
            Ok(None) => Ok(json!({ "status": "not_found" })),
            Err(error) => context_error(error),
        },
        HostContextKind::Workspace => context_row(state.database.workspaces().get(&request.id)),
    }
}

fn context_row<T: Serialize>(result: yukinal_database::Result<T>) -> Result<Value, String> {
    match result {
        Ok(value) => context_success(value),
        Err(error) => context_error(error),
    }
}

fn context_success<T: Serialize>(value: T) -> Result<Value, String> {
    Ok(json!({
        "status": "success",
        "data": serde_json::to_value(value).map_err(|error| error.to_string())?,
    }))
}

fn context_error(error: DatabaseError) -> Result<Value, String> {
    if matches!(error, DatabaseError::NotFound) {
        return Ok(json!({ "status": "not_found" }));
    }
    Ok(failed("internal", error.to_string(), false, None))
}

async fn ensure_session_with_cancel(
    state: &AppState,
    server_id: &str,
    cancel: &CancellationToken,
) -> Result<(), String> {
    if cancel.is_cancelled() {
        return Err("host operation cancelled".to_string());
    }
    tokio::select! {
        result = ensure_session(state, server_id) => result,
        _ = cancel.cancelled() => Err("host operation cancelled".to_string()),
    }
}

fn transport_or_cancel(error: impl std::fmt::Display, cancel: &CancellationToken) -> Value {
    if cancel.is_cancelled() {
        cancelled_failure()
    } else {
        failed("transport", error.to_string(), true, None)
    }
}

/// File-capability failure → the host tool result's failure code.
///
/// The codes themselves (`invalid_input` / `denied_by_policy`) belong to the sidecar's host
/// protocol, not to the file capability, so the mapping stays here while the rules, the messages
/// and the limits live in `yukinal-filesystem`. `retryable` follows the existing table: a bad
/// argument can be fixed by the Agent, a policy denial cannot.
fn filesystem_failure(error: FilesystemError, cancel: &CancellationToken) -> Value {
    match error {
        FilesystemError::InvalidInput(message) => failed("invalid_input", message, true, None),
        FilesystemError::DeniedByPolicy(message) => {
            failed("denied_by_policy", message, false, None)
        }
        FilesystemError::Transport(error) => transport_or_cancel(error, cancel),
    }
}

fn cancelled_failure() -> Value {
    failed("cancelled", "Host operation cancelled", false, None)
}

async fn server_info(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    if !is_empty_object(input) {
        return Ok(failed(
            "invalid_input",
            "server.info accepts an empty object",
            true,
            None,
        ));
    }

    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let session = match state.terminals.cached_session(server_id) {
        Ok(session) => session,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    let collected_at = yukinal_core::sidecar::iso8601_now();
    let (snapshot, _) = tokio::select! {
        result = yukinal_core::collector::collect_snapshot(
            &state.ssh,
            &session,
            server_id,
            &collected_at,
        ) => match result {
            Ok(result) => result,
            Err(error) => return Ok(transport_or_cancel(error, cancel)),
        },
        _ = cancel.cancelled() => return Ok(cancelled_failure()),
    };

    if let Err(error) = state.database.snapshots().insert(&snapshot) {
        return Ok(failed("internal", error.to_string(), false, None));
    }
    let output = serde_json::to_value(snapshot).map_err(|error| error.to_string())?;
    Ok(success(output))
}

async fn filesystem_read(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<FilesystemReadInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("filesystem.read input is invalid: {error}"),
                true,
                None,
            ))
        }
    };
    // Path policy and the byte cap are checked before anything else, cancellation included:
    // an invalid argument reports `invalid_input` even after the user pressed Stop, and a
    // blocked path never opens a session. The crate's request type carries that guarantee, so
    // the rules are not repeated here.
    let request = match AgentReadRequest::check(&input.path, input.max_bytes) {
        Ok(request) => request,
        Err(error) => return Ok(filesystem_failure(error, cancel)),
    };
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let service = remote_file_service(state);
    let read = tokio::select! {
        result = service.agent_read(server_id, &request) => match result {
            Ok(read) => read,
            Err(error) => return Ok(filesystem_failure(error, cancel)),
        },
        _ = cancel.cancelled() => return Ok(cancelled_failure()),
    };
    Ok(success(
        serde_json::to_value(FilesystemReadResult {
            path: read.path,
            content: read.content,
            truncated: read.truncated,
        })
        .map_err(|error| error.to_string())?,
    ))
}

async fn filesystem_write(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<FilesystemWriteInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("filesystem.write input is invalid: {error}"),
                true,
                None,
            ))
        }
    };
    let request = match AgentWriteRequest::check(&input.path, input.content) {
        Ok(request) => request,
        Err(error) => return Ok(filesystem_failure(error, cancel)),
    };
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let service = remote_file_service(state);
    let write = tokio::select! {
        result = service.agent_write(server_id, &request) => match result {
            Ok(write) => write,
            Err(error) => return Ok(filesystem_failure(error, cancel)),
        },
        _ = cancel.cancelled() => return Ok(cancelled_failure()),
    };
    Ok(success(
        serde_json::to_value(FilesystemWriteResult {
            path: write.path,
            bytes_written: write.bytes_written,
        })
        .map_err(|error| error.to_string())?,
    ))
}

async fn docker_ps(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
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
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let session = match state.terminals.cached_session(server_id) {
        Ok(session) => session,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
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
            cancel,
        )
        .await
    {
        Ok(result) => result,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
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

async fn docker_logs(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<DockerLogsInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("docker.logs input is invalid: {error}"),
                true,
                None,
            ))
        }
    };
    if !is_safe_container_ref(&input.container) {
        return Ok(failed(
            "invalid_input",
            "container must be a Docker name or id without shell metacharacters",
            true,
            None,
        ));
    }
    let tail = input.tail.unwrap_or(DEFAULT_LOG_TAIL);
    if !(1..=MAX_LOG_TAIL).contains(&tail) {
        return Ok(failed(
            "invalid_input",
            format!("tail must be between 1 and {MAX_LOG_TAIL}"),
            true,
            None,
        ));
    }
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let session = match state.terminals.cached_session(server_id) {
        Ok(session) => session,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    let command = format!(
        "docker logs --tail {tail} --timestamps -- {} 2>&1",
        shell_quote(&input.container)
    );
    let result = match state
        .ssh
        .execute(
            &session,
            &command,
            Some(std::time::Duration::from_secs(10)),
            cancel,
        )
        .await
    {
        Ok(result) => result,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    if result.exit_code != 0 {
        return Ok(failed(
            "execution_failed",
            format!("could not read logs for container `{}`", input.container),
            false,
            Some(json!({
                "exitCode": result.exit_code,
                "stderr": truncate_text(&result.stderr_lossy(), 1_000),
            })),
        ));
    }

    let (lines, truncated) = bounded_log_lines(&result.stdout_lossy(), tail);
    Ok(success(
        serde_json::to_value(DockerLogsResult {
            container: input.container,
            lines,
            truncated,
        })
        .map_err(|error| error.to_string())?,
    ))
}

async fn docker_inspect(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<DockerInspectInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("docker.inspect input is invalid: {error}"),
                true,
                None,
            ))
        }
    };
    if !is_safe_container_ref(&input.container) {
        return Ok(failed(
            "invalid_input",
            "container must be a Docker name or id without shell metacharacters",
            true,
            None,
        ));
    }
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let session = match state.terminals.cached_session(server_id) {
        Ok(session) => session,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    let command = format!(
        "docker inspect --format '{{{{json .}}}}' -- {} 2>/dev/null",
        shell_quote(&input.container)
    );
    let result = match state
        .ssh
        .execute(
            &session,
            &command,
            Some(std::time::Duration::from_secs(10)),
            cancel,
        )
        .await
    {
        Ok(result) => result,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    if result.exit_code != 0 {
        return Ok(failed(
            "not_found",
            format!("container `{}` was not found", input.container),
            false,
            Some(json!({
                "exitCode": result.exit_code,
                "stderr": truncate_text(&result.stderr_lossy(), 1_000),
            })),
        ));
    }
    let inspected = match parse_docker_inspect(&result.stdout_lossy()) {
        Ok(inspected) => inspected,
        Err(error) => return Ok(failed("execution_failed", error, false, None)),
    };
    Ok(success(
        serde_json::to_value(inspected).map_err(|error| error.to_string())?,
    ))
}

async fn docker_restart(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<DockerRestartInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("docker.restart input is invalid: {error}"),
                true,
                None,
            ))
        }
    };
    if !is_safe_container_ref(&input.container) {
        return Ok(failed(
            "invalid_input",
            "container must be a Docker name or id without shell metacharacters",
            true,
            None,
        ));
    }
    let timeout = input.timeout_seconds.unwrap_or(DEFAULT_RESTART_TIMEOUT);
    if !(1..=MAX_RESTART_TIMEOUT).contains(&timeout) {
        return Ok(failed(
            "invalid_input",
            format!("timeoutSeconds must be between 1 and {MAX_RESTART_TIMEOUT}"),
            true,
            None,
        ));
    }
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let session = match state.terminals.cached_session(server_id) {
        Ok(session) => session,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    let command = docker_restart_command(&input.container, timeout);
    let result = match state
        .ssh
        .execute_once(
            &session,
            &command,
            Some(std::time::Duration::from_secs(30)),
            cancel,
        )
        .await
    {
        Ok(result) => result,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    if result.exit_code != 0 {
        return Ok(failed(
            "execution_failed",
            format!("could not restart container `{}`", input.container),
            false,
            Some(json!({
                "exitCode": result.exit_code,
                "stderr": truncate_text(&result.stderr_lossy(), 1_000),
            })),
        ));
    }
    Ok(success(
        serde_json::to_value(DockerRestartResult {
            container: input.container,
            restarted: true,
        })
        .map_err(|error| error.to_string())?,
    ))
}

fn docker_restart_command(container: &str, timeout: usize) -> String {
    format!(
        "docker restart --time {timeout} -- {}",
        shell_quote(container)
    )
}

fn parse_docker_inspect(raw: &str) -> Result<DockerInspectResult, String> {
    let line = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| "docker inspect returned no JSON object".to_string())?;
    let row = serde_json::from_str::<DockerInspectRow>(line)
        .map_err(|error| format!("docker inspect returned invalid JSON: {error}"))?;
    let id = nonempty(row.id, "Id")?;
    let name = row
        .name
        .map(|value| value.trim_start_matches('/').to_string())
        .filter(|value| is_safe_container_ref(value))
        .ok_or_else(|| "docker inspect returned an invalid Name".to_string())?;
    let image = nonempty(row.config.and_then(|config| config.image), "Config.Image")?;
    let state = row
        .state
        .ok_or_else(|| "docker inspect omitted State".to_string())?;
    let status = nonempty(state.status, "State.Status")?;
    let started_at = state.started_at.filter(|value| !value.trim().is_empty());
    let health = state
        .health
        .and_then(|health| health.status)
        .filter(|value| !value.trim().is_empty());
    Ok(DockerInspectResult {
        id,
        name,
        image,
        state: status.clone(),
        status,
        restart_count: row.restart_count.unwrap_or(0),
        started_at,
        health,
    })
}

fn nonempty(value: Option<String>, field: &str) -> Result<String, String> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string())
        .ok_or_else(|| format!("docker inspect omitted {field}"))
}

fn is_safe_container_ref(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    value.len() <= 128
        && first.is_ascii_alphanumeric()
        && chars.all(|character| character.is_ascii_alphanumeric() || "_.-".contains(character))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn bounded_log_lines(raw: &str, limit: usize) -> (Vec<String>, bool) {
    let mut truncated = false;
    let lines = raw
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            if index >= limit {
                truncated = true;
                return None;
            }
            let bounded = truncate_text(line, MAX_LOG_LINE_CHARS);
            if bounded.chars().count() < line.chars().count() {
                truncated = true;
            }
            Some(bounded)
        })
        .collect();
    (lines, truncated)
}

fn truncate_text(value: &str, max_chars: usize) -> String {
    let mut output: String = value.chars().take(max_chars).collect();
    if value.chars().count() > max_chars {
        output.push('…');
    }
    output
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
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::{
        bounded_log_lines, cancel_sidecar_request, docker_restart_command, is_safe_container_ref,
        parse_docker_inspect, parse_docker_ps, shell_quote, HostCancellationRegistry,
    };

    #[test]
    fn cancellation_registry_cancels_and_removes_a_running_request() {
        let registry: HostCancellationRegistry = Arc::new(Mutex::new(HashMap::new()));
        let token = CancellationToken::new();
        registry
            .lock()
            .expect("registry lock")
            .insert(7, token.clone());

        let result = cancel_sidecar_request(&registry, json!({ "requestId": 7 }))
            .expect("cancellation response");
        assert_eq!(result["cancelled"], json!(true));
        assert!(token.is_cancelled());

        let result = cancel_sidecar_request(&registry, json!({ "requestId": 7 }))
            .expect("second cancellation response");
        assert_eq!(result["cancelled"], json!(false));
    }

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

    #[test]
    fn bounds_log_lines_and_marks_long_output() {
        let long_line = "x".repeat(4_010);
        let raw = format!("first\n{long_line}\nthird\n");
        let (lines, truncated) = bounded_log_lines(&raw, 2);

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "first");
        assert!(lines[1].ends_with('…'));
        assert!(truncated);
    }

    #[test]
    fn parses_normalized_inspect_fields_without_forwarding_raw_docker_shape() {
        let result = parse_docker_inspect(
            r#"{"Id":"sha256:abc","Name":"/web","Config":{"Image":"nginx:1.27"},"State":{"Status":"running","StartedAt":"2026-09-04T06:00:00Z","Health":{"Status":"healthy"}},"RestartCount":2}"#,
        )
        .expect("inspect output");

        assert_eq!(result.id, "sha256:abc");
        assert_eq!(result.name, "web");
        assert_eq!(result.state, "running");
        assert_eq!(result.restart_count, 2);
        assert_eq!(result.health.as_deref(), Some("healthy"));
    }

    #[test]
    fn container_reference_validation_and_shell_quote_are_defensive() {
        assert!(is_safe_container_ref("api_1.2-3"));
        assert!(!is_safe_container_ref("api;rm -rf /"));
        assert_eq!(shell_quote("api_1"), "'api_1'");
    }

    #[test]
    fn restart_command_is_bounded_and_shell_safe() {
        assert_eq!(
            docker_restart_command("api_1", 15),
            "docker restart --time 15 -- 'api_1'"
        );
    }
}
