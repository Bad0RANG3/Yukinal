//! Read-only remote log access through the C++ SSH service and Python normalizer.

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use crate::commands::ssh_service;
use crate::state::AppState;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ServerLogsInput {
    pub since_seconds: Option<u32>,
    pub unit: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogSource {
    Journalctl,
    Syslog,
    Messages,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerLogLine {
    pub text: String,
    pub level: LogLevel,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerLogsResponse {
    pub source: LogSource,
    pub lines: Vec<ServerLogLine>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `server_logs` gets remote output from C++ and applies the shared Python parser.
#[tauri::command]
pub async fn server_logs(
    app: AppHandle,
    state: State<'_, AppState>,
    server_id: String,
) -> Result<ServerLogsResponse, String> {
    if !ssh_service::supports_cpp_target(&state, &server_id)? {
        return Err("server.logs target is not supported by the C++ SSH service".into());
    }
    let output = ssh_service::server_logs(&app, &state, &server_id, None, None).await?;
    let normalized = crate::commands::server::normalize_ssh_output_via_python(
        &app,
        &state,
        "server.logs.normalize",
        &output,
        serde_json::json!({}),
    )
    .await?;
    deserialize_python_normalized_logs(normalized)
        .map_err(|error| format!("log discovery returned an invalid response: {error}"))
}

fn deserialize_python_normalized_logs(
    response: serde_json::Value,
) -> Result<ServerLogsResponse, String> {
    if response.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        let message = response
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Python Agent rejected the log output");
        return Err(message.to_string());
    }
    let output = response
        .get("output")
        .cloned()
        .ok_or_else(|| "Python Agent returned no normalized logs".to_string())?;
    serde_json::from_value(output)
        .map_err(|_| "Python Agent returned invalid normalized logs".to_string())
}

#[cfg(test)]
mod tests {
    use super::{LogLevel, LogSource, ServerLogLine, ServerLogsResponse};

    const FIXTURE: &str =
        include_str!("../../../../../packages/shared/fixtures/ipc/server_logs.json");

    #[test]
    fn serializes_to_the_shared_contract_fixture() {
        let actual = serde_json::to_value(ServerLogsResponse {
            source: LogSource::Journalctl,
            lines: vec![
                ServerLogLine {
                    text: "2026-09-04T06:00:00+0800 host sshd[10]: Accepted publickey".into(),
                    level: LogLevel::Info,
                },
                ServerLogLine {
                    text: "2026-09-04T06:02:00+0800 host app[13]: Failed to connect to database"
                        .into(),
                    level: LogLevel::Error,
                },
            ],
            message: None,
        })
        .expect("serialize");
        let expected: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture");
        assert_eq!(actual, expected);
    }
}
