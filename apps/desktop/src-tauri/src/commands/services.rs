//! Read-only remote service discovery through the C++ SSH service and Python normalizer.

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use crate::commands::ssh_service;
use crate::state::AppState;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ServerServicesInput {
    pub name: Option<String>,
    pub state: Option<ServiceState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceState {
    Running,
    Stopped,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceSource {
    Systemd,
    Docker,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerService {
    pub name: String,
    pub state: ServiceState,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerServicesResponse {
    pub source: ServiceSource,
    pub services: Vec<ServerService>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `server_services` gets remote output from C++ and applies the shared Python parser.
#[tauri::command]
pub async fn server_services(
    app: AppHandle,
    state: State<'_, AppState>,
    server_id: String,
) -> Result<ServerServicesResponse, String> {
    if !ssh_service::supports_cpp_target(&state, &server_id)? {
        return Err("server.services target is not supported by the C++ SSH service".into());
    }
    let output = ssh_service::service_inventory(&app, &state, &server_id).await?;
    let normalized = crate::commands::server::normalize_ssh_output_via_python(
        &app,
        &state,
        "server.services.normalize",
        &output,
        serde_json::json!({}),
    )
    .await?;
    deserialize_python_normalized_services(normalized)
        .map_err(|error| format!("service discovery returned an invalid response: {error}"))
}

fn deserialize_python_normalized_services(
    response: serde_json::Value,
) -> Result<ServerServicesResponse, String> {
    if response.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        let message = response
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Python Agent rejected the service inventory");
        return Err(message.to_string());
    }
    let output = response
        .get("output")
        .cloned()
        .ok_or_else(|| "Python Agent returned no normalized services".to_string())?;
    serde_json::from_value(output)
        .map_err(|_| "Python Agent returned invalid normalized services".to_string())
}

#[cfg(test)]
mod tests {
    use super::{ServerService, ServerServicesResponse, ServiceSource, ServiceState};

    const FIXTURE: &str =
        include_str!("../../../../../packages/shared/fixtures/ipc/server_services.json");

    #[test]
    fn serializes_to_the_shared_contract_fixture() {
        let actual = serde_json::to_value(ServerServicesResponse {
            source: ServiceSource::Systemd,
            services: vec![
                ServerService {
                    name: "sshd.service".into(),
                    state: ServiceState::Running,
                    status: "active/running".into(),
                    description: Some("OpenSSH server daemon".into()),
                },
                ServerService {
                    name: "nginx.service".into(),
                    state: ServiceState::Failed,
                    status: "failed/failed".into(),
                    description: Some("A high performance web server".into()),
                },
            ],
            message: None,
        })
        .expect("serialize");
        let expected: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture");
        assert_eq!(actual, expected);
    }
}
