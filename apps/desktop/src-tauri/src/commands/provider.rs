//! AI provider 配置命令：设置页写入 provider_configs 行；apiKey 只进 OS keychain，
//! SQLite 只存 credentialRef（不落盘、不进日志）。

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;
use tauri::State;

use crate::commands::activity::record_user_activity;
use crate::state::AppState;
use yukinal_credentials::{CredentialRef, CredentialStore, Secret};
use yukinal_database::models::{
    ActivityOutcome, ActivityType, AiProviderConfig, AiProviderKind, ProviderModelOption,
};

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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAutoImportResponse {
    pub imported: usize,
    pub providers: Vec<AiProviderConfig>,
}

/// Build the sidecar provider payload without serializing absent optional values
/// as JSON null. The shared runtime schema treats apiKey/customHeaders as optional
/// fields, so `null` would be a contract violation.
pub(crate) fn runtime_provider_config(
    provider: &AiProviderConfig,
    model: &str,
    api_key: Option<String>,
    timeout_ms: u64,
) -> Value {
    let mut config = serde_json::json!({
        "kind": "openai-compatible",
        "baseUrl": provider.base_url,
        "model": model,
        "timeoutMs": timeout_ms,
        "wireApi": provider.wire_api,
    });
    if let Some(api_key) = api_key {
        config["apiKey"] = serde_json::json!(api_key);
    }
    if let Some(custom_headers) = provider.custom_headers.as_ref() {
        config["customHeaders"] = serde_json::json!(custom_headers);
    }
    config
}

#[tauri::command]
pub async fn provider_list(state: State<'_, AppState>) -> Result<ProviderListResponse, String> {
    normalize_active_provider(&state)?;
    let providers = state
        .database
        .providers()
        .list_ai()
        .map_err(|error| error.to_string())?;
    Ok(ProviderListResponse { providers })
}

/// Scan local OpenCode/Codex/CC Switch configuration once the desktop starts.
///
/// Import IDs are deterministic, so repeated launches update the same rows instead
/// of creating a new provider on every boot. Existing user-selected providers stay
/// active; an imported provider is activated only when no provider is active yet.
#[tauri::command]
pub async fn provider_import_auto(
    state: State<'_, AppState>,
) -> Result<ProviderAutoImportResponse, String> {
    provider_import_auto_inner(&state)
}

/// Native startup path uses the same importer as the UI command. Keeping one
/// implementation prevents a desktop launch from depending on React having
/// already mounted before credentials are synchronized.
pub(crate) fn provider_import_auto_inner(
    state: &AppState,
) -> Result<ProviderAutoImportResponse, String> {
    let home = user_home()?;
    let had_active = state
        .database
        .providers()
        .list_ai()
        .map_err(|error| error.to_string())?
        .iter()
        .any(|provider| provider.enabled);
    let mut active = had_active;
    let mut imported = Vec::new();

    if let Ok(providers) = yukinal_core::opencode::read_opencode(&home) {
        for provider in providers {
            let api_key = provider.api_key().map(str::to_string);
            let next = upsert_imported(
                state,
                ImportedProvider {
                    source: "opencode",
                    source_id: provider.id,
                    name: provider.name,
                    base_url: provider.base_url,
                    model: provider.model,
                    wire_api: provider.wire_api.as_str().to_string(),
                    api_key,
                    custom_headers: provider.custom_headers,
                    models: provider
                        .models
                        .into_iter()
                        .map(|model| ProviderModelOption {
                            id: model.id,
                            label: model.label,
                            context_window: model.context_window,
                            supports_tool_calling: true,
                            supports_streaming: true,
                        })
                        .collect(),
                },
            )?;
            if !active {
                activate_only(state, &next.id)?;
                active = true;
            }
            imported.push(next);
        }
    }

    if let Ok(providers) = yukinal_core::ccswitch::read_codex(&home) {
        for provider in providers {
            let api_key = provider.api_key().map(str::to_string);
            let next = upsert_imported(
                state,
                ImportedProvider {
                    source: "codex",
                    source_id: provider.id,
                    name: provider.name,
                    base_url: provider.base_url,
                    model: provider.model,
                    wire_api: provider.wire_api.as_str().to_string(),
                    api_key,
                    custom_headers: BTreeMap::new(),
                    models: provider
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
                },
            )?;
            if !active {
                activate_only(state, &next.id)?;
                active = true;
            }
            imported.push(next);
        }
    }

    if let Ok(providers) = yukinal_core::ccswitch::read_ccswitch(&home) {
        for provider in providers {
            let api_key = provider.api_key().map(str::to_string);
            let next = upsert_imported(
                state,
                ImportedProvider {
                    source: "ccswitch",
                    source_id: provider.id,
                    name: provider.name,
                    base_url: provider.base_url,
                    model: provider.model,
                    wire_api: provider.wire_api.as_str().to_string(),
                    api_key,
                    custom_headers: BTreeMap::new(),
                    models: provider
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
                },
            )?;
            if !active {
                activate_only(state, &next.id)?;
                active = true;
            }
            imported.push(next);
        }
    }

    // Older databases could contain more than one enabled row. The Agent resolves
    // one provider per run, so leave the data in the same single-active state as
    // the explicit activate/save commands before returning it to the UI.
    normalize_active_provider(state)?;

    if !imported.is_empty() {
        record_user_activity(
            state,
            None,
            ActivityType::Configuration,
            "已自动导入本地 AI 配置",
            None,
            ActivityOutcome::Success,
        )?;
    }

    Ok(ProviderAutoImportResponse {
        imported: imported.len(),
        providers: imported,
    })
}

struct ImportedProvider {
    source: &'static str,
    source_id: String,
    name: String,
    base_url: String,
    model: String,
    wire_api: String,
    api_key: Option<String>,
    custom_headers: BTreeMap<String, String>,
    models: Vec<ProviderModelOption>,
}

fn upsert_imported(state: &AppState, input: ImportedProvider) -> Result<AiProviderConfig, String> {
    let existing = state
        .database
        .providers()
        .list_ai()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|provider| provider.id == imported_id(input.source, &input.source_id));
    let id = existing
        .as_ref()
        .map(|provider| provider.id.clone())
        .unwrap_or_else(|| imported_id(input.source, &input.source_id));
    let api_key_credential_ref = match input.api_key {
        Some(key) if !key.trim().is_empty() => {
            let account = format!("{}_{}", input.source, credential_account(&input.source_id));
            let reference = state
                .credentials
                .set("openai", &account, &Secret::from_utf8(key))
                .map_err(|error| error.to_string())?;
            Some(reference.to_string_ref())
        }
        _ => existing
            .as_ref()
            .and_then(|provider| provider.api_key_credential_ref.clone()),
    };
    let now = yukinal_core::sidecar::iso8601_now();
    let provider = AiProviderConfig {
        id,
        kind: AiProviderKind::OpenaiCompatible,
        label: input.name,
        base_url: input.base_url.trim_end_matches('/').to_string(),
        model: input.model,
        api_key_credential_ref,
        enabled: existing
            .as_ref()
            .map(|provider| provider.enabled)
            .unwrap_or(false),
        custom_headers: if input.custom_headers.is_empty() {
            existing
                .as_ref()
                .and_then(|provider| provider.custom_headers.clone())
        } else {
            Some(
                input
                    .custom_headers
                    .into_iter()
                    .map(|(key, value)| (key, Value::String(value)))
                    .collect(),
            )
        },
        max_input_tokens: existing
            .as_ref()
            .and_then(|provider| provider.max_input_tokens),
        wire_api: input.wire_api,
        models: (!input.models.is_empty())
            .then_some(input.models)
            .or_else(|| {
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
    Ok(provider)
}

fn imported_id(source: &str, source_id: &str) -> String {
    let mut slug = String::new();
    for character in source_id.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('_') {
            slug.push('_');
        }
    }
    while slug.ends_with('_') {
        slug.pop();
    }
    let slug = if slug.is_empty() {
        "provider"
    } else {
        slug.as_str()
    };
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in source_id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("prv_{source}_{slug}_{hash:016x}")
}

fn credential_account(source_id: &str) -> String {
    let mut account = String::new();
    for character in source_id.chars() {
        if character.is_ascii_alphanumeric() {
            account.push(character.to_ascii_lowercase());
        } else if !account.ends_with('_') {
            account.push('_');
        }
    }
    account.trim_matches('_').chars().take(80).collect()
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
    record_user_activity(
        &state,
        None,
        ActivityType::Configuration,
        "已导入 CC Switch Provider",
        None,
        ActivityOutcome::Success,
    )?;
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
    let provider_config = runtime_provider_config(&provider, &provider.model, api_key, 30_000);
    let response = state
        .supervisor
        .request(
            "provider.models",
            provider_config,
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
    record_user_activity(
        &state,
        None,
        ActivityType::Configuration,
        "已切换 AI Provider",
        None,
        ActivityOutcome::Success,
    )?;
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
    record_user_activity(
        &state,
        None,
        ActivityType::Configuration,
        "已导入本地 Codex Provider",
        None,
        ActivityOutcome::Success,
    )?;
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

fn primary_provider_id<F>(providers: &[AiProviderConfig], is_usable: F) -> Option<String>
where
    F: Fn(&AiProviderConfig) -> bool,
{
    providers
        .iter()
        .filter(|provider| is_usable(provider))
        .filter(|provider| provider.enabled)
        .max_by(provider_recency)
        .or_else(|| {
            providers
                .iter()
                .filter(|provider| is_usable(provider))
                .max_by(provider_recency)
        })
        .map(|provider| provider.id.clone())
}

fn provider_recency(left: &&AiProviderConfig, right: &&AiProviderConfig) -> std::cmp::Ordering {
    left.updated_at
        .cmp(&right.updated_at)
        .then_with(|| left.created_at.cmp(&right.created_at))
        .then_with(|| left.id.cmp(&right.id))
}

pub(crate) fn normalize_active_provider(state: &AppState) -> Result<(), String> {
    let providers = state
        .database
        .providers()
        .list_ai()
        .map_err(|error| error.to_string())?;
    // Prefer a configured provider whose credential can actually be resolved.
    // This repairs legacy rows that point at a deleted keychain entry while
    // keeping a no-key local endpoint usable.
    let primary_id =
        primary_provider_id(&providers, |provider| provider_is_usable(state, provider));

    for mut provider in providers {
        let should_enable = primary_id.as_deref() == Some(provider.id.as_str());
        if provider.enabled != should_enable {
            provider.enabled = should_enable;
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

fn provider_is_usable(state: &AppState, provider: &AiProviderConfig) -> bool {
    let Some(reference) = provider.api_key_credential_ref.as_deref() else {
        return is_local_endpoint(&provider.base_url);
    };
    let Ok(reference) = CredentialRef::parse(reference) else {
        return false;
    };
    let Ok(secret) = state.credentials.get(&reference) else {
        return false;
    };
    secret
        .as_utf8()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

fn is_local_endpoint(base_url: &str) -> bool {
    let normalized = base_url.trim().to_ascii_lowercase();
    [
        "http://localhost",
        "https://localhost",
        "http://127.0.0.1",
        "https://127.0.0.1",
        "http://[::1]",
        "https://[::1]",
    ]
    .iter()
    .any(|prefix| {
        normalized == *prefix
            || normalized.starts_with(&format!("{prefix}:"))
            || normalized.starts_with(&format!("{prefix}/"))
    })
}

#[cfg(test)]
mod tests {
    use super::{is_local_endpoint, primary_provider_id, runtime_provider_config};
    use serde_json::Value;
    use yukinal_database::models::{AiProviderConfig, AiProviderKind};

    fn provider(id: &str, enabled: bool) -> AiProviderConfig {
        AiProviderConfig {
            id: id.into(),
            kind: AiProviderKind::OpenaiCompatible,
            label: id.into(),
            base_url: "http://127.0.0.1:1234".into(),
            model: "test-model".into(),
            wire_api: "chat".into(),
            api_key_credential_ref: None,
            enabled,
            custom_headers: None,
            max_input_tokens: None,
            models: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn primary_provider_is_deterministic_when_legacy_data_has_multiple_enabled_rows() {
        let mut second = provider("prv_second", true);
        second.updated_at = "2026-01-01T00:00:01Z".into();
        let providers = vec![provider("prv_first", true), second];
        assert_eq!(
            primary_provider_id(&providers, |_| true),
            Some("prv_second".into())
        );
    }

    #[test]
    fn local_endpoints_can_work_without_a_credential() {
        assert!(is_local_endpoint("http://127.0.0.1:11434/v1"));
        assert!(is_local_endpoint("http://localhost:1234"));
        assert!(!is_local_endpoint("https://api.openai.com/v1"));
    }

    #[test]
    fn runtime_provider_config_omits_absent_optional_values() {
        let config =
            runtime_provider_config(&provider("prv_test", true), "test-model", None, 30_000);
        assert!(config.get("apiKey").is_none());
        assert!(config.get("customHeaders").is_none());
        assert_eq!(
            config.get("model"),
            Some(&Value::String("test-model".into()))
        );
    }
}
