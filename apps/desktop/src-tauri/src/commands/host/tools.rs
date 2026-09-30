//! Host tool executors.
//!
//! 每个 `host.tool.execute` 里的工具名对应这里的一个执行器：`server.info`、
//! `docker.*`、`systemd.*`、`package.*` 与 `filesystem.*`。它们共享同一条会话前缀
//! （`ensure_session_with_cancel` → `ensure_session`）和同一套失败映射
//! （`transport_or_cancel` / `filesystem_failure` / `cancelled_failure`）。
//!
//! 从 `host.rs` 拆出来的理由：这一段与调查证据、计划、上下文无关 —— 它只负责
//! 「把一个已校验的工具输入变成一次远端执行与一个有界结果」。`host.rs` 根模块保留
//! 协议分发与共享类型，这里只放执行体。

use super::*;
use crate::state::{ServerExecExecutionBinding, ServerExecTicketError};
use yukinal_database::repositories::TaskCommandBudgetReservation;

const SERVER_EXEC_MAX_COMMAND_CHARS: usize = 16_384;
const SERVER_EXEC_MAX_PURPOSE_CHARS: usize = 512;
const SERVER_EXEC_MAX_TIMEOUT_MS: u64 = 120_000;
const SERVER_EXEC_MAX_OUTPUT_BYTES: usize = 65_536;
const SERVER_EXEC_MAX_ENV_VARS: usize = 16;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ServerExecInput {
    command: String,
    purpose: String,
    timeout_ms: u64,
    max_output_bytes: usize,
    workdir: Option<String>,
    env: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerExecResult {
    state: &'static str,
    exit_code: i32,
    stdout: String,
    stderr: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
    duration_ms: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerExecInterruption {
    state: &'static str,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
    duration_ms: u64,
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

pub(super) async fn server_exec(
    state: &AppState,
    server_id: &str,
    request: &HostToolExecuteRequest,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<ServerExecInput>(request.input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("server.exec input is invalid: {error}"),
                true,
                None,
            ));
        }
    };
    if let Err(message) = validate_server_exec_input(&input) {
        return Ok(failed("invalid_input", message, true, None));
    }
    if request.task_id.as_deref().is_none_or(str::is_empty) {
        return Ok(failed(
            "denied_by_policy",
            "server.exec requires a durable task and approved plan step",
            false,
            None,
        ));
    }
    let task_id = request.task_id.as_deref().unwrap_or_default();
    let task = match state.database.investigations().get_task(task_id) {
        Ok(task) => task,
        Err(error) => {
            return Ok(failed(
                "not_found",
                format!("investigation task is unavailable: {error}"),
                false,
                None,
            ));
        }
    };
    let server = match state.database.servers().get(server_id) {
        Ok(server) => server,
        Err(error) => {
            return Ok(failed(
                "not_found",
                format!("target server is unavailable: {error}"),
                false,
                None,
            ));
        }
    };
    if task.scope.host != InvestigationTargetHost::Remote
        || task.scope.server_id.as_deref() != Some(server_id)
        || task.scope.environment != request.target.environment
        || server.metadata.environment != request.target.environment
    {
        return Ok(failed(
            "denied_by_policy",
            "server.exec target no longer matches the task scope and registered server environment",
            false,
            None,
        ));
    }
    if let Some(violation) = task_guardrail_violation(&task, SERVER_EXEC, &request.input) {
        return Ok(failed("denied_by_policy", violation.message, false, None));
    }
    let command_is_critical = is_critical_server_command(&input.command);
    let grant = task.guardrails.command_grant.as_ref().filter(|grant| {
        task.last_failure
            .as_ref()
            .is_none_or(|failure| failure.code != TaskFailureCode::OutcomeUnknown)
            && task.mode == InvestigationRunMode::Goal
            && task.permission_mode == InvestigationPermissionMode::Auto
            && task.automation_level == TaskAutomationLevel::Execute
            && grant.granted_by == task.created_by
            && grant.granted_by == "user"
            && grant.task_id == task.id
            && grant.server_id == server_id
            && grant.environment == request.target.environment
            && matches!(
                grant.environment,
                Environment::Development | Environment::Staging
            )
            && yukinal_time::parse_iso8601_utc(&grant.expires_at)
                .is_some_and(|expiry| yukinal_time::now_epoch_seconds() < expiry)
    });
    // Automatic task grants remain a separate, bounded authorization path. If the
    // caller carries an approval id, it must use that exact one-time host ticket; a
    // grant must never turn a forged or mismatched approval id into an allowed call.
    let using_task_grant = grant.is_some() && !command_is_critical && request.approval_id.is_none();
    if !using_task_grant {
        let Some(approval_id) = request.approval_id.as_deref() else {
            return Ok(failed(
                "denied_by_policy",
                if command_is_critical {
                    "critical command patterns cannot use a task command grant and require approval for this exact input"
                } else {
                    "server.exec requires either this task's valid command grant or approval for this exact input"
                },
                false,
                Some(json!({ "criticalPatternMatched": command_is_critical })),
            ));
        };
        if cancel.is_cancelled() {
            return Ok(cancelled_failure());
        }
        let input_fingerprint = match server_exec_input_fingerprint(&request.input) {
            Ok(fingerprint) => fingerprint,
            Err(error) => return Ok(failed("internal", error, false, None)),
        };
        let execution_binding = ServerExecExecutionBinding {
            approval_id,
            run_id: request.run_id.as_deref(),
            trace_id: &request.trace_id,
            call_id: &request.call_id,
            tool_name: &request.tool_name,
            input_fingerprint: &input_fingerprint,
            target_host: &request.target.host,
            server_id: request.target.server_id.as_deref(),
            workspace_id: request.target.workspace_id.as_deref(),
            environment: request.target.environment.as_str(),
            task_id: request.task_id.as_deref(),
            plan_id: request.plan_id.as_deref(),
            plan_step_id: request.plan_step_id.as_deref(),
            evidence_ids: request.evidence_ids.clone(),
        };
        if let Err(error) = state
            .server_exec_approvals
            .consume(execution_binding, yukinal_time::now_epoch_seconds)
            .await
        {
            let message = match error {
                ServerExecTicketError::Missing => {
                    "server.exec has no host-issued approval ticket for this call"
                }
                ServerExecTicketError::Expired => "server.exec approval ticket has expired",
                ServerExecTicketError::Mismatch => {
                    "server.exec request does not match the approved run, call, tool, target, plan, or input"
                }
                ServerExecTicketError::NotApproved => {
                    "server.exec approval has not been accepted by the host"
                }
            };
            return Ok(failed(
                "denied_by_policy",
                message,
                false,
                Some(json!({ "code": "approval_ticket" })),
            ));
        }
    }
    if cancel.is_cancelled() {
        return Ok(cancelled_failure());
    }
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let session = match state.terminals.cached_session(server_id) {
        Ok(session) => session,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    if let (true, Some(grant)) = (using_task_grant, grant) {
        match state.database.investigations().reserve_task_command_budget(
            TaskCommandBudgetReservation {
                task_id,
                grant_id: &grant.grant_id,
                server_id,
                environment: request.target.environment,
                duration_ms: input.timeout_ms,
                output_bytes: input.max_output_bytes as u64,
                updated_at: &yukinal_core::sidecar::iso8601_now(),
            },
        ) {
            Ok(_) => {}
            Err(error) => {
                return Ok(failed(
                    "denied_by_policy",
                    format!("task command delegation refused this call: {error}"),
                    false,
                    Some(json!({ "code": "task_command_budget" })),
                ));
            }
        }
    }

    let remote_command = server_exec_command(&input);
    let started = std::time::Instant::now();
    let result = state
        .ssh
        .execute_bounded_once(
            &session,
            &remote_command,
            Some(std::time::Duration::from_millis(input.timeout_ms)),
            input.max_output_bytes,
            cancel,
        )
        .await;
    let duration_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let result = match result {
        Ok(result) if result.exit_code >= 0 => result,
        Ok(result) => {
            let output = ServerExecInterruption {
                state: "result_unknown",
                exit_code: None,
                stdout: result.stdout_lossy(),
                stderr: result.stderr_lossy(),
                stdout_truncated: result.stdout_truncated,
                stderr_truncated: result.stderr_truncated,
                duration_ms,
            };
            return Ok(failed(
                "execution_failed",
                "remote command returned without an exit status; its effect is unknown",
                false,
                Some(serde_json::to_value(output).map_err(|error| error.to_string())?),
            ));
        }
        Err(yukinal_ssh::Error::Timeout) => {
            let output = server_exec_interruption("timed_out", duration_ms);
            return Ok(failed(
                "timeout",
                "remote command exceeded its host-enforced timeout; its effect may have occurred",
                false,
                Some(serde_json::to_value(output).map_err(|error| error.to_string())?),
            ));
        }
        Err(yukinal_ssh::Error::Cancelled) if cancel.is_cancelled() => {
            let output = server_exec_interruption("cancelled", duration_ms);
            return Ok(failed(
                "cancelled",
                "remote command was cancelled; its effect may have occurred",
                false,
                Some(serde_json::to_value(output).map_err(|error| error.to_string())?),
            ));
        }
        Err(error) => {
            let output = server_exec_interruption("result_unknown", duration_ms);
            return Ok(failed(
                "transport",
                format!("SSH command result is unavailable: {error}; its effect may have occurred"),
                false,
                Some(serde_json::to_value(output).map_err(|error| error.to_string())?),
            ));
        }
    };

    let output = ServerExecResult {
        state: "completed",
        exit_code: result.exit_code,
        stdout: result.stdout_lossy(),
        stderr: result.stderr_lossy(),
        stdout_truncated: result.stdout_truncated,
        stderr_truncated: result.stderr_truncated,
        duration_ms,
    };
    let output_value = serde_json::to_value(output).map_err(|error| error.to_string())?;
    if result.exit_code != 0 {
        return Ok(failed(
            "execution_failed",
            format!("remote command exited with status {}", result.exit_code),
            false,
            Some(output_value),
        ));
    }
    Ok(success(output_value))
}

fn server_exec_interruption(state: &'static str, duration_ms: u64) -> ServerExecInterruption {
    ServerExecInterruption {
        state,
        exit_code: None,
        stdout: String::new(),
        stderr: String::new(),
        stdout_truncated: false,
        stderr_truncated: false,
        duration_ms,
    }
}

fn validate_server_exec_input(input: &ServerExecInput) -> std::result::Result<(), String> {
    if input.command.trim().is_empty()
        || input.command.chars().count() > SERVER_EXEC_MAX_COMMAND_CHARS
        || input.command.contains('\0')
    {
        return Err(format!(
            "command must contain 1 to {SERVER_EXEC_MAX_COMMAND_CHARS} characters and no NUL"
        ));
    }
    if input.purpose.trim().is_empty()
        || input.purpose.chars().count() > SERVER_EXEC_MAX_PURPOSE_CHARS
        || input.purpose.chars().any(char::is_control)
    {
        return Err(format!(
            "purpose must contain 1 to {SERVER_EXEC_MAX_PURPOSE_CHARS} printable characters"
        ));
    }
    if input.timeout_ms == 0 || input.timeout_ms > SERVER_EXEC_MAX_TIMEOUT_MS {
        return Err(format!(
            "timeoutMs must be between 1 and {SERVER_EXEC_MAX_TIMEOUT_MS}"
        ));
    }
    if input.max_output_bytes == 0 || input.max_output_bytes > SERVER_EXEC_MAX_OUTPUT_BYTES {
        return Err(format!(
            "maxOutputBytes must be between 1 and {SERVER_EXEC_MAX_OUTPUT_BYTES}"
        ));
    }
    if let Some(workdir) = input.workdir.as_deref() {
        if workdir.len() > 4_096
            || !workdir.starts_with('/')
            || workdir.contains('\0')
            || workdir.chars().any(char::is_control)
            || workdir.split('/').any(|part| part == "." || part == "..")
        {
            return Err("workdir must be a bounded absolute POSIX path without dot traversal or control characters".into());
        }
    }
    if let Some(env) = input.env.as_ref() {
        if env.len() > SERVER_EXEC_MAX_ENV_VARS {
            return Err(format!(
                "env may contain at most {SERVER_EXEC_MAX_ENV_VARS} variables"
            ));
        }
        for (key, value) in env {
            if !matches!(
                key.as_str(),
                "LANG"
                    | "LC_ALL"
                    | "LC_CTYPE"
                    | "LC_MESSAGES"
                    | "LC_TIME"
                    | "LC_NUMERIC"
                    | "LC_COLLATE"
                    | "TZ"
                    | "TERM"
            ) || value.len() > 256
                || value.chars().any(char::is_control)
            {
                return Err(format!(
                    "environment variable `{key}` is not on the non-sensitive allowlist or has an invalid value"
                ));
            }
        }
    }
    Ok(())
}

fn server_exec_command(input: &ServerExecInput) -> String {
    let mut command = String::new();
    if let Some(workdir) = input.workdir.as_deref() {
        command.push_str("cd ");
        command.push_str(&shell_quote(workdir));
        command.push_str(" && ");
    }
    if let Some(env) = input.env.as_ref().filter(|env| !env.is_empty()) {
        command.push_str("env");
        for (key, value) in env {
            command.push(' ');
            command.push_str(key);
            command.push('=');
            command.push_str(&shell_quote(value));
        }
        command.push_str(" /bin/sh -c ");
    } else {
        command.push_str("/bin/sh -c ");
    }
    // SSH exec carries a shell command string. Quoting this inner shell invocation only
    // preserves boundaries for workdir/env wrappers; it does not make arbitrary remote
    // shell text safe or equivalent to an argv vector.
    command.push_str(&shell_quote(&input.command));
    command
}

fn is_critical_server_command(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    let words = lower.split_whitespace().collect::<Vec<_>>();
    let has_recursive_force_delete = words.iter().enumerate().any(|(index, word)| {
        if word.trim_matches(|character: char| !character.is_ascii_alphanumeric()) != "rm" {
            return false;
        }
        let arguments = words
            .iter()
            .skip(index + 1)
            .take_while(|argument| !matches!(**argument, ";" | "&&" | "||" | "|" | "&"));
        let mut has_recursive_or_force = false;
        let mut deletes_root = false;
        for argument in arguments {
            let argument = argument.trim_matches(|character: char| {
                matches!(character, '\'' | '"' | '`' | '(' | ')' | ';' | '&' | '|')
            });
            if argument.starts_with('-')
                && (argument
                    .chars()
                    .skip(1)
                    .any(|flag| matches!(flag, 'r' | 'f'))
                    || matches!(argument, "--recursive" | "--force"))
            {
                has_recursive_or_force = true;
            }
            if matches!(argument, "/" | "/*" | "/.") || argument.starts_with("/*/") {
                deletes_root = true;
            }
        }
        has_recursive_or_force || deletes_root
    });
    let mut sql_without_comments = String::new();
    let mut remaining_sql = lower.as_str();
    while let Some(comment_start) = remaining_sql.find("/*") {
        sql_without_comments.push_str(&remaining_sql[..comment_start]);
        let comment = &remaining_sql[comment_start + 2..];
        let Some(comment_end) = comment.find("*/") else {
            remaining_sql = "";
            break;
        };
        remaining_sql = &comment[comment_end + 2..];
        sql_without_comments.push(' ');
    }
    sql_without_comments.push_str(remaining_sql);
    let sql_tokens = sql_without_comments
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    let no_whitespace = lower
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    has_recursive_force_delete
        || words.iter().any(|word| word.starts_with("mkfs"))
        || sql_tokens
            .windows(2)
            .any(|pair| pair == ["drop", "database"])
        || no_whitespace.contains("of=/dev/")
        || ["sd", "nvme", "hd", "vd"]
            .iter()
            .any(|device| no_whitespace.contains(&format!(">/dev/{device}")))
        || words.iter().enumerate().any(|(index, word)| {
            word.trim_matches(|character: char| !character.is_ascii_alphanumeric()) == "chmod"
                && words.iter().skip(index + 1).any(|argument| {
                    argument.starts_with('-') && argument.to_ascii_lowercase().contains('r')
                })
                && words.iter().skip(index + 1).any(|argument| {
                    matches!(
                        argument
                            .trim_matches(|character: char| matches!(character, '\'' | '"' | '`')),
                        "/" | "/*"
                    )
                })
        })
}

/// File-capability failure → the host tool result's failure code.
///
/// The codes themselves (`invalid_input` / `denied_by_policy`) belong to the sidecar's host
/// protocol, not to the file capability, so the mapping stays here while the rules, the messages
/// and the limits live in `yukinal-filesystem`. `retryable` follows the existing table: a bad
/// argument can be fixed by the Agent, a policy denial cannot.
///
/// The two edit refusals ride on the existing `invalid_input` code — the vocabulary has no
/// edit-specific code, and both messages say what to do next:
/// - a **revision mismatch** is exactly "your input is stale, re-read and retry", so it is
///   retryable, and `detail` carries both revision strings so the Agent can see the drift;
/// - a file **over the edit cap** cannot be fixed by retrying anything (the file has to shrink),
///   so it is `retryable: false`: the fix is a different tool, not another attempt.
pub(super) fn filesystem_failure(error: FilesystemError, cancel: &CancellationToken) -> Value {
    // The wording belongs to the capability, so it is taken from the typed error itself instead of
    // being re-written here, where it could drift away from the rule it explains.
    let message = error.to_string();
    match error {
        FilesystemError::InvalidInput(_) => failed("invalid_input", message, true, None),
        FilesystemError::DeniedByPolicy(_) => failed("denied_by_policy", message, false, None),
        FilesystemError::RevisionMismatch { expected, actual } => failed(
            "invalid_input",
            message,
            true,
            Some(json!({ "expectedRevision": expected, "actualRevision": actual })),
        ),
        FilesystemError::FileTooLargeToEdit { limit } => failed(
            "invalid_input",
            message,
            false,
            Some(json!({ "maxEditableBytes": limit })),
        ),
        FilesystemError::FileTooLargeToBackup { limit } => failed(
            "invalid_input",
            message,
            false,
            Some(json!({ "maxBackupBytes": limit })),
        ),
        // 「远端做不到安全替换」：重试多少次都一样（要变的是服务器或这个文件），所以它有一个
        // 自己的码 —— 把它报成 invalid_input 会让模型把「换条路重试」当成正确反应。
        FilesystemError::UnsafeRemoteWrite(_) => failed("unsupported", message, false, None),
        // 并发修改：与过期的 revision 同一类，下一步都是「重新读，再带着新 revision 重试」。
        FilesystemError::ConcurrentChange(_) => failed("invalid_input", message, true, None),
        FilesystemError::MetadataNotPreserved { missing, .. } => failed(
            "unsupported",
            message,
            false,
            Some(json!({ "missingMetadata": missing })),
        ),
        FilesystemError::Transport(error) => transport_or_cancel(error, cancel),
    }
}

pub(super) async fn server_info(
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
            cancel,
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

pub(super) async fn server_logs(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<ServerLogsInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("server.logs input is invalid: {error}"),
                true,
                None,
            ));
        }
    };
    let command = match log_discovery_command_for(&input) {
        Ok(command) => command,
        Err(error) => return Ok(failed("invalid_input", error, true, None)),
    };
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let session = match state.terminals.cached_session(server_id) {
        Ok(session) => session,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
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
    let parsed = match parse_logs_output(&result.stdout_lossy()) {
        Ok(parsed) => parsed,
        Err(error) => {
            return Ok(failed(
                "execution_failed",
                format!("server.logs returned an invalid response: {error}"),
                false,
                Some(json!({ "exitCode": result.exit_code })),
            ));
        }
    };
    if (input.since_seconds.is_some() || input.unit.is_some())
        && parsed.source != crate::commands::logs::LogSource::Journalctl
    {
        return Ok(failed(
            "unsupported",
            "server.logs filters require journalctl; the fallback log source was not returned unfiltered",
            false,
            Some(json!({ "source": parsed.source })),
        ));
    }
    Ok(success(
        serde_json::to_value(parsed).map_err(|error| error.to_string())?,
    ))
}

pub(super) async fn server_services(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<ServerServicesInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("server.services input is invalid: {error}"),
                true,
                None,
            ));
        }
    };
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let session = match state.terminals.cached_session(server_id) {
        Ok(session) => session,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    let result = match state
        .ssh
        .execute(
            &session,
            service_discovery_command(),
            Some(std::time::Duration::from_secs(10)),
            cancel,
        )
        .await
    {
        Ok(result) => result,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    let mut parsed = match parse_services_output(&result.stdout_lossy()) {
        Ok(parsed) => parsed,
        Err(error) => {
            return Ok(failed(
                "execution_failed",
                format!("server.services returned an invalid response: {error}"),
                false,
                Some(json!({ "exitCode": result.exit_code })),
            ));
        }
    };
    if let Err(error) = filter_services(&mut parsed, &input) {
        return Ok(failed("invalid_input", error, true, None));
    }
    Ok(success(
        serde_json::to_value(parsed).map_err(|error| error.to_string())?,
    ))
}

pub(super) async fn filesystem_read(
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
            ));
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
            revision: read.revision,
        })
        .map_err(|error| error.to_string())?,
    ))
}

pub(super) async fn filesystem_write(
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
            ));
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

/// `filesystem.edit`: read the file, verify its revision, replace one exact match, write it back.
///
/// All of that happens inside one capability call (`RemoteFileService::agent_edit`) and is
/// cancellable as a whole, because a half-applied edit is exactly what the guard exists to
/// prevent. The checks themselves (cap, revision, single match) live in `yukinal-filesystem`.
pub(super) async fn filesystem_edit(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<FilesystemEditInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("filesystem.edit input is invalid: {error}"),
                true,
                None,
            ));
        }
    };
    let request = match AgentEditRequest::check(
        &input.path,
        &input.expected_revision,
        input.old_string,
        input.new_string,
    ) {
        Ok(request) => request,
        Err(error) => return Ok(filesystem_failure(error, cancel)),
    };
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let service = remote_file_service(state);
    let edit = tokio::select! {
        result = service.agent_edit(server_id, &request) => match result {
            Ok(edit) => edit,
            Err(error) => return Ok(filesystem_failure(error, cancel)),
        },
        _ = cancel.cancelled() => return Ok(cancelled_failure()),
    };
    Ok(success(
        serde_json::to_value(FilesystemEditResult {
            path: edit.path,
            revision: edit.revision,
            bytes_before: edit.bytes_before,
            bytes_after: edit.bytes_after,
            line_delta: edit.line_delta,
        })
        .map_err(|error| error.to_string())?,
    ))
}

/// Generate a single-use token for a host-owned sibling backup path. The token never comes from the
/// model, and `AgentBackupRequest::check` still validates its exact shape before SFTP is touched.
pub(super) fn backup_token() -> String {
    let mut bytes = [0_u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) fn backup_record_id(server_id: &str, backup_path: &str) -> String {
    format!(
        "backup_{:x}",
        Sha256::digest(format!("{server_id}\0{backup_path}").as_bytes())
    )
}

pub(super) fn backup_owner_matches_request(
    record: &FilesystemBackupRecord,
    request: &HostToolExecuteRequest,
) -> bool {
    match (record.task_id.as_deref(), request.task_id.as_deref()) {
        (Some(owner), Some(request_task)) => owner == request_task,
        (None, None) => true,
        _ => false,
    }
}

/// Read the host-owned backup ledger for the current investigation task.
///
/// This never opens an SSH session and never proves that the remote sibling still exists.  The
/// ledger is intentionally presented as metadata so an Agent can explain what may be cleaned or
/// restored before a separately approved action performs a remote check.
pub(super) async fn filesystem_backup_list(
    state: &AppState,
    server_id: &str,
    input: &Value,
    request_context: &HostToolExecuteRequest,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<FilesystemBackupListInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("filesystem.backup.list input is invalid: {error}"),
                false,
                None,
            ));
        }
    };
    let Some(task_id) = request_context.task_id.as_deref() else {
        return Ok(failed(
            "invalid_input",
            "filesystem.backup.list requires a durable investigation task",
            false,
            None,
        ));
    };
    let task = match state.database.investigations().get_task(task_id) {
        Ok(task) => task,
        Err(DatabaseError::NotFound) => {
            return Ok(failed(
                "not_found",
                format!("investigation task `{task_id}` was not found"),
                false,
                None,
            ));
        }
        Err(error) => return Ok(failed("internal", error.to_string(), false, None)),
    };
    if task.server_id.as_deref() != Some(server_id) {
        return Ok(failed(
            "denied_by_policy",
            "filesystem.backup.list target does not match the task's registered server",
            false,
            None,
        ));
    }
    if let Some(path) = input.path.as_deref() {
        if let Err(error) = validate_remote_path(path) {
            return Ok(failed("invalid_input", error, false, None));
        }
    }
    let status = match input.status.as_deref() {
        None => None,
        Some(raw) => match FilesystemBackupStatus::parse(raw) {
            Some(status) => Some(status),
            None => {
                return Ok(failed(
                    "invalid_input",
                    "filesystem.backup.list status must be available, restored, or deleted",
                    false,
                    None,
                ));
            }
        },
    };
    let limit = input.limit.unwrap_or(64);
    if !(1..=128).contains(&limit) {
        return Ok(failed(
            "invalid_input",
            "filesystem.backup.list limit must be between 1 and 128",
            false,
            None,
        ));
    }
    let (backups, truncated) = match state.database.filesystem_backups().list_for_task(
        server_id,
        task_id,
        status,
        input.path.as_deref(),
        limit,
    ) {
        Ok(result) => result,
        Err(DatabaseError::Validation(error)) => {
            return Ok(failed("invalid_input", error, false, None));
        }
        Err(error) => return Ok(failed("internal", error.to_string(), false, None)),
    };
    let backups = backups
        .into_iter()
        .map(|backup| FilesystemBackupLedgerItemResult {
            id: backup.id,
            server_id: backup.server_id,
            task_id: backup.task_id.unwrap_or_else(|| task_id.to_string()),
            path: backup.path,
            backup_path: backup.backup_path,
            revision: backup.revision,
            bytes_backed_up: backup.bytes_backed_up,
            status: backup.status.as_str().to_string(),
            created_at: backup.created_at,
            updated_at: backup.updated_at,
            restored_at: backup.restored_at,
            deleted_at: backup.deleted_at,
        })
        .collect();
    Ok(success(
        serde_json::to_value(FilesystemBackupListResult { backups, truncated })
            .map_err(|error| error.to_string())?,
    ))
}

/// Hard bound on how many `available` rows one retention scan reads. This is a planning
/// read, not a cleanup: it must return promptly even on a long-lived ledger.
const MAX_RETENTION_SCAN: usize = 512;
/// The retention result never lists more than this many candidates at once.
const MAX_RETENTION_CANDIDATES: usize = 32;

/// Plan a cross-task backup rotation without touching the remote target (ADR 0076).
///
/// This is `filesystem.backup.retention`: a read-only, host-owned ledger query. It returns
/// the exact backups that a `backup_rotation` step would remove; it never deletes, and it
/// never probes the remote sibling, so a candidate is a plan input, not a proof.
pub(super) async fn filesystem_backup_retention(
    state: &AppState,
    server_id: &str,
    input: &Value,
    _request_context: &HostToolExecuteRequest,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<FilesystemBackupRetentionInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("filesystem.backup.retention input is invalid: {error}"),
                false,
                None,
            ));
        }
    };
    if input.keep_latest.is_none() && input.older_than_days.is_none() {
        return Ok(failed(
            "invalid_input",
            "filesystem.backup.retention requires keepLatest or olderThanDays",
            false,
            None,
        ));
    }
    if input
        .keep_latest
        .is_some_and(|keep| !(1..=64).contains(&keep))
    {
        return Ok(failed(
            "invalid_input",
            "filesystem.backup.retention keepLatest must be between 1 and 64",
            false,
            None,
        ));
    }
    if input
        .older_than_days
        .is_some_and(|days| !(1..=3650).contains(&days))
    {
        return Ok(failed(
            "invalid_input",
            "filesystem.backup.retention olderThanDays must be between 1 and 3650",
            false,
            None,
        ));
    }
    if let Some(prefix) = input.path_prefix.as_deref() {
        if let Err(error) = validate_remote_path(prefix) {
            return Ok(failed("invalid_input", error, false, None));
        }
    }
    let (records, scan_truncated) = match state
        .database
        .filesystem_backups()
        .list_available_for_server_with_path_prefix(
            server_id,
            input.path_prefix.as_deref(),
            MAX_RETENTION_SCAN,
        ) {
        Ok(result) => result,
        Err(DatabaseError::Validation(error)) => {
            return Ok(failed("invalid_input", error, false, None));
        }
        Err(error) => return Ok(failed("internal", error.to_string(), false, None)),
    };
    let filtered: Vec<_> = records
        .into_iter()
        .filter(|record| {
            input
                .path_prefix
                .as_deref()
                .is_none_or(|prefix| record.path.starts_with(prefix))
        })
        .collect();
    let scanned_count = filtered.len();
    let (mut candidates, kept_count) = retention_candidates(
        filtered,
        input.keep_latest,
        input.older_than_days,
        yukinal_time::now_epoch_seconds(),
    );
    let candidate_truncated = candidates.len() > MAX_RETENTION_CANDIDATES;
    if candidate_truncated {
        candidates.truncate(MAX_RETENTION_CANDIDATES);
    }
    let candidates = candidates
        .into_iter()
        .map(|backup| FilesystemBackupRetentionCandidateResult {
            path: backup.path,
            backup_path: backup.backup_path,
            revision: backup.revision,
            task_id: backup.task_id,
            created_at: backup.created_at,
            bytes_backed_up: backup.bytes_backed_up,
        })
        .collect();
    Ok(success(
        serde_json::to_value(FilesystemBackupRetentionResult {
            candidates,
            truncated: scan_truncated || candidate_truncated,
            kept_count,
            scanned_count,
        })
        .map_err(|error| error.to_string())?,
    ))
}

/// Newest-first per original path, then keep/delete selection. Pure, so the table can be
/// tested without a database or a target. When both filters are given a record must satisfy
/// **both** (intersection), which is the conservative reading of "keep N and also require
/// age".
pub(super) fn retention_candidates(
    mut records: Vec<FilesystemBackupRecord>,
    keep_latest: Option<usize>,
    older_than_days: Option<u64>,
    now_epoch_seconds: u64,
) -> (Vec<FilesystemBackupRecord>, usize) {
    records.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| right.created_at.cmp(&left.created_at))
            .then_with(|| right.id.cmp(&left.id))
    });
    let cutoff =
        older_than_days.map(|days| now_epoch_seconds.saturating_sub(days.saturating_mul(86_400)));
    let mut candidates = Vec::new();
    let mut index_in_path = 0usize;
    let mut previous_path: Option<&str> = None;
    for record in &records {
        if previous_path != Some(record.path.as_str()) {
            previous_path = Some(record.path.as_str());
            index_in_path = 0;
        }
        let beyond_keep = keep_latest.is_some_and(|limit| index_in_path >= limit);
        let old_enough = cutoff.is_some_and(|cutoff| {
            yukinal_time::parse_iso8601_utc(&record.created_at)
                .is_some_and(|created| created < cutoff)
        });
        let is_candidate = match (keep_latest.is_some(), older_than_days.is_some()) {
            (true, true) => beyond_keep && old_enough,
            (true, false) => beyond_keep,
            (false, true) => old_enough,
            (false, false) => false,
        };
        if is_candidate {
            candidates.push(record.clone());
        }
        index_in_path += 1;
    }
    let kept = records.len().saturating_sub(candidates.len());
    (candidates, kept)
}

pub(super) async fn filesystem_backup(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
    request_context: &HostToolExecuteRequest,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<FilesystemBackupInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("filesystem.backup input is invalid: {error}"),
                true,
                None,
            ));
        }
    };
    let request = match AgentBackupRequest::check(&input.path, &backup_token()) {
        Ok(request) => request,
        Err(error) => return Ok(filesystem_failure(error, cancel)),
    };
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let service = remote_file_service(state);
    let backup = tokio::select! {
        result = service.agent_backup(server_id, &request) => match result {
            Ok(backup) => backup,
            Err(error) => return Ok(filesystem_failure(error, cancel)),
        },
        _ = cancel.cancelled() => return Ok(cancelled_failure()),
    };
    let now = yukinal_core::sidecar::iso8601_now();
    let bytes_backed_up = match i64::try_from(backup.bytes_backed_up) {
        Ok(bytes) => bytes,
        Err(_) => {
            return Ok(failed(
                "internal",
                "filesystem backup byte count exceeded the local ledger range",
                false,
                None,
            ));
        }
    };
    let record = FilesystemBackupRecord {
        id: backup_record_id(server_id, &backup.backup_path),
        server_id: server_id.to_string(),
        task_id: request_context.task_id.clone(),
        plan_id: request_context.plan_id.clone(),
        plan_step_id: request_context.plan_step_id.clone(),
        trace_id: Some(request_context.trace_id.clone()),
        call_id: Some(request_context.call_id.clone()),
        path: backup.path.clone(),
        backup_path: backup.backup_path.clone(),
        revision: backup.revision.clone(),
        bytes_backed_up,
        status: FilesystemBackupStatus::Available,
        created_at: now.clone(),
        updated_at: now,
        restored_at: None,
        deleted_at: None,
    };
    if let Err(error) = state.database.filesystem_backups().insert(&record) {
        return Ok(failed(
            "internal",
            "backup was created remotely but could not be registered in the host ledger; preserve it and reconcile before restoring",
            false,
            Some(json!({ "database": error.to_string(), "backupPath": backup.backup_path })),
        ));
    }
    Ok(success(
        serde_json::to_value(FilesystemBackupResult {
            path: backup.path,
            backup_path: backup.backup_path,
            revision: backup.revision,
            bytes_backed_up: backup.bytes_backed_up,
        })
        .map_err(|error| error.to_string())?,
    ))
}

pub(super) async fn filesystem_backup_cleanup(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
    request_context: &HostToolExecuteRequest,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<FilesystemBackupCleanupInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("filesystem.backup.cleanup input is invalid: {error}"),
                true,
                None,
            ));
        }
    };
    match input.resolve() {
        Ok(BackupCleanupRequest::Single(item)) => {
            filesystem_backup_cleanup_single(state, server_id, item, cancel, request_context).await
        }
        Ok(BackupCleanupRequest::Batch(items)) => {
            filesystem_backup_cleanup_batch(state, server_id, items, cancel, request_context).await
        }
        Err(error) => Ok(failed("invalid_input", error, false, None)),
    }
}

async fn filesystem_backup_cleanup_single(
    state: &AppState,
    server_id: &str,
    input: FilesystemBackupCleanupItemInput,
    cancel: &CancellationToken,
    request_context: &HostToolExecuteRequest,
) -> Result<Value, String> {
    let request = match AgentCleanupBackupRequest::check(
        &input.path,
        &input.backup_path,
        &input.expected_revision,
    ) {
        Ok(request) => request,
        Err(error) => return Ok(filesystem_failure(error, cancel)),
    };
    let backup = match state.database.filesystem_backups().find_available(
        server_id,
        &input.path,
        &input.backup_path,
    ) {
        Ok(Some(record)) => record,
        Ok(None) => {
            return Ok(failed(
                "denied_by_policy",
                "filesystem.backup.cleanup requires an available backup previously created by this host for the same server and target path",
                false,
                None,
            ));
        }
        Err(error) => {
            return Ok(failed(
                "internal",
                format!("could not read the filesystem backup ledger: {error}"),
                false,
                None,
            ));
        }
    };
    if !backup_owner_matches_request(&backup, request_context) {
        return Ok(failed(
            "denied_by_policy",
            "filesystem.backup.cleanup cannot use a backup owned by another investigation task",
            false,
            Some(json!({
                "backupTaskId": backup.task_id,
                "requestTaskId": request_context.task_id,
            })),
        ));
    }
    if !backup
        .revision
        .eq_ignore_ascii_case(&input.expected_revision)
    {
        return Ok(failed(
            "invalid_input",
            "expectedRevision does not match the revision recorded for this backup; preserve it and re-read the ledger",
            false,
            Some(json!({
                "expectedRevision": backup.revision,
                "actualRevision": input.expected_revision,
            })),
        ));
    }
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let service = remote_file_service(state);
    let cleanup = tokio::select! {
        result = service.agent_cleanup_backup(server_id, &request) => match result {
            Ok(cleanup) => cleanup,
            Err(error) => return Ok(filesystem_failure(error, cancel)),
        },
        _ = cancel.cancelled() => return Ok(cancelled_failure()),
    };
    if let Err(error) = state.database.filesystem_backups().mark_deleted(
        server_id,
        &cleanup.path,
        &cleanup.backup_path,
        &yukinal_core::sidecar::iso8601_now(),
    ) {
        return Ok(failed(
            "internal",
            "filesystem.backup.cleanup removed the remote backup but the host could not consume its ledger entry; reconcile before any further cleanup",
            false,
            Some(json!({ "database": error.to_string(), "backupPath": cleanup.backup_path })),
        ));
    }
    Ok(success(
        serde_json::to_value(FilesystemBackupCleanupResult {
            path: cleanup.path,
            backup_path: cleanup.backup_path,
            revision: cleanup.revision,
            bytes_deleted: cleanup.bytes_deleted,
        })
        .map_err(|error| error.to_string())?,
    ))
}

/// One batch item's outcome. `removed` is only ever produced after a verified remote delete
/// and a consumed ledger row; everything else is explicit.
fn cleanup_item_removed(
    item: &FilesystemBackupCleanupItemInput,
    revision: &str,
    bytes_deleted: usize,
) -> FilesystemBackupCleanupBatchItemResult {
    FilesystemBackupCleanupBatchItemResult {
        path: item.path.clone(),
        backup_path: item.backup_path.clone(),
        outcome: "removed".to_string(),
        revision: Some(revision.to_string()),
        bytes_deleted: Some(bytes_deleted),
        reason: None,
    }
}

fn cleanup_item_not_removed(
    item: &FilesystemBackupCleanupItemInput,
    outcome: &str,
    reason: impl Into<String>,
) -> FilesystemBackupCleanupBatchItemResult {
    FilesystemBackupCleanupBatchItemResult {
        path: item.path.clone(),
        backup_path: item.backup_path.clone(),
        outcome: outcome.to_string(),
        revision: None,
        bytes_deleted: None,
        reason: Some(reason.into()),
    }
}

/// A batch cleanup walks its items in order, re-reading the ledger for each one.
///
/// Cross-task deletion is allowed only when the current plan step is an approved
/// `backup_rotation` step that bound this exact item. Between items the cancellation token
/// is checked; once cancelled, the remaining items are reported as skipped rather than
/// silently omitted. Partial success is always visible in `partial`.
async fn filesystem_backup_cleanup_batch(
    state: &AppState,
    server_id: &str,
    items: Vec<FilesystemBackupCleanupItemInput>,
    cancel: &CancellationToken,
    request_context: &HostToolExecuteRequest,
) -> Result<Value, String> {
    // Validate every item before touching anything: one malformed item must not let the
    // earlier items run and then fail the whole call.
    let mut requests = Vec::with_capacity(items.len());
    for item in &items {
        match AgentCleanupBackupRequest::check(
            &item.path,
            &item.backup_path,
            &item.expected_revision,
        ) {
            Ok(request) => requests.push(request),
            Err(error) => return Ok(filesystem_failure(error, cancel)),
        }
    }

    let mut results = Vec::with_capacity(items.len());
    let mut session_ready = false;
    for (index, item) in items.iter().enumerate() {
        if cancel.is_cancelled() {
            results.push(cleanup_item_not_removed(item, "skipped", "cancelled"));
            continue;
        }
        let backup = match state.database.filesystem_backups().find_available(
            server_id,
            &item.path,
            &item.backup_path,
        ) {
            Ok(Some(record)) => record,
            Ok(None) => {
                results.push(cleanup_item_not_removed(
                    item,
                    "skipped",
                    "no available host ledger record for this server and backup path",
                ));
                continue;
            }
            Err(error) => {
                results.push(cleanup_item_not_removed(
                    item,
                    "failed",
                    format!("could not read the filesystem backup ledger: {error}"),
                ));
                continue;
            }
        };
        let cross_task = !backup_owner_matches_request(&backup, request_context);
        if cross_task && !cross_task_cleanup_is_plan_bound(state, request_context, item) {
            results.push(cleanup_item_not_removed(
                item,
                "skipped",
                "owned by another investigation task and not bound by the approved rotation step",
            ));
            continue;
        }
        if !backup
            .revision
            .eq_ignore_ascii_case(&item.expected_revision)
        {
            results.push(cleanup_item_not_removed(
                item,
                "failed",
                "expectedRevision does not match the revision recorded for this backup",
            ));
            continue;
        }
        if !session_ready {
            if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
                // One transport failure is very likely to repeat for every remaining item,
                // so stop and mark the rest explicitly instead of hammering a dead target.
                results.push(cleanup_item_not_removed(item, "failed", error));
                for remaining in items.iter().skip(index + 1) {
                    results.push(cleanup_item_not_removed(
                        remaining,
                        "skipped",
                        "transport failed for an earlier item",
                    ));
                }
                break;
            }
            session_ready = true;
        }
        let service = remote_file_service(state);
        let cleanup = tokio::select! {
            result = service.agent_cleanup_backup(server_id, &requests[index]) => match result {
                Ok(cleanup) => cleanup,
                Err(error) => {
                    results.push(cleanup_item_not_removed(
                        item,
                        "failed",
                        format!("{error}"),
                    ));
                    continue;
                }
            },
            _ = cancel.cancelled() => {
                results.push(cleanup_item_not_removed(item, "skipped", "cancelled"));
                continue;
            }
        };
        if let Err(error) = state.database.filesystem_backups().mark_deleted(
            server_id,
            &cleanup.path,
            &cleanup.backup_path,
            &yukinal_core::sidecar::iso8601_now(),
        ) {
            results.push(cleanup_item_not_removed(
                item,
                "failed",
                format!(
                    "removed the remote backup but could not consume its ledger entry: {error}"
                ),
            ));
            continue;
        }
        results.push(cleanup_item_removed(
            item,
            &cleanup.revision,
            cleanup.bytes_deleted,
        ));
    }

    let partial = results.iter().any(|item| item.outcome != "removed");
    Ok(success(
        serde_json::to_value(FilesystemBackupCleanupBatchResult {
            items: results,
            partial,
        })
        .map_err(|error| error.to_string())?,
    ))
}

/// Whether an approved rotation step bound this exact cross-task item.
///
/// The generic plan check already compares the whole `items` binding before execution; this
/// is the execution-time re-check, so a forged IPC request cannot delete another task's
/// backup by omitting the plan binding.
pub(super) fn cross_task_cleanup_is_plan_bound(
    state: &AppState,
    request_context: &HostToolExecuteRequest,
    item: &FilesystemBackupCleanupItemInput,
) -> bool {
    let (Some(task_id), Some(plan_id), Some(step_id)) = (
        request_context.task_id.as_deref(),
        request_context.plan_id.as_deref(),
        request_context.plan_step_id.as_deref(),
    ) else {
        return false;
    };
    let Ok(Some(plan)) = state.database.investigations().latest_plan(task_id) else {
        return false;
    };
    if plan.id != plan_id {
        return false;
    }
    let Some(step) = plan.steps.iter().find(|step| step.id == step_id) else {
        return false;
    };
    if step.kind != PlanStepKind::Action
        || !step.requires_approval
        || !step
            .allowed_tools
            .iter()
            .any(|tool| tool == FILESYSTEM_BACKUP_CLEANUP)
    {
        return false;
    }
    let Some(expected) = step
        .input_bindings
        .as_ref()
        .and_then(|bindings| bindings.get("items"))
    else {
        return false;
    };
    let Ok(expected_items) =
        serde_json::from_str::<Vec<FilesystemBackupCleanupItemInput>>(expected)
    else {
        return false;
    };
    expected_items.iter().any(|candidate| {
        candidate.path == item.path
            && candidate.backup_path == item.backup_path
            && candidate
                .expected_revision
                .eq_ignore_ascii_case(&item.expected_revision)
    })
}

pub(super) async fn filesystem_restore(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
    request_context: &HostToolExecuteRequest,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<FilesystemRestoreInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("filesystem.restore input is invalid: {error}"),
                true,
                None,
            ));
        }
    };
    let request =
        match AgentRestoreRequest::check(&input.path, &input.backup_path, &input.expected_revision)
        {
            Ok(request) => request,
            Err(error) => return Ok(filesystem_failure(error, cancel)),
        };
    let backup = match state.database.filesystem_backups().find_available(
        server_id,
        &input.path,
        &input.backup_path,
    ) {
        Ok(Some(record)) => record,
        Ok(None) => {
            return Ok(failed(
                "denied_by_policy",
                "filesystem.restore requires an available backup previously created by this host for the same server and target path",
                false,
                None,
            ));
        }
        Err(error) => {
            return Ok(failed(
                "internal",
                format!("could not read the filesystem backup ledger: {error}"),
                false,
                None,
            ));
        }
    };
    if !backup_owner_matches_request(&backup, request_context) {
        return Ok(failed(
            "denied_by_policy",
            "filesystem.restore cannot use a backup owned by another investigation task",
            false,
            Some(json!({
                "backupTaskId": backup.task_id,
                "requestTaskId": request_context.task_id,
            })),
        ));
    }
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let service = remote_file_service(state);
    let restore = tokio::select! {
        result = service.agent_restore(server_id, &request) => match result {
            Ok(restore) => restore,
            Err(error) => return Ok(filesystem_failure(error, cancel)),
        },
        _ = cancel.cancelled() => return Ok(cancelled_failure()),
    };
    if let Err(error) = state.database.filesystem_backups().mark_restored(
        server_id,
        &restore.path,
        &restore.backup_path,
        &yukinal_core::sidecar::iso8601_now(),
    ) {
        return Ok(failed(
            "internal",
            "filesystem.restore changed the remote file but the host could not consume its backup ledger entry; reconcile before any further restore",
            false,
            Some(json!({ "database": error.to_string(), "backupPath": restore.backup_path })),
        ));
    }
    Ok(success(
        serde_json::to_value(FilesystemRestoreResult {
            path: restore.path,
            backup_path: restore.backup_path,
            revision: restore.revision,
            bytes_before: restore.bytes_before,
            bytes_after: restore.bytes_after,
        })
        .map_err(|error| error.to_string())?,
    ))
}

pub(super) async fn docker_ps(
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
            ));
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

pub(super) async fn docker_logs(
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
            ));
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

pub(super) async fn docker_inspect(
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
            ));
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

pub(super) async fn docker_restart(
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
            ));
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

pub(super) async fn systemd_inspect(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<SystemdInspectInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("systemd.inspect input is invalid: {error}"),
                true,
                None,
            ));
        }
    };
    if !is_safe_systemd_service_ref(&input.service) {
        return Ok(failed(
            "invalid_input",
            "service must be a .service unit without shell metacharacters",
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
    let command = systemd_inspect_command(&input.service);
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
            format!(
                "systemd service `{}` was not found or could not be inspected",
                input.service
            ),
            false,
            Some(json!({
                "exitCode": result.exit_code,
                "stderr": truncate_text(&result.stderr_lossy(), 1_000),
            })),
        ));
    }
    let inspected = match parse_systemd_inspect(&result.stdout_lossy(), &input.service) {
        Ok(inspected) => inspected,
        Err(error) => return Ok(failed("execution_failed", error, false, None)),
    };
    Ok(success(
        serde_json::to_value(inspected).map_err(|error| error.to_string())?,
    ))
}

pub(super) async fn systemd_restart(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<SystemdRestartInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("systemd.restart input is invalid: {error}"),
                true,
                None,
            ));
        }
    };
    if !is_safe_systemd_service_ref(&input.service) {
        return Ok(failed(
            "invalid_input",
            "service must be a .service unit without shell metacharacters",
            true,
            None,
        ));
    }
    let timeout = input
        .timeout_seconds
        .unwrap_or(DEFAULT_SYSTEMD_RESTART_TIMEOUT);
    if !(1..=MAX_SYSTEMD_RESTART_TIMEOUT).contains(&timeout) {
        return Ok(failed(
            "invalid_input",
            format!("timeoutSeconds must be between 1 and {MAX_SYSTEMD_RESTART_TIMEOUT}"),
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
    let command = systemd_restart_command(&input.service);
    let result = match state
        .ssh
        .execute_once(
            &session,
            &command,
            Some(std::time::Duration::from_secs(timeout as u64)),
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
            format!("could not restart systemd service `{}`", input.service),
            false,
            Some(json!({
                "exitCode": result.exit_code,
                "stderr": truncate_text(&result.stderr_lossy(), 1_000),
            })),
        ));
    }
    Ok(success(
        serde_json::to_value(SystemdRestartResult {
            service: input.service,
            restarted: true,
        })
        .map_err(|error| error.to_string())?,
    ))
}

pub(super) async fn package_inspect(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<PackageInspectInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("package.inspect input is invalid: {error}"),
                true,
                None,
            ));
        }
    };
    if !is_supported_package_manager(&input.manager) {
        return Ok(failed(
            "invalid_input",
            "package manager must be apt or dnf",
            true,
            None,
        ));
    }
    if !is_safe_package_ref(&input.package) {
        return Ok(failed(
            "invalid_input",
            "package must be a safe reference without shell metacharacters",
            true,
            None,
        ));
    }
    let Some(command) = package_inspect_command(&input.manager, &input.package) else {
        return Ok(failed(
            "invalid_input",
            "package.inspect could not construct a safe query",
            true,
            None,
        ));
    };
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let session = match state.terminals.cached_session(server_id) {
        Ok(session) => session,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
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
            format!("could not inspect package `{}`", input.package),
            false,
            Some(json!({
                "exitCode": result.exit_code,
                "stderr": truncate_text(&result.stderr_lossy(), 1_000),
            })),
        ));
    }
    match parse_package_inspect(&result.stdout_lossy(), &input.manager, &input.package) {
        Ok(inspected) => Ok(success(
            serde_json::to_value(inspected).map_err(|error| error.to_string())?,
        )),
        Err(error) if error.starts_with("package manager ") => {
            Ok(failed("unsupported", error, false, None))
        }
        Err(error) => Ok(failed("execution_failed", error, false, None)),
    }
}

pub(super) async fn package_install(
    state: &AppState,
    server_id: &str,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let input = match serde_json::from_value::<PackageInstallInput>(input.clone()) {
        Ok(input) => input,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("package.install input is invalid: {error}"),
                true,
                None,
            ));
        }
    };
    if !is_supported_package_manager(&input.manager) {
        return Ok(failed(
            "invalid_input",
            "package manager must be apt or dnf",
            true,
            None,
        ));
    }
    if !is_safe_package_ref(&input.package) {
        return Ok(failed(
            "invalid_input",
            "package must be a safe reference without shell metacharacters",
            true,
            None,
        ));
    }
    if let Some(version) = input.version.as_deref() {
        if !is_safe_package_version(version) {
            return Ok(failed(
                "invalid_input",
                "package version contains unsupported characters",
                true,
                None,
            ));
        }
    }
    let timeout = input
        .timeout_seconds
        .unwrap_or(DEFAULT_PACKAGE_INSTALL_TIMEOUT);
    if !(1..=MAX_PACKAGE_INSTALL_TIMEOUT).contains(&timeout) {
        return Ok(failed(
            "invalid_input",
            format!("timeoutSeconds must be between 1 and {MAX_PACKAGE_INSTALL_TIMEOUT}"),
            true,
            None,
        ));
    }
    let command =
        match package_install_command(&input.manager, &input.package, input.version.as_deref()) {
            Ok(command) => command,
            Err(error) => return Ok(failed("invalid_input", error, true, None)),
        };
    if let Err(error) = ensure_session_with_cancel(state, server_id, cancel).await {
        return Ok(transport_or_cancel(error, cancel));
    }
    let session = match state.terminals.cached_session(server_id) {
        Ok(session) => session,
        Err(error) => return Ok(transport_or_cancel(error, cancel)),
    };
    let result = match state
        .ssh
        .execute_once(
            &session,
            &command,
            Some(std::time::Duration::from_secs(timeout as u64)),
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
            format!("could not install package `{}`", input.package),
            false,
            Some(json!({
                "exitCode": result.exit_code,
                "stderr": truncate_text(&result.stderr_lossy(), 1_000),
            })),
        ));
    }
    Ok(success(
        serde_json::to_value(PackageInstallResult {
            manager: input.manager,
            package: input.package,
            version: input.version,
            installed: true,
        })
        .map_err(|error| error.to_string())?,
    ))
}

fn is_empty_object(value: &Value) -> bool {
    value.as_object().is_some_and(serde_json::Map::is_empty)
}

#[cfg(test)]
mod server_exec_tests {
    use super::{
        is_critical_server_command, server_exec_command, validate_server_exec_input,
        ServerExecInput,
    };
    use std::collections::BTreeMap;

    fn valid_input() -> ServerExecInput {
        ServerExecInput {
            command: "printf '%s' hello".into(),
            purpose: "verify command quoting".into(),
            timeout_ms: 1_000,
            max_output_bytes: 4_096,
            workdir: Some("/srv/app".into()),
            env: Some(BTreeMap::from([("LANG".into(), "C.UTF-8".into())])),
        }
    }

    #[test]
    fn host_validation_rejects_path_traversal_secrets_and_unbounded_limits() {
        let mut input = valid_input();
        input.workdir = Some("/srv/../etc".into());
        assert!(validate_server_exec_input(&input).is_err());

        let mut input = valid_input();
        input.env = Some(BTreeMap::from([(
            "LD_PRELOAD".into(),
            "/tmp/evil.so".into(),
        )]));
        assert!(validate_server_exec_input(&input).is_err());

        let mut input = valid_input();
        input.max_output_bytes = super::SERVER_EXEC_MAX_OUTPUT_BYTES + 1;
        assert!(validate_server_exec_input(&input).is_err());
    }

    #[test]
    fn host_wrapper_quotes_optional_directory_and_environment_values() {
        let mut input = valid_input();
        input.workdir = Some("/srv/app with 'quote'".into());
        input.env = Some(BTreeMap::from([("LANG".into(), "C 'UTF-8'".into())]));
        let command = server_exec_command(&input);
        assert!(command.starts_with("cd "));
        assert!(command.contains(" && env LANG="));
        assert!(
            command.contains("'\\''"),
            "single quotes must be escaped by the host wrapper"
        );
        assert!(command.ends_with("hello'"));
    }

    #[test]
    fn host_command_scanner_keeps_known_critical_patterns_out_of_task_grants() {
        for command in [
            "rm /",
            "sudo rm --recursive /tmp/cache",
            "mkfs.ext4 /dev/sda1",
            "DROP /* comment */ DATABASE app",
            "dd if=/dev/zero of=/dev/nvme0n1",
            "echo bad > /dev/vda",
            "chmod -R /",
        ] {
            assert!(
                is_critical_server_command(command),
                "expected critical match for: {command}"
            );
        }
        assert!(!is_critical_server_command("systemctl restart api"));
    }
}
