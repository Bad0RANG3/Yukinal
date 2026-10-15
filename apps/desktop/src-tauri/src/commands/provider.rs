//! Tauri bridge for the Python-owned AI provider settings and model operations.
//!
//! Provider validation, persistence, activation, model discovery, and testing live in the
//! Python Agent sidecar. This module translates the desktop IPC contract and keeps host-only
//! responsibilities such as activity records and access to the OS credential store.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tauri::{AppHandle, State};

use crate::commands::activity::record_user_activity;
use crate::state::AppState;
use yukinal_database::models::{
    ActivityOutcome, ActivityType, AiProviderConfig, ProviderModelOption,
};

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderListResponse {
    pub providers: Vec<AiProviderConfig>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSaveResponse {
    pub provider: AiProviderConfig,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelsResponse {
    pub models: Vec<ProviderModelOption>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTestResponse {
    pub ok: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderActivateResponse {
    pub provider: AiProviderConfig,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDeleteResponse {
    pub deleted: bool,
    pub credential_reclaimed: bool,
}

async fn provider_request(
    app: &AppHandle,
    state: &AppState,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    crate::commands::server::call_python_sidecar(
        app,
        state,
        method,
        params,
        std::time::Duration::from_secs(35),
    )
    .await
}

fn parse_response<T: DeserializeOwned>(response: Value, field: &str) -> Result<T, String> {
    let value = response
        .get(field)
        .cloned()
        .ok_or_else(|| format!("Python Agent returned an invalid `{field}` response"))?;
    serde_json::from_value(value)
        .map_err(|_| format!("Python Agent returned an invalid `{field}` response"))
}

#[tauri::command]
pub async fn provider_list(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ProviderListResponse, String> {
    let response = provider_request(&app, &state, "provider.list", json!({})).await?;
    Ok(ProviderListResponse {
        providers: parse_response(response, "providers")?,
    })
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn provider_save(
    app: AppHandle,
    state: State<'_, AppState>,
    kind: String,
    base_url: String,
    model: String,
    label: Option<String>,
    api_key: Option<String>,
    provider_id: Option<String>,
    wire_api: Option<String>,
    custom_headers: Option<Map<String, Value>>,
    api_version: Option<String>,
    models: Option<Vec<ProviderModelOption>>,
) -> Result<ProviderSaveResponse, String> {
    let response = provider_request(
        &app,
        &state,
        "provider.save",
        json!({
            "kind": kind,
            "baseUrl": base_url,
            "model": model,
            "label": label,
            "apiKey": api_key,
            "providerId": provider_id,
            "wireApi": wire_api,
            "customHeaders": custom_headers,
            "apiVersion": api_version,
            "models": models,
        }),
    )
    .await?;
    let provider = parse_response(response, "provider")?;
    record_user_activity(
        &state,
        None,
        ActivityType::Configuration,
        "已保存 AI Provider",
        None,
        ActivityOutcome::Success,
    )?;
    Ok(ProviderSaveResponse { provider })
}

#[tauri::command]
pub async fn provider_models(
    app: AppHandle,
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<ProviderModelsResponse, String> {
    let response = provider_request(
        &app,
        &state,
        "provider.models",
        json!({ "providerId": provider_id }),
    )
    .await?;
    Ok(ProviderModelsResponse {
        models: parse_response(response, "models")?,
    })
}

#[tauri::command]
pub async fn provider_test(
    app: AppHandle,
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<ProviderTestResponse, String> {
    let response = provider_request(
        &app,
        &state,
        "provider.test",
        json!({ "providerId": provider_id }),
    )
    .await?;
    serde_json::from_value(response)
        .map_err(|_| "Python Agent returned an invalid provider test response".into())
}

#[tauri::command]
pub async fn provider_delete(
    app: AppHandle,
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<ProviderDeleteResponse, String> {
    let response = provider_request(
        &app,
        &state,
        "provider.delete",
        json!({ "providerId": provider_id }),
    )
    .await?;
    let result = serde_json::from_value::<ProviderDeleteResponse>(response)
        .map_err(|_| "Python Agent returned an invalid provider delete response".to_string())?;
    record_user_activity(
        &state,
        None,
        ActivityType::Configuration,
        "已删除 AI Provider",
        None,
        ActivityOutcome::Success,
    )?;
    Ok(result)
}

#[tauri::command]
pub async fn provider_activate(
    app: AppHandle,
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<ProviderActivateResponse, String> {
    let response = provider_request(
        &app,
        &state,
        "provider.activate",
        json!({ "providerId": provider_id }),
    )
    .await?;
    let provider = parse_response(response, "provider")?;
    record_user_activity(
        &state,
        None,
        ActivityType::Configuration,
        "已切换 AI Provider",
        None,
        ActivityOutcome::Success,
    )?;
    Ok(ProviderActivateResponse { provider })
}

#[cfg(test)]
mod tests {
    use super::ProviderDeleteResponse;
    use serde_json::{json, Value};
    use yukinal_database::models::AiProviderKind;

    #[test]
    fn ai_provider_kind_uses_shared_wire_spelling() {
        assert_eq!(
            serde_json::to_value(AiProviderKind::OpenaiCompatible).unwrap(),
            json!("openai-compatible")
        );
        assert_eq!(serde_json::to_value(AiProviderKind::Anthropic).unwrap(), json!("anthropic"));
        assert_eq!(serde_json::to_value(AiProviderKind::Gemini).unwrap(), json!("gemini"));
    }

    #[test]
    fn the_delete_response_matches_the_shared_fixture() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../../packages/shared/fixtures/ipc/provider_delete.json"
        ))
        .expect("contract fixture must be valid JSON");
        let actual = serde_json::to_value(ProviderDeleteResponse {
            deleted: true,
            credential_reclaimed: true,
        })
        .expect("serializes");
        assert_eq!(actual, fixture);
    }
}
