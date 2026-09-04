//! Workspace listing: the project view reads the existing local workspace rows.

use serde::Serialize;
use tauri::State;

use crate::state::AppState;
use yukinal_database::models::Workspace;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceListResponse {
    pub workspaces: Vec<Workspace>,
}

#[tauri::command]
pub fn workspace_list(state: State<'_, AppState>) -> Result<WorkspaceListResponse, String> {
    let workspaces = state
        .database
        .workspaces()
        .list()
        .map_err(|error| error.to_string())?;
    Ok(WorkspaceListResponse { workspaces })
}

#[cfg(test)]
mod tests {
    use super::WorkspaceListResponse;
    use yukinal_database::models::{Environment, Workspace, WorkspaceRepository};

    const FIXTURE: &str =
        include_str!("../../../../../packages/shared/fixtures/ipc/workspace_list.json");

    #[test]
    fn workspace_list_serializes_nested_repositories() {
        let response = WorkspaceListResponse {
            workspaces: vec![Workspace {
                id: "ws_01".into(),
                name: "Checkout Staging".into(),
                server_ids: vec!["srv_01abc".into()],
                repositories: vec![WorkspaceRepository {
                    id: "repo_01".into(),
                    name: "api".into(),
                    host: "remote".into(),
                    path: Some("/srv/api".into()),
                    server_id: Some("srv_01abc".into()),
                    git_url: Some("https://example.com/api.git".into()),
                    default_branch: Some("main".into()),
                }],
                provider_ids: vec!["prv_01".into()],
                default_environment: Environment::Staging,
            }],
        };

        let json = serde_json::to_value(response).expect("serialize workspace list");
        let expected: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture");
        assert_eq!(json, expected);
    }
}
