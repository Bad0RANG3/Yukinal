//! Agent run commands: the UI sends a prompt; Rust resolves the provider +
//! credential (SQLite row + OS keychain), forwards `agent.run.start` to the
//! sidecar, and streams every observable step back as Tauri events.
//!
//! The sidecar never sees a key until this call: material rides only on the
//! transient JSON-RPC params (ADR 0001/0006; 使用点注入规则).

use serde::Serialize;
use serde_json::json;
use tauri::State;

use crate::state::AppState;
use yukinal_credentials::{CredentialRef, CredentialStore};
use yukinal_database::models::AiProviderConfig;

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

/// 解析 apiKey：SQLite 里只有 credentialRef，材料在 OS keychain（使用点解析）。
fn resolve_api_key(
    state: &AppState,
    provider: &AiProviderConfig,
) -> Result<Option<String>, String> {
    let Some(ref_) = &provider.api_key_credential_ref else {
        return Ok(None); // 本地端点（Ollama 等）不需要 key
    };
    let reference = CredentialRef::parse(ref_).map_err(|error| error.to_string())?;
    let secret = state
        .credentials
        .get(&reference)
        .map_err(|error| error.to_string())?;
    secret
        .as_utf8()
        .map(|value| Some(value.into_owned()))
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn agent_run_start(
    state: State<'_, AppState>,
    session_id: String,
    prompt: String,
    provider_id: Option<String>,
    model: Option<String>,
    workspace_id: Option<String>,
    focus_server_id: Option<String>,
) -> Result<RunStartResponse, String> {
    let provider = resolve_provider(&state, provider_id.as_deref())?;
    let api_key = resolve_api_key(&state, &provider)?;
    let selected_model = model
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| provider.model.clone());

    let run_id = format!("run_{}", yukinal_core::sidecar::iso8601_now());
    let mut params = json!({
        "runId": run_id,
        "sessionId": session_id,
        "prompt": prompt,
        "providerConfig": {
            "kind": "openai-compatible",
            "baseUrl": provider.base_url,
            "model": selected_model,
            "apiKey": api_key,
            "customHeaders": provider.custom_headers,
            "timeoutMs": 120_000,
            "wireApi": provider.wire_api,
        },
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
    let response = state
        .supervisor
        .request(
            "agent.run.start",
            params,
            std::time::Duration::from_secs(10),
        )
        .await
        .map_err(|error| error.to_string())?;
    let _ = response;
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
            .unwrap_or(false),
    })
}

#[tauri::command]
pub async fn agent_approval_respond(
    state: State<'_, AppState>,
    approval_id: String,
    decision: String,
) -> Result<ApprovalRespondResponse, String> {
    let response = state
        .supervisor
        .request(
            "agent.approval.respond",
            json!({
                "approvalId": approval_id,
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
            .unwrap_or(false),
    })
}
