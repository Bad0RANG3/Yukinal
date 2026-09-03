//! AI provider 配置命令：设置页写入 provider_configs 行；apiKey 只进 OS keychain，
//! SQLite 存 credentialRef（S10 规则：不落盘、不进日志）。

use serde::Serialize;
use tauri::State;

use crate::state::AppState;
use yukinal_credentials::{CredentialStore, Secret};
use yukinal_database::models::{AiProviderConfig, AiProviderKind};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderListResponse {
    pub providers: Vec<AiProviderConfig>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSaveResponse {
    pub provider: AiProviderConfig,
}

#[tauri::command]
pub async fn provider_list(state: State<'_, AppState>) -> Result<ProviderListResponse, String> {
    let providers = state
        .database
        .providers()
        .list_ai()
        .map_err(|error| error.to_string())?;
    Ok(ProviderListResponse { providers })
}

/// 保存 OpenAI-compatible provider。apiKey 给了就换一份（进 keychain）；不给就保留
/// 旧引用（不然每次保存都要重新粘贴 key）。
#[tauri::command]
pub async fn provider_save_openai(
    state: State<'_, AppState>,
    base_url: String,
    model: String,
    label: Option<String>,
    api_key: Option<String>,
) -> Result<ProviderSaveResponse, String> {
    let existing = state
        .database
        .providers()
        .list_ai()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|provider| provider.enabled);

    let id = existing
        .as_ref()
        .map(|provider| provider.id.clone())
        .unwrap_or_else(|| crate::commands::server::next_id("prv"));

    let api_key_credential_ref = match api_key {
        Some(key) if !key.trim().is_empty() => {
            let reference = state
                .credentials
                .set("openai", "default", &Secret::from_utf8(key))
                .map_err(|error| error.to_string())?;
            Some(reference.to_string_ref())
        }
        // 没给新 key：沿用旧的（没有旧的就保持无 key，本地端点场景）。
        _ => existing
            .as_ref()
            .and_then(|provider| provider.api_key_credential_ref.clone()),
    };

    let now = yukinal_core::sidecar::iso8601_now();
    let provider = AiProviderConfig {
        id: id.clone(),
        kind: AiProviderKind::OpenaiCompatible,
        label: label.unwrap_or_else(|| base_url.clone()),
        base_url: base_url.trim().trim_end_matches('/').to_string(),
        model: model.trim().to_string(),
        api_key_credential_ref,
        enabled: true,
        custom_headers: None,
        max_input_tokens: None,
        created_at: existing
            .as_ref()
            .map(|provider| provider.created_at.clone())
            .unwrap_or_else(|| now.clone()),
        updated_at: now,
    };
    state
        .database
        .providers()
        .upsert_ai(&provider)
        .map_err(|error| error.to_string())?;

    Ok(ProviderSaveResponse { provider })
}
