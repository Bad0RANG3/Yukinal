//! AI provider 配置命令：设置页写入 provider_configs 行；apiKey 只进 OS keychain，
//! SQLite 只存 credentialRef（不落盘、不进日志）。

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
        wire_api: "chat".into(),
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
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map_err(|_| "无法确定用户目录（USERPROFILE/HOME 均缺失）".to_string())?;
    let providers = yukinal_core::ccswitch::read_ccswitch(std::path::Path::new(&home))
        .map_err(|error| error.to_string())?;

    let items: Vec<serde_json::Value> = providers
        .into_iter()
        .map(|provider| {
            serde_json::json!({
                "id": provider.id,
                "name": provider.name,
                "baseUrl": provider.base_url,
                "model": provider.model,
                "wireApi": provider.wire_api.as_str(),
                "hasApiKey": provider.has_api_key,
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
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map_err(|_| "无法确定用户目录（USERPROFILE/HOME 均缺失）".to_string())?;
    let providers = yukinal_core::ccswitch::read_ccswitch(std::path::Path::new(&home))
        .map_err(|error| error.to_string())?;
    let found = providers
        .into_iter()
        .find(|provider| provider.id == cc_switch_provider_id)
        .ok_or_else(|| format!("cc-switch 中没有 `{cc_switch_provider_id}`（可能已被删除）"))?;

    let api_key_credential_ref = match found.api_key() {
        Some(key) => {
            let reference = state
                .credentials
                .set("openai", "default", &Secret::from_utf8(key.to_string()))
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
        created_at: now.clone(),
        updated_at: now,
    };
    state
        .database
        .providers()
        .upsert_ai(&provider)
        .map_err(|error| error.to_string())?;
    Ok(ProviderSaveResponse { provider })
}
