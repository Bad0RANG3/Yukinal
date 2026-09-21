//! Investigation evidence, findings, decision briefs and retention handlers.
//!
//! 从 `host.rs` 拆出来的理由：这一段是一组完整的只读/记录协议 —— 证据记录、取回、
//! 搜索、关联、比较、留存预览，以及 finding / brief 的写入。它们共享同一套作用域
//! 校验（`same_investigation_scope` / `same_investigation_target`）和有界 JSON/文本
//! 比较（`collect_json_diff` / `compare_text_content`），与工具执行、计划、上下文无关。

use super::*;

/// Persist one already-redacted read-only result for an existing investigation task.
///
/// The sidecar prepares the envelope so it can classify and bound the tool output. The host
/// remains the authority on task existence, target scope and the final database validation.
pub(super) fn handle_evidence_record(state: &AppState, params: Value) -> Result<Value, String> {
    let evidence = match serde_json::from_value::<Evidence>(
        params.get("evidence").cloned().unwrap_or(Value::Null),
    ) {
        Ok(evidence) => evidence,
        Err(error) => {
            return Ok(evidence_failure(
                "invalid_input",
                format!("invalid evidence: {error}"),
                false,
            ))
        }
    };

    let task = match state.database.investigations().get_task(&evidence.task_id) {
        Ok(task) => task,
        Err(DatabaseError::NotFound) => {
            return Ok(evidence_failure(
                "not_found",
                format!("investigation task `{}` was not found", evidence.task_id),
                false,
            ))
        }
        Err(error) => return Ok(evidence_failure("internal", error.to_string(), false)),
    };

    if !same_investigation_scope(&task, &evidence) {
        return Ok(evidence_failure(
            "denied_by_policy",
            "evidence target does not match the investigation task scope",
            false,
        ));
    }

    // The sidecar may include a run id for context, but only the host knows which
    // investigation run is currently active. Overwrite the untrusted value so scheduled
    // samples cannot be attributed to another run by model output.
    let mut evidence = evidence;
    evidence.run_id = task.active_run_id.clone();

    // A model may repeat the same read or retry the persistence call after a
    // transport hiccup. Collapse only an identical observation inside the
    // currently active durable run; separate scheduled runs must retain their
    // own samples for comparison.
    if let Err(error) = state.database.investigations().validate_evidence(&evidence) {
        return match error {
            DatabaseError::Validation(message) => {
                Ok(evidence_failure("invalid_input", message, false))
            }
            error => Ok(evidence_failure("internal", error.to_string(), false)),
        };
    }

    match state
        .database
        .investigations()
        .find_evidence_in_run(&evidence)
    {
        Ok(Some(existing)) => {
            return Ok(json!({
                "recorded": true,
                "evidenceId": existing.id,
                "reused": true,
            }));
        }
        Ok(None) => {}
        Err(error) => return Ok(evidence_failure("internal", error.to_string(), false)),
    }

    match state.database.investigations().add_evidence(&evidence) {
        Ok(()) => Ok(json!({
            "recorded": true,
            "evidenceId": evidence.id,
            "reused": false,
        })),
        Err(DatabaseError::Validation(message)) => {
            Ok(evidence_failure("invalid_input", message, false))
        }
        Err(error) => Ok(evidence_failure("internal", error.to_string(), false)),
    }
}

pub(super) fn handle_evidence_fetch(state: &AppState, params: Value) -> Result<Value, String> {
    let request = serde_json::from_value::<HostEvidenceFetchRequest>(params)
        .map_err(|error| format!("invalid evidence fetch request: {error}"))?;
    if request.task_id.trim().is_empty() || request.evidence_id.trim().is_empty() {
        return Ok(json!({
            "status": "failed",
            "error": { "code": "invalid_input", "message": "taskId and evidenceId are required", "retryable": false }
        }));
    }
    let evidence = match state
        .database
        .investigations()
        .get_evidence(&request.evidence_id)
    {
        Ok(evidence) => evidence,
        Err(DatabaseError::NotFound) => return Ok(json!({ "status": "not_found" })),
        Err(error) => {
            return Ok(json!({
                "status": "failed",
                "error": { "code": "internal", "message": error.to_string(), "retryable": false }
            }))
        }
    };
    if evidence.task_id != request.task_id {
        return Ok(json!({
            "status": "failed",
            "error": { "code": "denied_by_policy", "message": "evidence does not belong to the current investigation task", "retryable": false }
        }));
    }
    Ok(json!({
        "status": "success",
        "evidence": evidence_json_with_freshness_at(&evidence, yukinal_time::now_epoch_seconds()),
    }))
}

pub(super) fn handle_evidence_search(state: &AppState, params: Value) -> Result<Value, String> {
    let request = match serde_json::from_value::<HostEvidenceSearchRequest>(params) {
        Ok(request) => request,
        Err(error) => {
            return Ok(json!({
                "status": "failed",
                "error": { "code": "invalid_input", "message": format!("invalid evidence search request: {error}"), "retryable": false }
            }))
        }
    };
    if request.task_id.trim().is_empty() || request.task_id.len() > 256 {
        return Ok(json!({
            "status": "failed",
            "error": { "code": "invalid_input", "message": "taskId must be between 1 and 256 characters", "retryable": false }
        }));
    }
    let limit = request.limit.unwrap_or(32);
    if !(1..=MAX_EVIDENCE_SEARCH_LIMIT).contains(&limit) {
        return Ok(json!({
            "status": "failed",
            "error": { "code": "invalid_input", "message": format!("limit must be between 1 and {MAX_EVIDENCE_SEARCH_LIMIT}"), "retryable": false }
        }));
    }
    if request.source_tool.as_deref().is_some_and(|source| {
        source.trim().is_empty()
            || source.chars().count() > 256
            || source.chars().any(char::is_control)
    }) {
        return Ok(json!({
            "status": "failed",
            "error": { "code": "invalid_input", "message": "sourceTool must be between 1 and 256 visible characters", "retryable": false }
        }));
    }
    for timestamp in [request.from.as_deref(), request.to.as_deref()]
        .into_iter()
        .flatten()
    {
        if yukinal_time::parse_iso8601_utc(timestamp).is_none() {
            return Ok(json!({
                "status": "failed",
                "error": { "code": "invalid_input", "message": "from/to must be UTC ISO-8601 timestamps", "retryable": false }
            }));
        }
    }
    if let (Some(from), Some(to)) = (request.from.as_deref(), request.to.as_deref()) {
        if yukinal_time::parse_iso8601_utc(from) > yukinal_time::parse_iso8601_utc(to) {
            return Ok(json!({
                "status": "failed",
                "error": { "code": "invalid_input", "message": "from must not be after to", "retryable": false }
            }));
        }
    }
    let task = match state.database.investigations().get_task(&request.task_id) {
        Ok(task) => task,
        Err(DatabaseError::NotFound) => {
            return Ok(json!({
                "status": "failed",
                "error": { "code": "not_found", "message": "investigation task was not found", "retryable": false }
            }))
        }
        Err(error) => {
            return Ok(json!({
                "status": "failed",
                "error": { "code": "internal", "message": error.to_string(), "retryable": false }
            }))
        }
    };
    let scope = match request.target.as_ref() {
        Some(target) => match host_target_to_investigation(target) {
            Ok(scope) if same_investigation_target(&task.scope, &scope) => scope,
            Ok(_) => {
                return Ok(json!({
                    "status": "failed",
                    "error": { "code": "denied_by_policy", "message": "evidence target does not match the investigation task scope", "retryable": false }
                }))
            }
            Err(error) => {
                return Ok(json!({
                    "status": "failed",
                    "error": { "code": "invalid_input", "message": error, "retryable": false }
                }))
            }
        },
        None => task.scope.clone(),
    };
    let evidence = match state.database.investigations().search_evidence(
        &task.id,
        &EvidenceSearchQuery {
            source_tool: request.source_tool,
            kind: request.kind,
            from: request.from,
            to: request.to,
            scope: Some(scope),
            limit,
        },
    ) {
        Ok(evidence) => evidence,
        Err(DatabaseError::Validation(message)) => {
            return Ok(json!({
                "status": "failed",
                "error": { "code": "invalid_input", "message": message, "retryable": false }
            }))
        }
        Err(error) => {
            return Ok(json!({
                "status": "failed",
                "error": { "code": "internal", "message": error.to_string(), "retryable": false }
            }))
        }
    };
    let evaluated_at_epoch = yukinal_time::now_epoch_seconds();
    let summaries = evidence
        .iter()
        .map(|item| evidence_summary_at(item, evaluated_at_epoch))
        .collect::<Vec<_>>();
    Ok(json!({ "status": "success", "evidence": summaries }))
}

/// Return metadata that was collected with one anchor observation. A host-owned run is the
/// strongest relation; legacy/manual evidence without a run falls back to a small time window.
/// This is deliberately correlation, not a causal or semantic conclusion: the model still has
/// to inspect the returned sources and cite the underlying evidence in a Finding.
pub(super) fn handle_evidence_correlation(
    state: &AppState,
    params: Value,
) -> Result<Value, String> {
    let request = match serde_json::from_value::<HostEvidenceCorrelationRequest>(params) {
        Ok(request) => request,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("invalid evidence correlation request: {error}"),
                false,
                None,
            ))
        }
    };
    if request.task_id.trim().is_empty()
        || request.task_id.len() > 256
        || request.anchor_evidence_id.trim().is_empty()
        || request.anchor_evidence_id.len() > 256
    {
        return Ok(failed(
            "invalid_input",
            "taskId and anchorEvidenceId must be between 1 and 256 characters",
            false,
            None,
        ));
    }
    let window_seconds = request
        .window_seconds
        .unwrap_or(DEFAULT_EVIDENCE_CORRELATION_WINDOW_SECONDS);
    if !(1..=MAX_EVIDENCE_CORRELATION_WINDOW_SECONDS).contains(&window_seconds) {
        return Ok(failed(
            "invalid_input",
            format!(
                "windowSeconds must be between 1 and {MAX_EVIDENCE_CORRELATION_WINDOW_SECONDS}"
            ),
            false,
            None,
        ));
    }
    let limit = request.limit.unwrap_or(32);
    if !(1..=MAX_EVIDENCE_SEARCH_LIMIT).contains(&limit) {
        return Ok(failed(
            "invalid_input",
            format!("limit must be between 1 and {MAX_EVIDENCE_SEARCH_LIMIT}"),
            false,
            None,
        ));
    }

    let task = match state.database.investigations().get_task(&request.task_id) {
        Ok(task) => task,
        Err(DatabaseError::NotFound) => {
            return Ok(failed(
                "not_found",
                "investigation task was not found",
                false,
                None,
            ))
        }
        Err(error) => return Ok(failed("internal", error.to_string(), false, None)),
    };
    let anchor = match state
        .database
        .investigations()
        .get_evidence(&request.anchor_evidence_id)
    {
        Ok(evidence) => evidence,
        Err(DatabaseError::NotFound) => {
            return Ok(failed(
                "not_found",
                "anchor evidence was not found in the current task",
                false,
                None,
            ))
        }
        Err(error) => return Ok(failed("internal", error.to_string(), false, None)),
    };
    if anchor.task_id != task.id || !same_investigation_scope(&task, &anchor) {
        return Ok(failed(
            "denied_by_policy",
            "anchor evidence does not belong to the current investigation scope",
            false,
            None,
        ));
    }

    let (matched_by, related) = if let Some(run_id) = anchor.run_id.as_deref() {
        let evidence = match state
            .database
            .investigations()
            .list_evidence_for_run(run_id, limit)
        {
            Ok(evidence) => evidence,
            Err(error) => return Ok(failed("internal", error.to_string(), false, None)),
        };
        ("same_run", evidence)
    } else {
        let Some(anchor_epoch) = yukinal_time::parse_iso8601_utc(&anchor.collected_at) else {
            return Ok(failed(
                "invalid_input",
                "anchor evidence has no valid UTC collection timestamp for time-window correlation",
                false,
                None,
            ));
        };
        let from = yukinal_time::iso8601_utc(anchor_epoch.saturating_sub(window_seconds));
        let to = yukinal_time::iso8601_utc(anchor_epoch.saturating_add(window_seconds));
        let evidence = match state.database.investigations().search_evidence(
            &task.id,
            &EvidenceSearchQuery {
                source_tool: None,
                kind: None,
                from: Some(from),
                to: Some(to),
                scope: Some(task.scope.clone()),
                limit,
            },
        ) {
            Ok(evidence) => evidence,
            Err(error) => return Ok(failed("internal", error.to_string(), false, None)),
        };
        ("time_window", evidence)
    };

    let mut warnings = Vec::new();
    let evaluated_at_epoch = yukinal_time::now_epoch_seconds();
    let anchor_summary = evidence_summary_at(&anchor, evaluated_at_epoch);
    if anchor_summary.freshness.status != "fresh" {
        warnings.push(format!(
            "anchor evidence is {} under default-v1 freshness",
            anchor_summary.freshness.status
        ));
    }
    if anchor.truncated {
        warnings.push("anchor evidence was truncated at collection".into());
    }
    let mut source_tools = BTreeSet::new();
    let summaries = related
        .into_iter()
        .filter(|evidence| evidence.task_id == task.id && same_investigation_scope(&task, evidence))
        .take(limit)
        .map(|evidence| {
            source_tools.insert(evidence.source_tool.clone());
            let summary = evidence_summary_at(&evidence, evaluated_at_epoch);
            if summary.freshness.status != "fresh" && warnings.len() < 8 {
                warnings.push(format!(
                    "{} evidence is {} under default-v1 freshness",
                    evidence.source_tool, summary.freshness.status
                ));
            }
            if evidence.truncated && warnings.len() < 8 {
                warnings.push(format!(
                    "{} evidence was truncated at collection",
                    evidence.source_tool
                ));
            }
            summary
        })
        .collect::<Vec<_>>();
    source_tools.insert(anchor.source_tool.clone());
    if summaries.is_empty() {
        warnings.push("no other evidence matched the host-owned run or time window".into());
    }
    warnings.truncate(8);
    Ok(json!({
        "status": "success",
        "correlation": {
            "anchor": anchor_summary,
            "evidence": summaries,
            "matchedBy": matched_by,
            "windowSeconds": window_seconds,
            "sourceTools": source_tools.into_iter().take(32).collect::<Vec<_>>(),
            "warnings": warnings,
        },
    }))
}

pub(super) fn evidence_summary_at(evidence: &Evidence, evaluated_at_epoch: u64) -> EvidenceSummary {
    EvidenceSummary {
        freshness: evidence_freshness_at(&evidence.collected_at, evaluated_at_epoch),
        id: evidence.id.clone(),
        task_id: evidence.task_id.clone(),
        run_id: evidence.run_id.clone(),
        scope: evidence.scope.clone(),
        kind: evidence.kind,
        source_tool: evidence.source_tool.clone(),
        collected_at: evidence.collected_at.clone(),
        input_summary: evidence.input_summary.clone(),
        content_type: evidence.content_type,
        content_hash: evidence.content_hash.clone(),
        truncated: evidence.truncated,
        redaction_status: evidence.redaction_status,
    }
}

/// Compare two persisted observations without returning either body. The model can ask for a
/// single body explicitly through `investigation.evidence`, but routine anomaly explanation gets
/// only host-derived paths/counts so a comparison cannot accidentally flood context or bypass the
/// evidence boundary.
pub(super) fn handle_evidence_compare(state: &AppState, params: Value) -> Result<Value, String> {
    let request = match serde_json::from_value::<HostEvidenceCompareRequest>(params) {
        Ok(request) => request,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("invalid evidence comparison request: {error}"),
                false,
                None,
            ))
        }
    };
    if request.task_id.trim().is_empty()
        || request.task_id.len() > 256
        || request.left_evidence_id.trim().is_empty()
        || request.right_evidence_id.trim().is_empty()
        || request.left_evidence_id.len() > 256
        || request.right_evidence_id.len() > 256
    {
        return Ok(failed(
            "invalid_input",
            "taskId and both evidence ids must be between 1 and 256 characters",
            false,
            None,
        ));
    }
    let task = match state.database.investigations().get_task(&request.task_id) {
        Ok(task) => task,
        Err(DatabaseError::NotFound) => {
            return Ok(failed(
                "not_found",
                "investigation task was not found",
                false,
                None,
            ))
        }
        Err(error) => return Ok(failed("internal", error.to_string(), false, None)),
    };
    let load = |id: &str, side: &str| -> Result<Evidence, Value> {
        match state.database.investigations().get_evidence(id) {
            Ok(evidence) => {
                if evidence.task_id != task.id || !same_investigation_scope(&task, &evidence) {
                    Err(failed(
                        "denied_by_policy",
                        format!(
                            "{side} evidence does not belong to the current investigation scope"
                        ),
                        false,
                        None,
                    ))
                } else {
                    Ok(evidence)
                }
            }
            Err(DatabaseError::NotFound) => Err(failed(
                "not_found",
                format!("{side} evidence was not found in the current task"),
                false,
                None,
            )),
            Err(error) => Err(failed("internal", error.to_string(), false, None)),
        }
    };
    let left = match load(&request.left_evidence_id, "left") {
        Ok(evidence) => evidence,
        Err(response) => return Ok(response),
    };
    let right = match load(&request.right_evidence_id, "right") {
        Ok(evidence) => evidence,
        Err(response) => return Ok(response),
    };
    let evaluated_at_epoch = yukinal_time::now_epoch_seconds();
    let left_summary = evidence_summary_at(&left, evaluated_at_epoch);
    let right_summary = evidence_summary_at(&right, evaluated_at_epoch);
    let status = if left.content_hash == right.content_hash {
        "identical"
    } else {
        "changed"
    };
    let shape = match (left.content_type, right.content_type) {
        (EvidenceContentType::Json, EvidenceContentType::Json) => "json",
        (EvidenceContentType::Text, EvidenceContentType::Text) => "text",
        _ => "mixed",
    };
    let mut warnings = Vec::new();
    if left.truncated || right.truncated {
        warnings.push("one or both evidence bodies were truncated at collection".into());
    }
    if left.source_tool != right.source_tool || left.kind != right.kind {
        warnings.push("source or evidence kind differs; interpret correlation cautiously".into());
    }
    for (side, freshness) in [
        ("left", &left_summary.freshness),
        ("right", &right_summary.freshness),
    ] {
        if freshness.status != "fresh" {
            warnings.push(format!(
                "{side} evidence is {} under default-v1 freshness",
                freshness.status
            ));
        }
    }

    let (changed_paths, changed_path_count, diff_truncated, text) = if status == "identical" {
        (Vec::new(), 0, false, None)
    } else {
        match (left.content_type, right.content_type) {
            (EvidenceContentType::Json, EvidenceContentType::Json) => {
                let mut diff = JsonDiff::default();
                collect_json_diff(&left.content, &right.content, "$", 0, &mut diff);
                (diff.paths, diff.count, diff.truncated, None)
            }
            (EvidenceContentType::Text, EvidenceContentType::Text) => {
                let (text, truncated) = compare_text_content(&left.content, &right.content);
                (Vec::new(), 0, truncated, text)
            }
            _ => (vec!["$".into()], 1, false, None),
        }
    };
    if diff_truncated {
        warnings.push(
            "comparison details were bounded; inspect individual evidence ids for more context"
                .into(),
        );
    }
    if warnings.len() > 8 {
        warnings.truncate(8);
    }
    Ok(json!({
        "status": "success",
        "comparison": EvidenceComparison {
            status,
            shape,
            left: left_summary,
            right: right_summary,
            changed_paths,
            changed_path_count,
            diff_truncated,
            text,
            warnings,
        },
    }))
}

/// Report local retention candidates to the Agent without giving it a delete capability.
/// The destructive IPC command remains a separate, user-confirmed desktop action.
pub(super) fn handle_retention_preview(state: &AppState, params: Value) -> Result<Value, String> {
    let request = match serde_json::from_value::<HostRetentionPreviewRequest>(params) {
        Ok(request) => request,
        Err(error) => {
            return Ok(failed(
                "invalid_input",
                format!("invalid retention preview request: {error}"),
                false,
                None,
            ))
        }
    };
    if request.task_id.trim().is_empty()
        || request.task_id.chars().count() > 256
        || request.task_id.chars().any(char::is_control)
    {
        return Ok(failed(
            "invalid_input",
            "taskId must be between 1 and 256 visible characters",
            false,
            None,
        ));
    }
    if request.cutoff_at.as_deref().is_some_and(|cutoff| {
        cutoff.trim().is_empty()
            || cutoff.chars().count() > 80
            || cutoff.chars().any(char::is_control)
    }) {
        return Ok(failed(
            "invalid_input",
            "cutoffAt must be a bounded UTC ISO-8601 timestamp",
            false,
            None,
        ));
    }
    let cutoff_at = request.cutoff_at.unwrap_or_else(|| {
        yukinal_time::iso8601_utc(
            yukinal_time::now_epoch_seconds().saturating_sub(DEFAULT_RETENTION_DAYS * 86_400),
        )
    });
    if yukinal_time::parse_iso8601_utc(&cutoff_at).is_none() {
        return Ok(failed(
            "invalid_input",
            "cutoffAt must be a UTC ISO-8601 timestamp",
            false,
            None,
        ));
    }
    let limit = request.limit.unwrap_or(DEFAULT_RETENTION_LIMIT);
    if !(1..=MAX_RETENTION_LIMIT).contains(&limit) {
        return Ok(failed(
            "invalid_input",
            format!("limit must be between 1 and {MAX_RETENTION_LIMIT}"),
            false,
            None,
        ));
    }
    let preview =
        match state
            .database
            .investigation_retention()
            .preview(&request.task_id, &cutoff_at, limit)
        {
            Ok(preview) => preview,
            Err(DatabaseError::NotFound) => {
                return Ok(failed(
                    "not_found",
                    "investigation task was not found",
                    false,
                    None,
                ))
            }
            Err(DatabaseError::Validation(message)) => {
                return Ok(failed("invalid_input", message, false, None))
            }
            Err(error) => return Ok(failed("internal", error.to_string(), false, None)),
        };
    Ok(json!({
        "status": "success",
        "preview": retention_preview_json(preview),
    }))
}

pub(super) fn retention_preview_json(preview: InvestigationRetentionPreview) -> Value {
    json!({
        "taskId": preview.task_id,
        "cutoffAt": preview.cutoff_at,
        "candidates": preview.candidates.into_iter().map(|item| json!({
            "id": item.id,
            "taskId": item.task_id,
            "kind": item.kind.as_str(),
            "createdAt": item.created_at,
            "bytes": item.bytes,
            "reason": item.reason,
        })).collect::<Vec<_>>(),
        "protectedCount": preview.protected_count,
        "candidateBytes": preview.candidate_bytes,
        "truncated": preview.truncated,
    })
}

const MAX_COMPARE_PATHS: usize = 64;
const MAX_COMPARE_PATH_COUNT: usize = 10_000;
const MAX_COMPARE_NODES: usize = 4_096;
const MAX_COMPARE_DEPTH: usize = 8;
const MAX_COMPARE_LINES: usize = 100_000;

#[derive(Default)]
pub(super) struct JsonDiff {
    pub(super) paths: Vec<String>,
    count: usize,
    nodes: usize,
    pub(super) truncated: bool,
}

pub(super) fn collect_json_diff(
    left: &Value,
    right: &Value,
    path: &str,
    depth: usize,
    diff: &mut JsonDiff,
) {
    if left == right {
        return;
    }
    if diff.nodes >= MAX_COMPARE_NODES {
        diff.truncated = true;
        return;
    }
    diff.nodes += 1;
    if depth >= MAX_COMPARE_DEPTH || diff.count >= MAX_COMPARE_PATH_COUNT {
        record_json_diff_path(path, diff);
        diff.truncated = true;
        return;
    }
    match (left, right) {
        (Value::Object(left), Value::Object(right)) => {
            let keys = left
                .keys()
                .chain(right.keys())
                .cloned()
                .collect::<BTreeSet<_>>();
            for key in keys {
                let child_path = if key.chars().all(|character| {
                    character.is_ascii_alphanumeric() || character == '_' || character == '-'
                }) {
                    format!("{path}.{key}")
                } else {
                    format!("{path}[{key:?}]")
                };
                match (left.get(&key), right.get(&key)) {
                    (Some(left), Some(right)) => {
                        collect_json_diff(left, right, &child_path, depth + 1, diff)
                    }
                    _ => record_json_diff_path(&child_path, diff),
                }
                if diff.truncated && diff.nodes >= MAX_COMPARE_NODES {
                    break;
                }
            }
        }
        (Value::Array(left), Value::Array(right)) => {
            for index in 0..left.len().max(right.len()) {
                let child_path = format!("{path}[{index}]");
                match (left.get(index), right.get(index)) {
                    (Some(left), Some(right)) => {
                        collect_json_diff(left, right, &child_path, depth + 1, diff)
                    }
                    _ => record_json_diff_path(&child_path, diff),
                }
                if diff.truncated && diff.nodes >= MAX_COMPARE_NODES {
                    break;
                }
            }
        }
        _ => record_json_diff_path(path, diff),
    }
}

pub(super) fn record_json_diff_path(path: &str, diff: &mut JsonDiff) {
    diff.count = diff.count.saturating_add(1).min(MAX_COMPARE_PATH_COUNT);
    if diff.paths.len() < MAX_COMPARE_PATHS {
        diff.paths.push(path.chars().take(512).collect());
    } else {
        diff.truncated = true;
    }
}

pub(super) fn compare_text_content(
    left: &Value,
    right: &Value,
) -> (Option<EvidenceTextComparison>, bool) {
    let (Some(left), Some(right)) = (left.as_str(), right.as_str()) else {
        return (None, false);
    };
    let left_lines = left
        .split('\n')
        .take(MAX_COMPARE_LINES + 1)
        .collect::<Vec<_>>();
    let right_lines = right
        .split('\n')
        .take(MAX_COMPARE_LINES + 1)
        .collect::<Vec<_>>();
    let truncated = left_lines.len() > MAX_COMPARE_LINES || right_lines.len() > MAX_COMPARE_LINES;
    let left_count = left_lines.len().min(MAX_COMPARE_LINES);
    let right_count = right_lines.len().min(MAX_COMPARE_LINES);
    let paired_changes = left_lines
        .iter()
        .zip(right_lines.iter())
        .take(MAX_COMPARE_LINES)
        .filter(|(left, right)| left != right)
        .count();
    let changed_line_count = paired_changes + left_count.abs_diff(right_count);
    (
        Some(EvidenceTextComparison {
            left_line_count: left_count,
            right_line_count: right_count,
            changed_line_count,
            added_line_count: right_count.saturating_sub(left_count),
            removed_line_count: left_count.saturating_sub(right_count),
        }),
        truncated,
    )
}

pub(super) fn handle_finding_record(state: &AppState, params: Value) -> Result<Value, String> {
    let finding = match serde_json::from_value::<Finding>(
        params.get("finding").cloned().unwrap_or(Value::Null),
    ) {
        Ok(finding) => finding,
        Err(error) => {
            return Ok(record_failure(
                "invalid_input",
                format!("invalid finding: {error}"),
                false,
            ))
        }
    };
    let task = match state.database.investigations().get_task(&finding.task_id) {
        Ok(task) => task,
        Err(DatabaseError::NotFound) => {
            return Ok(record_failure(
                "not_found",
                "investigation task was not found",
                false,
            ))
        }
        Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
    };
    if finding.evidence_ids.is_empty() && finding.kind != FindingKind::Unknown {
        return Ok(record_failure(
            "invalid_input",
            "fact and inference findings require at least one evidence reference",
            false,
        ));
    }
    for evidence_id in &finding.evidence_ids {
        match state.database.investigations().get_evidence(evidence_id) {
            Ok(evidence) if evidence.task_id == task.id => {}
            Ok(_) => {
                return Ok(record_failure(
                    "denied_by_policy",
                    "finding references evidence from another task",
                    false,
                ))
            }
            Err(DatabaseError::NotFound) => {
                return Ok(record_failure(
                    "invalid_input",
                    format!("evidence `{evidence_id}` was not found"),
                    false,
                ))
            }
            Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
        }
    }
    match state.database.investigations().add_finding(&finding) {
        Ok(()) => Ok(json!({ "recorded": true, "finding": finding })),
        Err(DatabaseError::Validation(message)) => {
            Ok(record_failure("invalid_input", message, false))
        }
        Err(error) => Ok(record_failure("internal", error.to_string(), false)),
    }
}

pub(super) fn handle_brief_record(state: &AppState, params: Value) -> Result<Value, String> {
    let brief = match serde_json::from_value::<DecisionBrief>(
        params.get("brief").cloned().unwrap_or(Value::Null),
    ) {
        Ok(brief) => brief,
        Err(error) => {
            return Ok(record_failure(
                "invalid_input",
                format!("invalid decision brief: {error}"),
                false,
            ))
        }
    };
    let task = match state.database.investigations().get_task(&brief.task_id) {
        Ok(task) => task,
        Err(DatabaseError::NotFound) => {
            return Ok(record_failure(
                "not_found",
                "investigation task was not found",
                false,
            ))
        }
        Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
    };
    if let Some(plan_id) = brief.plan_id.as_deref() {
        let current_plan = state
            .database
            .investigations()
            .latest_plan(&task.id)
            .map_err(|error| error.to_string())?;
        if current_plan.as_ref().map(|plan| plan.id.as_str()) != Some(plan_id) {
            return Ok(record_failure(
                "plan_deviation",
                "decision brief must describe the current plan revision",
                false,
            ));
        }
    }
    for finding_id in brief.finding_ids.iter().chain(
        brief
            .options
            .iter()
            .flat_map(|option| option.finding_ids.iter()),
    ) {
        match state.database.investigations().get_finding(finding_id) {
            Ok(finding) if finding.task_id == task.id => {}
            Ok(_) => {
                return Ok(record_failure(
                    "denied_by_policy",
                    "decision brief references a finding from another task",
                    false,
                ))
            }
            Err(DatabaseError::NotFound) => {
                return Ok(record_failure(
                    "invalid_input",
                    format!("finding `{finding_id}` was not found"),
                    false,
                ))
            }
            Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
        }
    }
    for evidence_id in brief
        .options
        .iter()
        .flat_map(|option| option.evidence_ids.iter())
    {
        match state.database.investigations().get_evidence(evidence_id) {
            Ok(evidence) if evidence.task_id == task.id => {}
            Ok(_) => {
                return Ok(record_failure(
                    "denied_by_policy",
                    "decision brief references evidence from another task",
                    false,
                ))
            }
            Err(DatabaseError::NotFound) => {
                return Ok(record_failure(
                    "invalid_input",
                    format!("evidence `{evidence_id}` was not found"),
                    false,
                ))
            }
            Err(error) => return Ok(record_failure("internal", error.to_string(), false)),
        }
    }
    match state.database.investigations().save_decision_brief(&brief) {
        Ok(()) => Ok(json!({ "recorded": true, "brief": brief })),
        Err(DatabaseError::Validation(message)) => {
            Ok(record_failure("invalid_input", message, false))
        }
        Err(error) => Ok(record_failure("internal", error.to_string(), false)),
    }
}
