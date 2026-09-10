//! Agent run commands: the UI sends a prompt; Rust resolves the provider +
//! credential (SQLite row + OS keychain), forwards `agent.run.start` to the
//! sidecar, and streams every observable step back as Tauri events.
//!
//! The sidecar never sees a key until this call: material rides only on the
//! transient JSON-RPC params (ADR 0001/0006; resolve secrets at the point of use).

use serde::Serialize;
use serde_json::json;
use tauri::State;

use crate::commands::provider::{resolve_api_key, runtime_provider_config};
use crate::state::AppState;
use yukinal_database::models::AiProviderConfig;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptPart {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStartResponse {
    pub run_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStopResponse {
    pub stopped: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRespondResponse {
    pub accepted: bool,
}

/// 第一个启用的 AI provider；没有就明确报错（UI 引导去配置，不做假 provider）。
fn resolve_provider(
    state: &AppState,
    provider_id: Option<&str>,
) -> Result<AiProviderConfig, String> {
    let providers = state
        .database
        .providers()
        .list_ai()
        .map_err(|error| error.to_string())?;
    providers
        .into_iter()
        .find(|provider| {
            provider_id
                .map(|id| provider.id == id)
                .unwrap_or(provider.enabled)
                && provider.enabled
        })
        .ok_or_else(|| {
            "没有启用的 AI provider：请先到「设置 ▸ Provider」配置（baseUrl/model/API key）".into()
        })
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn agent_run_start(
    state: State<'_, AppState>,
    run_id: Option<String>,
    session_id: String,
    prompt: String,
    message_id: Option<String>,
    parts: Option<Vec<PromptPart>>,
    delivery: Option<String>,
    resume: Option<bool>,
    provider_id: Option<String>,
    model: Option<String>,
    workspace_id: Option<String>,
    focus_server_id: Option<String>,
    permission_mode: Option<String>,
    // Run mode (readonly / plan / goal). Forwarded verbatim: the sidecar's
    // permission engine owns the meaning, so Rust must not reinterpret it.
    mode: Option<String>,
) -> Result<RunStartResponse, String> {
    // Repair legacy databases before resolving the provider. The UI normally does
    // this through provider_list, but run.start must remain safe when invoked
    // directly or while the startup query is still refreshing.
    crate::commands::provider::normalize_active_provider(&state)?;
    let provider = resolve_provider(&state, provider_id.as_deref())?;
    let api_key = resolve_api_key(&state, &provider)?;
    let selected_model = model
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| provider.model.clone());

    // Millisecond timestamps can collide when two submissions arrive in the
    // same tick; use the process-wide opaque id generator instead.
    let run_id = run_id
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| crate::commands::server::next_id("run"));
    let message_id = message_id.unwrap_or_else(|| format!("msg_{run_id}"));
    let parts = parts.filter(|items| !items.is_empty()).unwrap_or_else(|| {
        vec![PromptPart {
            kind: "text".into(),
            text: prompt.clone(),
        }]
    });
    let provider_config = runtime_provider_config(&provider, &selected_model, api_key, 120_000);

    let mut params = json!({
        "runId": run_id,
        "sessionId": session_id,
        "prompt": prompt,
        "messageId": message_id,
        "parts": parts
            .into_iter()
            .map(|part| json!({ "type": part.kind, "text": part.text }))
            .collect::<Vec<_>>(),
        "delivery": delivery.unwrap_or_else(|| "async".into()),
        "resume": resume.unwrap_or(true),
        "providerConfig": provider_config,
    });
    if let Some(workspace_id) = workspace_id.as_deref() {
        params["workspaceId"] = json!(workspace_id);
    }
    if let Some(server_id) = focus_server_id.as_deref() {
        let server = state
            .database
            .servers()
            .get(server_id)
            .map_err(|error| error.to_string())?;
        let mut target = json!({
            "host": "remote",
            "serverId": server.id,
            "environment": server.metadata.environment,
        });
        if let Some(workspace_id) = workspace_id.as_deref() {
            target["workspaceId"] = json!(workspace_id);
        }
        params["focusServerId"] = json!(server_id);
        params["target"] = target;
    }
    if let Some(permission_mode) = permission_mode {
        params["permissionMode"] = json!(permission_mode);
    }
    if let Some(mode) = mode {
        params["mode"] = json!(mode);
    }
    let response = state
        .supervisor
        .request(
            "agent.run.start",
            params,
            std::time::Duration::from_secs(10),
        )
        .await
        .map_err(|error| error.to_string())?;
    let returned_run_id = response
        .get("runId")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "agent sidecar returned an invalid run.start response".to_string())?;
    if returned_run_id != run_id {
        return Err("agent sidecar returned a different run id".into());
    }
    Ok(RunStartResponse { run_id })
}

#[tauri::command]
pub async fn agent_run_stop(
    state: State<'_, AppState>,
    run_id: String,
) -> Result<RunStopResponse, String> {
    let response = state
        .supervisor
        .request(
            "agent.run.stop",
            json!({ "runId": run_id }),
            std::time::Duration::from_secs(10),
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok(RunStopResponse {
        stopped: response
            .get("stopped")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| "agent sidecar returned an invalid run.stop response".to_string())?,
    })
}

#[tauri::command]
pub async fn agent_approval_respond(
    state: State<'_, AppState>,
    approval_id: String,
    run_id: String,
    decision: String,
) -> Result<ApprovalRespondResponse, String> {
    let response = state
        .supervisor
        .request(
            "agent.approval.respond",
            json!({
                "approvalId": approval_id,
                "runId": run_id,
                "decision": decision,
                "respondedAt": yukinal_core::sidecar::iso8601_now(),
            }),
            std::time::Duration::from_secs(10),
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok(ApprovalRespondResponse {
        accepted: response
            .get("accepted")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| "agent sidecar returned an invalid approval response".to_string())?,
    })
}
