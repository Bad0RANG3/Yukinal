//! AI provider 配置命令：设置页写入 provider_configs 行；apiKey 只进 OS keychain，
//! SQLite 只存 credentialRef（不落盘、不进日志）。

use serde::Serialize;
use serde_json::Value;
use tauri::State;

use crate::commands::activity::record_user_activity;
use crate::state::AppState;
use yukinal_credentials::{CredentialRef, CredentialStore, Secret};
use yukinal_database::models::{
    ActivityOutcome, ActivityType, AiProviderConfig, AiProviderKind, ProviderModelOption,
};

const SAFE_METADATA_HEADER_NAMES: &[&str] = &[
    "http-referer",
    "referer",
    "origin",
    "user-agent",
    "x-app-name",
    "x-app-version",
    "x-client-name",
    "x-client-version",
    "x-title",
];

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

/// 原生协议各自的公开端点，用于 provider 行里没有（或只有一个空白）base URL 的情况。
///
/// **有意不为 `openai-compatible` 提供默认值。** 那个 kind 覆盖的是我们不拥有的端点
/// （OpenRouter、Ollama、vLLM、内部网关……），填一个 `https://api.openai.com/v1` 的默认值
/// 等于把用户的密钥送去一个他从未选择的服务商 —— 这是「默认值」这个词唯一真正危险的地方。
/// 原生协议的端点则没有歧义：协议本身就是那家服务商的（ADR 0011 第 5 点）。
const ANTHROPIC_DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const GEMINI_DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com";

/// Build the sidecar provider payload without serializing absent optional values
/// as JSON null. The shared runtime schema treats apiKey/customHeaders as optional
/// fields, so `null` would be a contract violation.
///
/// `kind` 与 `wireApi` 在这里分开处理，因为两者是正交的轴：`wireApi` 只在
/// `openai-compatible` 里被写进 payload，另外两种 kind 连这个键都不出现 —— 共享 schema
/// 把「原生 kind 带 wireApi」判为非法输入，所以「顺手带上」会直接让运行以 INVALID_PARAMS 失败。
pub(crate) fn runtime_provider_config(
    provider: &AiProviderConfig,
    model: &str,
    api_key: Option<String>,
    timeout_ms: u64,
) -> Value {
    let kind = provider.kind;
    let base_url = match kind {
        AiProviderKind::Anthropic => base_url_or(provider, ANTHROPIC_DEFAULT_BASE_URL),
        AiProviderKind::Gemini => base_url_or(provider, GEMINI_DEFAULT_BASE_URL),
        AiProviderKind::OpenaiCompatible => provider.base_url.trim().to_string(),
    };
    let mut config = serde_json::json!({
        "kind": kind.as_str(),
        "baseUrl": base_url,
        "model": model,
        "timeoutMs": timeout_ms,
    });
    if let Some(wire_api) = wire_api_of(provider) {
        config["wireApi"] = serde_json::json!(wire_api);
    }
    if let Some(api_key) = api_key {
        config["apiKey"] = serde_json::json!(api_key);
    }
    if let Some(custom_headers) = sanitize_custom_headers(provider.custom_headers.as_ref()) {
        config["customHeaders"] = serde_json::json!(custom_headers);
    }
    config
}

/// 行里的方言，**仅当这个 kind 有方言轴**。原生协议没有，所以这里返回 `None`：
/// payload 里连 `wireApi` 这个键都不该出现（共享 schema 拒绝「原生 kind 带 wireApi」，
/// 而不是把它当作可以忽略的字段）。
fn wire_api_of(provider: &AiProviderConfig) -> Option<&str> {
    if !provider.kind.has_wire_api() {
        return None;
    }
    provider
        .wire_api
        .as_deref()
        .filter(|value| !value.trim().is_empty())
}

/// 行里的 base URL，空白时退到该协议自己的公开端点。
fn base_url_or(provider: &AiProviderConfig, fallback: &str) -> String {
    let configured = provider.base_url.trim().trim_end_matches('/');
    if configured.is_empty() {
        fallback.to_string()
    } else {
        configured.to_string()
    }
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

/// 保存一个 AI provider（三种 kind 共用这一条保存路径，ADR 0011 第 6 点）。
///
/// 命令名曾经是 `provider_save_openai` —— 一个把「只有一种 kind」写进名字里的名字；kind
/// 现在是**必填参数**，因为一个不说明自己是什么的 Provider 配置根本没法解释：同一个
/// base URL 用 OpenAI 方言还是 Messages API 去调，是完全不同的两件事。未知 kind 在这里
/// 就失败，而不是落库成一个「看起来正常」的 openai-compatible。
///
/// apiKey 给了就换一份（进 keychain）；不给就保留旧引用（不然每次保存都要重新粘贴 key）。
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn provider_save(
    state: State<'_, AppState>,
    kind: String,
    base_url: String,
    model: String,
    label: Option<String>,
    api_key: Option<String>,
    provider_id: Option<String>,
    wire_api: Option<String>,
    models: Option<Vec<ProviderModelOption>>,
) -> Result<ProviderSaveResponse, String> {
    // `from_db`（严格拼写）而不是 `from_db_column`：旧拼写只该被**读**进来，写出的一律是
    // `as_str()` 的当前拼写，所以受理一个旧写法只会让界面能把历史拼写再存一遍。
    let kind = AiProviderKind::from_db(kind.trim()).ok_or_else(|| {
        format!(
            "不支持的 Provider kind `{}`：只支持 openai-compatible、anthropic、gemini。",
            kind.trim()
        )
    })?;
    let requested_id = provider_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(id) = requested_id {
        if !id
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
            || !id.chars().all(|character| {
                character.is_ascii_lowercase()
                    || character.is_ascii_digit()
                    || character == '-'
                    || character == '_'
            })
        {
            return Err(
                "Provider ID 只能使用小写字母、数字、连字符和下划线，且首字符不能是符号。".into(),
            );
        }
    }
    let existing = state
        .database
        .providers()
        .list_ai()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|provider| {
            requested_id
                .map(|id| provider.id == id)
                .unwrap_or(provider.enabled)
        });

    // `kind` 与 `wireApi` 正交：原生协议只有一种请求形状，所以给它们配一个方言是**请求错误**，
    // 不是被忽略的字段。共享 schema 已经拒绝这种组合；这里再拒一次，因为这个参数会直接落库。
    let requested_wire_api = wire_api
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if !kind.has_wire_api() && requested_wire_api.is_some() {
        return Err(format!(
            "kind `{}` 没有 wireApi：原生协议只有一种请求形状。",
            kind.as_str()
        ));
    }
    let wire_api = if kind.has_wire_api() {
        if let Some(value) = requested_wire_api {
            if value != "chat" && value != "responses" {
                return Err(format!(
                    "wireApi `{value}` 无效：只支持 chat 或 responses。"
                ));
            }
        }
        Some(
            requested_wire_api
                .map(str::to_string)
                .or_else(|| {
                    existing
                        .as_ref()
                        .and_then(|provider| provider.wire_api.clone())
                })
                .unwrap_or_else(|| "chat".into()),
        )
    } else {
        None
    };

    let id = existing
        .as_ref()
        .map(|provider| provider.id.clone())
        .or_else(|| requested_id.map(str::to_string))
        .unwrap_or_else(|| crate::commands::server::next_id("prv"));

    let api_key_credential_ref = match api_key {
        Some(key) if !key.trim().is_empty() => {
            let reference = state
                .credentials
                .set("openai", &format!("provider_{id}"), &Secret::from_utf8(key))
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
        kind,
        label: label.unwrap_or_else(|| base_url.clone()),
        base_url: base_url.trim().trim_end_matches('/').to_string(),
        model: model.trim().to_string(),
        api_key_credential_ref,
        enabled: true,
        custom_headers: None,
        max_input_tokens: None,
        wire_api,
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

/// 用一次真实的最小生成请求验证 provider 能通：端点、凭据、模型、接口类型。
///
/// 参数只带 `providerId`，配置和密钥都在 Rust 侧取出（密钥绝不经过 IPC 往返），
/// 期望的回复内容由 sidecar 判定，这里只负责把结论和原因带回 UI。
#[tauri::command]
pub async fn provider_test(
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<Value, String> {
    let provider = state
        .database
        .providers()
        .get_ai(&provider_id)
        .map_err(|error| error.to_string())?;
    let api_key = resolve_api_key(&state, &provider)?;
    let provider_config = runtime_provider_config(&provider, &provider.model, api_key, 30_000);
    state
        .supervisor
        .request(
            "provider.test",
            provider_config,
            std::time::Duration::from_secs(35),
        )
        .await
        .map_err(|error| format!("模型测试失败：{error}"))
}

/// 解析 provider 的 apiKey：SQLite 里只有 credentialRef，材料在 OS keychain。
///
/// 密钥在使用点解析，绝不写进 provider 行、日志或 IPC 参数之外的任何地方。
///
/// 这里同时是 `agent_run.rs` 的出处：两个模块原本各有一份行为完全相同的副本，
/// 而「没有 credentialRef 是合法状态、不是错误」这条规则（本地端点如 Ollama
/// 不需要 key）只应该有一个实现，否则两份副本很容易在这条规则上分叉。
pub(crate) fn resolve_api_key(
    state: &AppState,
    provider: &AiProviderConfig,
) -> Result<Option<String>, String> {
    let Some(reference) = provider.api_key_credential_ref.as_deref() else {
        return Ok(None); // 本地端点（Ollama 等）不需要 key
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
        let sanitized_headers = sanitize_custom_headers(provider.custom_headers.as_ref());
        let headers_changed = provider.custom_headers != sanitized_headers;
        if provider.enabled != should_enable || headers_changed {
            provider.enabled = should_enable;
            provider.custom_headers = sanitized_headers;
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

fn sanitize_custom_headers(
    headers: Option<&serde_json::Map<String, Value>>,
) -> Option<serde_json::Map<String, Value>> {
    let values = headers?;
    let safe = values
        .iter()
        .filter_map(|(name, value)| {
            let text = value.as_str()?;
            is_safe_metadata_header(name, text)
                .then(|| (name.clone(), Value::String(text.to_string())))
        })
        .collect::<serde_json::Map<_, _>>();
    (!safe.is_empty()).then_some(safe)
}

fn is_safe_metadata_header(name: &str, value: &str) -> bool {
    let normalized = name.trim().to_ascii_lowercase();
    SAFE_METADATA_HEADER_NAMES.contains(&normalized.as_str())
        && !value.trim().is_empty()
        && value.len() <= 4_096
        && !value.contains('\r')
        && !value.contains('\n')
        && !value.to_ascii_lowercase().starts_with("bearer ")
        && !value.to_ascii_lowercase().starts_with("basic ")
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
    use super::{
        is_local_endpoint, primary_provider_id, runtime_provider_config, sanitize_custom_headers,
    };
    use serde_json::{json, Value};
    use yukinal_database::models::{AiProviderConfig, AiProviderKind};

    #[test]
    fn ai_provider_kind_uses_shared_wire_spelling() {
        assert_eq!(
            serde_json::to_value(AiProviderKind::OpenaiCompatible).unwrap(),
            json!("openai-compatible")
        );
        // 三种 kind 的拼写必须与 `packages/shared` 的 `AI_PROVIDER_KINDS` 逐字一致：
        // 两端各写一遍枚举，其中一端漂了只会表现为「界面存得进、Agent 认不出」。
        assert_eq!(
            serde_json::to_value(AiProviderKind::Anthropic).unwrap(),
            json!("anthropic")
        );
        assert_eq!(
            serde_json::to_value(AiProviderKind::Gemini).unwrap(),
            json!("gemini")
        );
    }

    fn provider(id: &str, enabled: bool) -> AiProviderConfig {
        AiProviderConfig {
            id: id.into(),
            kind: AiProviderKind::OpenaiCompatible,
            label: id.into(),
            base_url: "http://127.0.0.1:1234".into(),
            model: "test-model".into(),
            wire_api: Some("chat".into()),
            api_key_credential_ref: None,
            enabled,
            custom_headers: None,
            max_input_tokens: None,
            models: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn provider_of_kind(kind: AiProviderKind, base_url: &str) -> AiProviderConfig {
        AiProviderConfig {
            kind,
            base_url: base_url.into(),
            // 只有 openai-compatible 有方言轴；另外两种的 `None` 是读路径会产出的形状。
            wire_api: kind.has_wire_api().then(|| "chat".to_string()),
            ..provider("prv_kind", true)
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

    /// 每种 kind 都要产出**自己那个协议**的配置（ADR 0011 第 6 点）。
    #[test]
    fn runtime_provider_config_speaks_the_protocol_of_each_kind() {
        let compatible = runtime_provider_config(
            &provider_of_kind(
                AiProviderKind::OpenaiCompatible,
                "https://gw.example.com/v1",
            ),
            "gpt-5.2",
            None,
            30_000,
        );
        assert_eq!(compatible["kind"], json!("openai-compatible"));
        assert_eq!(compatible["baseUrl"], json!("https://gw.example.com/v1"));
        assert_eq!(compatible["wireApi"], json!("chat"));

        let anthropic = runtime_provider_config(
            &provider_of_kind(AiProviderKind::Anthropic, "https://api.anthropic.com"),
            "claude-sonnet-4-5",
            None,
            30_000,
        );
        assert_eq!(anthropic["kind"], json!("anthropic"));
        assert_eq!(anthropic["baseUrl"], json!("https://api.anthropic.com"));
        // 原生协议没有方言轴：这个键不该出现，否则共享 schema 会拒绝整份配置。
        assert!(anthropic.get("wireApi").is_none());

        let gemini = runtime_provider_config(
            &provider_of_kind(
                AiProviderKind::Gemini,
                "https://generativelanguage.googleapis.com",
            ),
            "gemini-2.5-flash",
            None,
            30_000,
        );
        assert_eq!(gemini["kind"], json!("gemini"));
        assert_eq!(
            gemini["baseUrl"],
            json!("https://generativelanguage.googleapis.com")
        );
        assert!(gemini.get("wireApi").is_none());
    }

    /// 没有 base URL 时，原生协议退到**自己**的公开端点，而不是一个 OpenAI 形状的地址；
    /// openai-compatible 则没有默认值 —— 它覆盖的端点不是我们的，编一个默认值等于把用户的
    /// 密钥送去一个他没选过的服务商。
    #[test]
    fn a_missing_base_url_falls_back_to_the_protocols_own_endpoint() {
        for blank in ["", "   ", "/"] {
            let anthropic = runtime_provider_config(
                &provider_of_kind(AiProviderKind::Anthropic, blank),
                "claude-sonnet-4-5",
                None,
                30_000,
            );
            assert_eq!(
                anthropic["baseUrl"],
                json!("https://api.anthropic.com"),
                "blank base URL {blank:?}"
            );

            let gemini = runtime_provider_config(
                &provider_of_kind(AiProviderKind::Gemini, blank),
                "gemini-2.5-flash",
                None,
                30_000,
            );
            assert_eq!(
                gemini["baseUrl"],
                json!("https://generativelanguage.googleapis.com"),
                "blank base URL {blank:?}"
            );
        }

        let compatible = runtime_provider_config(
            &provider_of_kind(AiProviderKind::OpenaiCompatible, "  "),
            "m",
            None,
            30_000,
        );
        assert_eq!(
            compatible["baseUrl"],
            json!(""),
            "openai-compatible must not be given an invented endpoint"
        );
    }

    #[test]
    fn custom_headers_keep_only_non_secret_gateway_metadata() {
        let headers = json!({
            "HTTP-Referer": "https://desktop.example",
            "Authorization": "Bearer not-for-storage",
            "X-Api-Key": "not-for-storage",
        });
        let headers = headers.as_object().expect("header object");
        let sanitized = sanitize_custom_headers(Some(headers)).expect("safe header remains");
        assert_eq!(sanitized.len(), 1);
        assert_eq!(
            sanitized.get("HTTP-Referer"),
            Some(&Value::String("https://desktop.example".into()))
        );
    }
}
