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
            ))
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
            ))
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
            ))
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
            ))
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
            ))
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
            ))
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
                ))
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
            return Ok(failed("invalid_input", error, false, None))
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
            ))
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
            ))
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
            ))
        }
    };
    let request = match AgentCleanupBackupRequest::check(
        &input.path,
        &input.backup_path,
        &input.expected_revision,
    ) {
        Ok(request) => request,
        Err(error) => return Ok(filesystem_failure(error, cancel)),
    };
    let backup = match state
        .database
        .filesystem_backups()
        .find_available(server_id, &input.path, &input.backup_path)
    {
        Ok(Some(record)) => record,
        Ok(None) => {
            return Ok(failed(
                "denied_by_policy",
                "filesystem.backup.cleanup requires an available backup previously created by this host for the same server and target path",
                false,
                None,
            ))
        }
        Err(error) => {
            return Ok(failed(
                "internal",
                format!("could not read the filesystem backup ledger: {error}"),
                false,
                None,
            ))
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
            ))
        }
    };
    let request =
        match AgentRestoreRequest::check(&input.path, &input.backup_path, &input.expected_revision)
        {
            Ok(request) => request,
            Err(error) => return Ok(filesystem_failure(error, cancel)),
        };
    let backup = match state
        .database
        .filesystem_backups()
        .find_available(server_id, &input.path, &input.backup_path)
    {
        Ok(Some(record)) => record,
        Ok(None) => {
            return Ok(failed(
                "denied_by_policy",
                "filesystem.restore requires an available backup previously created by this host for the same server and target path",
                false,
                None,
            ))
        }
        Err(error) => {
            return Ok(failed(
                "internal",
                format!("could not read the filesystem backup ledger: {error}"),
                false,
                None,
            ))
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
            ))
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
            ))
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
            ))
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
            ))
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
