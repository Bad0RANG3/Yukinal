//! AI provider 配置命令：设置页写入 provider_configs 行；apiKey 只进 OS keychain，
//! SQLite 只存 credentialRef（不落盘、不进日志）。

use serde::Serialize;
use tauri::State;

use crate::state::AppState;
use yukinal_credentials::{CredentialRef, CredentialStore, Secret};
use yukinal_database::models::{AiProviderConfig, AiProviderKind, ProviderModelOption};

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
#[allow(clippy::too_many_arguments)]
pub async fn provider_save_openai(
    state: State<'_, AppState>,
    base_url: String,
    model: String,
    label: Option<String>,
    api_key: Option<String>,
    provider_id: Option<String>,
    wire_api: Option<String>,
    models: Option<Vec<ProviderModelOption>>,
) -> Result<ProviderSaveResponse, String> {
    let existing = state
        .database
        .providers()
        .list_ai()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|provider| {
            provider_id
                .as_deref()
                .map(|id| provider.id == id)
                .unwrap_or(provider.enabled)
        });

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
        wire_api: wire_api.unwrap_or_else(|| {
            existing
                .as_ref()
                .map(|provider| provider.wire_api.clone())
                .unwrap_or_else(|| "chat".into())
        }),
        models: models.or_else(|| {
            existing
                .as_ref()
                .and_then(|provider| provider.models.clone())
        }),
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
    activate_only(&state, &provider.id)?;

    Ok(ProviderSaveResponse { provider })
}

// ---------------------------------------------------------------------------
// CC Switch 导入（第三方供应商切换工具，如 codex 的 My Codex）

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CcSwitchImportListResponse {
    pub providers: Vec<serde_json::Value>,
}

/// 列出 cc-switch 里可导入的候选。**绝不返回 apiKey**：key 在 apply 时才
/// 由 Rust 进程内取出并进 keychain。
#[tauri::command]
pub async fn provider_import_ccswitch(
    _state: State<'_, AppState>,
) -> Result<CcSwitchImportListResponse, String> {
    let home = user_home()?;
    let providers =
        yukinal_core::ccswitch::read_ccswitch(&home).map_err(|error| error.to_string())?;

    let items: Vec<serde_json::Value> = providers
        .into_iter()
        .map(|provider| {
            let models = provider.models;
            serde_json::json!({
                "id": provider.id,
                "name": provider.name,
                "baseUrl": provider.base_url.clone(),
                "model": provider.model.clone(),
                "wireApi": provider.wire_api.as_str(),
                "hasApiKey": provider.has_api_key,
                "models": models,
            })
        })
        .collect();
    Ok(CcSwitchImportListResponse { providers: items })
}

/// 应用一个候选：Rust 读 key → keychain；SQLite 只存 provider 行（含 wireApi）。
#[tauri::command]
pub async fn provider_import_ccswitch_apply(
    state: State<'_, AppState>,
    cc_switch_provider_id: String,
) -> Result<ProviderSaveResponse, String> {
    let home = user_home()?;
    let providers =
        yukinal_core::ccswitch::read_ccswitch(&home).map_err(|error| error.to_string())?;
    let found = providers
        .into_iter()
        .find(|provider| provider.id == cc_switch_provider_id)
        .ok_or_else(|| format!("cc-switch 中没有 `{cc_switch_provider_id}`（可能已被删除）"))?;

    let api_key_credential_ref = match found.api_key() {
        Some(key) => {
            // Each imported provider gets its own keychain account. Reusing a
            // fixed account would make importing provider B silently replace
            // provider A's credential reference.
            let account = format!(
                "ccswitch_{}",
                cc_switch_provider_id.replace([':', '/'], "_")
            );
            let reference = state
                .credentials
                .set("openai", &account, &Secret::from_utf8(key.to_string()))
                .map_err(|error| error.to_string())?;
            Some(reference.to_string_ref())
        }
        None => None,
    };

    let now = yukinal_core::sidecar::iso8601_now();
    let provider = AiProviderConfig {
        id: crate::commands::server::next_id("prv"),
        kind: AiProviderKind::OpenaiCompatible,
        label: found.name.clone(),
        base_url: found.base_url.trim_end_matches('/').to_string(),
        model: found.model.clone(),
        api_key_credential_ref,
        enabled: true,
        custom_headers: None,
        max_input_tokens: None,
        wire_api: match found.wire_api {
            yukinal_core::ccswitch::WireApi::Responses => "responses".into(),
            yukinal_core::ccswitch::WireApi::Chat => "chat".into(),
        },
        models: Some(
            found
                .models
                .iter()
                .map(|model| ProviderModelOption {
                    id: model.id.clone(),
                    label: model.label.clone(),
                    context_window: model.context_window,
                    supports_tool_calling: model.supports_tool_calling,
                    supports_streaming: model.supports_streaming,
                })
                .collect(),
        ),
        created_at: now.clone(),
        updated_at: now,
    };
    state
        .database
        .providers()
        .upsert_ai(&provider)
        .map_err(|error| error.to_string())?;
    activate_only(&state, &provider.id)?;
    Ok(ProviderSaveResponse { provider })
}

fn user_home() -> Result<std::path::PathBuf, String> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
        .ok_or_else(|| "无法确定用户目录（USERPROFILE/HOME 均缺失）".to_string())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelsResponse {
    pub models: Vec<ProviderModelOption>,
}

#[tauri::command]
pub async fn provider_models(
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<ProviderModelsResponse, String> {
    let provider = state
        .database
        .providers()
        .get_ai(&provider_id)
        .map_err(|error| error.to_string())?;
    let cached = provider.models.clone().unwrap_or_else(|| {
        vec![ProviderModelOption {
            id: provider.model.clone(),
            label: provider.model.clone(),
            context_window: None,
            supports_tool_calling: true,
            supports_streaming: true,
        }]
    });
    let api_key = resolve_api_key(&state, &provider)?;
    let response = state
        .supervisor
        .request(
            "provider.models",
            serde_json::json!({
                "kind": "openai-compatible",
                "baseUrl": provider.base_url,
                "model": provider.model,
                "apiKey": api_key,
                "customHeaders": provider.custom_headers.clone(),
                "timeoutMs": 30_000,
                "wireApi": provider.wire_api.clone(),
            }),
            std::time::Duration::from_secs(35),
        )
        .await
        .map_err(|error| format!("model endpoint unavailable: {error}"))?;
    let models = response
        .get("models")
        .cloned()
        .and_then(|value| serde_json::from_value::<Vec<ProviderModelOption>>(value).ok())
        .filter(|models| !models.is_empty())
        .unwrap_or(cached);
    let mut updated = provider;
    updated.models = Some(models.clone());
    updated.updated_at = yukinal_core::sidecar::iso8601_now();
    state
        .database
        .providers()
        .upsert_ai(&updated)
        .map_err(|error| error.to_string())?;
    Ok(ProviderModelsResponse { models })
}

fn resolve_api_key(
    state: &AppState,
    provider: &AiProviderConfig,
) -> Result<Option<String>, String> {
    let Some(reference) = provider.api_key_credential_ref.as_deref() else {
        return Ok(None);
    };
    let reference = CredentialRef::parse(reference).map_err(|error| error.to_string())?;
    let secret = state
        .credentials
        .get(&reference)
        .map_err(|error| error.to_string())?;
    secret
        .as_utf8()
        .map(|value| Some(value.into_owned()))
        .map_err(|error| error.to_string())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderActivateResponse {
    pub provider: AiProviderConfig,
}

#[tauri::command]
pub async fn provider_activate(
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<ProviderActivateResponse, String> {
    let providers = state
        .database
        .providers()
        .list_ai()
        .map_err(|error| error.to_string())?;
    let selected = providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .cloned()
        .ok_or_else(|| format!("未找到 Provider `{provider_id}`"))?;
    for mut provider in providers {
        let should_enable = provider.id == provider_id;
        if provider.enabled != should_enable {
            provider.enabled = should_enable;
            state
                .database
                .providers()
                .upsert_ai(&provider)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(ProviderActivateResponse {
        provider: AiProviderConfig {
            enabled: true,
            ..selected
        },
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalCodexImportListResponse {
    pub providers: Vec<serde_json::Value>,
}

#[tauri::command]
pub async fn provider_import_codex(
    _state: State<'_, AppState>,
) -> Result<LocalCodexImportListResponse, String> {
    let providers =
        yukinal_core::ccswitch::read_codex(&user_home()?).map_err(|error| error.to_string())?;
    Ok(LocalCodexImportListResponse {
        providers: providers
            .into_iter()
            .map(|provider| {
                serde_json::json!({
                    "id": provider.id,
                    "name": provider.name,
                    "baseUrl": provider.base_url,
                    "model": provider.model,
                    "wireApi": provider.wire_api.as_str(),
                    "hasApiKey": provider.has_api_key,
                    "models": provider.models,
                })
            })
            .collect(),
    })
}

#[tauri::command]
pub async fn provider_import_codex_apply(
    state: State<'_, AppState>,
    codex_provider_id: String,
    model: Option<String>,
) -> Result<ProviderSaveResponse, String> {
    let providers =
        yukinal_core::ccswitch::read_codex(&user_home()?).map_err(|error| error.to_string())?;
    let found = providers
        .into_iter()
        .find(|provider| provider.id == codex_provider_id)
        .ok_or_else(|| format!("本地 Codex 配置中没有 `{codex_provider_id}`"))?;
    let api_key_credential_ref = found
        .api_key()
        .map(|key| {
            state
                .credentials
                .set("openai", "codex_local", &Secret::from_utf8(key.to_string()))
                .map(|reference| reference.to_string_ref())
                .map_err(|error| error.to_string())
        })
        .transpose()?;
    let now = yukinal_core::sidecar::iso8601_now();
    let provider = AiProviderConfig {
        id: crate::commands::server::next_id("prv"),
        kind: AiProviderKind::OpenaiCompatible,
        label: found.name,
        base_url: found.base_url,
        model: model.unwrap_or(found.model),
        api_key_credential_ref,
        enabled: true,
        custom_headers: None,
        max_input_tokens: None,
        wire_api: found.wire_api.as_str().to_string(),
        models: Some(
            found
                .models
                .into_iter()
                .map(|model| ProviderModelOption {
                    id: model.id,
                    label: model.label,
                    context_window: model.context_window,
                    supports_tool_calling: model.supports_tool_calling,
                    supports_streaming: model.supports_streaming,
                })
                .collect(),
        ),
        created_at: now.clone(),
        updated_at: now,
    };
    state
        .database
        .providers()
        .upsert_ai(&provider)
        .map_err(|error| error.to_string())?;
    activate_only(&state, &provider.id)?;
    Ok(ProviderSaveResponse { provider })
}

fn activate_only(state: &AppState, provider_id: &str) -> Result<(), String> {
    let providers = state
        .database
        .providers()
        .list_ai()
        .map_err(|error| error.to_string())?;
    for mut provider in providers {
        let enabled = provider.id == provider_id;
        if provider.enabled != enabled {
            provider.enabled = enabled;
            provider.updated_at = yukinal_core::sidecar::iso8601_now();
            state
                .database
                .providers()
                .upsert_ai(&provider)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}
