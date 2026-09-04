//! Activity IPC commands: read-only access to the persisted audit stream.

use serde::Serialize;
use tauri::State;

use crate::state::AppState;
use yukinal_database::models::{Activity, ActivityOutcome, ActivitySource, ActivityType};

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 100;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityListResponse {
    pub activities: Vec<Activity>,
}

pub(crate) fn record_user_activity(
    state: &State<'_, AppState>,
    server_id: Option<&str>,
    activity_type: ActivityType,
    title: &str,
    description: Option<String>,
    outcome: ActivityOutcome,
) -> Result<(), String> {
    state
        .database
        .activities()
        .insert(&Activity {
            id: crate::commands::server::next_id("act"),
            server_id: server_id.map(str::to_string),
            workspace_id: None,
            r#type: activity_type,
            title: title.to_string(),
            description,
            source: ActivitySource::User,
            actor: "user".to_string(),
            reason: Some("用户在工作区执行操作".to_string()),
            outcome: Some(outcome),
            trace_id: None,
            created_at: yukinal_core::sidecar::iso8601_now(),
        })
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn activity_list(
    state: State<'_, AppState>,
    server_id: Option<String>,
    limit: Option<usize>,
) -> Result<ActivityListResponse, String> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(format!("activity limit must be between 1 and {MAX_LIMIT}"));
    }

    let activities = match server_id.as_deref() {
        Some(server_id) => state
            .database
            .activities()
            .list_recent_for_server(server_id, limit),
        None => state.database.activities().list_recent(limit),
    }
    .map_err(|error| error.to_string())?;

    Ok(ActivityListResponse { activities })
}

#[cfg(test)]
mod tests {
    use super::ActivityListResponse;
    use serde_json::Value;
    use yukinal_database::models::{Activity, ActivityOutcome, ActivitySource, ActivityType};

    const FIXTURE: &str =
        include_str!("../../../../../packages/shared/fixtures/ipc/activity_list.json");

    #[test]
    fn activity_list_serializes_to_the_contract_fixture() {
        let actual = serde_json::to_value(ActivityListResponse {
            activities: vec![
                Activity {
                    id: "act_01".into(),
                    server_id: Some("srv_01abc".into()),
                    workspace_id: None,
                    r#type: ActivityType::Connection,
                    title: "已连接服务器".into(),
                    description: None,
                    source: ActivitySource::User,
                    actor: "user".into(),
                    reason: Some("用户请求连接".into()),
                    outcome: Some(ActivityOutcome::Success),
                    trace_id: None,
                    created_at: "2026-01-01T00:00:00.000Z".into(),
                },
                Activity {
                    id: "act_02".into(),
                    server_id: None,
                    workspace_id: None,
                    r#type: ActivityType::Configuration,
                    title: "Provider 已更新".into(),
                    description: Some("更新默认模型".into()),
                    source: ActivitySource::System,
                    actor: "core".into(),
                    reason: None,
                    outcome: Some(ActivityOutcome::Success),
                    trace_id: None,
                    created_at: "2026-01-01T00:01:00.000Z".into(),
                },
            ],
        })
        .expect("serialize");
        let expected: Value = serde_json::from_str(FIXTURE).expect("fixture");
        assert_eq!(actual, expected);
    }
}
