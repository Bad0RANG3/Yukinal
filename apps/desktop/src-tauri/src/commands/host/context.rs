//! `host.context.fetch`：交给 sidecar 的调查上下文投影。
//!
//! 从 `host.rs` 拆出来的理由：这一段只读任务、证据元数据与工件摘要，不执行任何
//! 远端工具、也不写任何东西。它与证据/计划处理器共用 `record_failure` 等根模块辅助。

use super::*;

pub(super) fn handle_context_request(state: &AppState, params: Value) -> Result<Value, String> {
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
        HostContextKind::Investigation => investigation_context(state, &request.id),
    }
}

pub(super) fn investigation_context(state: &AppState, task_id: &str) -> Result<Value, String> {
    let task = match state.database.investigations().get_task(task_id) {
        Ok(task) => task,
        Err(error) => return context_error(error),
    };
    let evidence = state
        .database
        .investigations()
        .list_evidence(task_id, MAX_EVIDENCE_SEARCH_LIMIT)
        .map_err(|error| error.to_string())?;
    let findings = state
        .database
        .investigations()
        .list_findings(task_id, 100)
        .map_err(|error| error.to_string())?;
    let decision_brief = state
        .database
        .investigations()
        .latest_decision_brief(task_id)
        .map_err(|error| error.to_string())?;
    let runs = state
        .database
        .investigations()
        .list_runs(task_id, 64)
        .map_err(|error| error.to_string())?;
    let steps = state
        .database
        .investigations()
        .list_steps(task_id, 512)
        .map_err(|error| error.to_string())?;
    let artifacts = state
        .database
        .investigations()
        .list_artifacts(task_id, 128)
        .map_err(|error| error.to_string())?;
    let plan = state
        .database
        .investigations()
        .latest_plan(task_id)
        .map_err(|error| error.to_string())?;
    let evaluated_at_epoch = yukinal_time::now_epoch_seconds();
    context_success(InvestigationContextResponse {
        task,
        evidence: evidence
            .into_iter()
            .map(|item| InvestigationEvidenceSummary::from_at(item, evaluated_at_epoch))
            .collect(),
        findings,
        runs,
        steps,
        artifacts: artifacts
            .into_iter()
            .map(InvestigationArtifactSummary::from)
            .collect(),
        plan,
        decision_brief,
    })
}

pub(super) fn context_row<T: Serialize>(
    result: yukinal_database::Result<T>,
) -> Result<Value, String> {
    match result {
        Ok(value) => context_success(value),
        Err(error) => context_error(error),
    }
}

pub(super) fn context_success<T: Serialize>(value: T) -> Result<Value, String> {
    Ok(json!({
        "status": "success",
        "data": serde_json::to_value(value).map_err(|error| error.to_string())?,
    }))
}

pub(super) fn context_error(error: DatabaseError) -> Result<Value, String> {
    if matches!(error, DatabaseError::NotFound) {
        return Ok(json!({ "status": "not_found" }));
    }
    Ok(failed("internal", error.to_string(), false, None))
}
