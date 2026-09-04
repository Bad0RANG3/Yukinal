//! Read-only access to persisted Agent tool execution traces.

use serde::Serialize;
use tauri::State;

use crate::state::AppState;
use yukinal_database::models::ToolExecutionRecord;

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 100;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionListResponse {
    pub executions: Vec<ToolExecutionRecord>,
}

/// Load a trace or a bounded recent slice of the persisted execution audit.
#[tauri::command]
pub fn tool_execution_list(
    state: State<'_, AppState>,
    trace_id: Option<String>,
    server_id: Option<String>,
    limit: Option<usize>,
) -> Result<ToolExecutionListResponse, String> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(format!(
            "tool execution limit must be between 1 and {MAX_LIMIT}"
        ));
    }
    if trace_id.as_deref().is_some_and(str::is_empty) {
        return Err("trace id must not be empty".to_string());
    }
    if let Some(server_id) = server_id.as_deref() {
        if !is_stable_server_id(server_id) {
            return Err("server id must be an opaque srv_ id".to_string());
        }
    }

    let executions = match (trace_id.as_deref(), server_id.as_deref()) {
        (Some(trace_id), _) => state
            .database
            .executions()
            .list_for_trace(trace_id)
            .map(|records| records.into_iter().take(limit).collect()),
        (None, Some(server_id)) => state
            .database
            .executions()
            .list_recent_for_server(server_id, limit),
        (None, None) => state.database.executions().list_recent(limit),
    }
    .map_err(|error| error.to_string())?;

    Ok(ToolExecutionListResponse { executions })
}

fn is_stable_server_id(value: &str) -> bool {
    value.len() > 4
        && value.starts_with("srv_")
        && value
            .bytes()
            .skip(4)
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::{is_stable_server_id, ToolExecutionListResponse};
    use serde_json::json;
    use yukinal_database::models::{
        Environment, PermissionMode, RiskLevel, ToolExecutionRecord, ToolExecutionStatus,
    };

    const FIXTURE: &str =
        include_str!("../../../../../packages/shared/fixtures/ipc/tool_execution_list.json");

    #[test]
    fn server_ids_match_the_wire_contract() {
        assert!(is_stable_server_id("srv_01abc"));
        assert!(!is_stable_server_id("srv_01_abc"));
        assert!(!is_stable_server_id("api.example.com:22"));
    }

    #[test]
    fn empty_execution_list_matches_the_shared_fixture() {
        let actual = serde_json::to_value(ToolExecutionListResponse { executions: vec![] })
            .expect("serialize");
        let expected: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture");
        assert_eq!(actual, expected);
    }

    #[test]
    fn waiting_approval_status_keeps_its_wire_underscore() {
        let record = ToolExecutionRecord {
            trace_id: "trc_1".into(),
            step_id: "step_1".into(),
            call_id: "call_1".into(),
            tool_name: "docker.ps".into(),
            server_id: None,
            environment: Environment::Local,
            risk_level: RiskLevel::Read,
            decision: PermissionMode::Ask,
            approved_by: None,
            status: ToolExecutionStatus::WaitingApproval,
            input: json!({}),
            output: None,
            error: None,
            started_at: "2026-01-01T00:00:00Z".into(),
            ended_at: None,
            duration_ms: None,
        };
        assert_eq!(
            serde_json::to_value(record).expect("serialize")["status"],
            "waiting_approval"
        );
    }
}
