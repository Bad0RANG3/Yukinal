//! AI provider 配置命令：设置页写入 provider_configs 行；apiKey 只进 OS keychain，
//! SQLite 只存 credentialRef（不落盘、不进日志）。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tauri::State;

use crate::commands::activity::record_user_activity;
use crate::state::AppState;
use yukinal_core::provider::{
    primary_provider_id, runtime_provider_config, sanitize_custom_headers,
};
use yukinal_credentials::{CredentialRef, CredentialStore, Secret};
use yukinal_database::models::{
    ActivityOutcome, ActivityType, AiProviderConfig, AiProviderKind, ProviderModelOption,
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
    custom_headers: Option<Map<String, Value>>,
    api_version: Option<String>,
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

    // Anthropic versions its protocol header by date. Other kinds must not persist
    // a value that their adapter would ignore.
    let api_version = api_version
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if let Some(value) = api_version.as_deref() {
        if kind != AiProviderKind::Anthropic {
            return Err(format!(
                "kind `{}` 没有 apiVersion：只有 Anthropic Messages API 使用这个日期版本头。",
                kind.as_str()
            ));
        }
        if !is_iso_date(value) {
            return Err(format!(
                "apiVersion `{value}` 无效：必须使用 YYYY-MM-DD 格式。"
            ));
        }
    }

    // The shared schema is the first gate, but this command is also a direct Tauri
    // entry point. Reject anything the sanitizer would have to drop instead of
    // silently saving a different header set than the caller supplied.
    let custom_headers = match custom_headers {
        Some(headers) => {
            let sanitized = sanitize_custom_headers(Some(&headers));
            if sanitized.as_ref().map_or(0, Map::len) != headers.len() {
                return Err(
                    "自定义请求头包含未批准或可能携带凭据的名称或值；请只填写非敏感网关元数据。"
                        .into(),
                );
            }
            sanitized
        }
        None => None,
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
        custom_headers,
        api_version,
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

/// `YYYY-MM-DD`，且年月日字段范围正确。只按位数判断会把 `2026-99-99` 存进库，
/// 然后在真实请求里变成一个难解释的 400。
fn is_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let Some(year) = value[0..4].parse::<u16>().ok() else {
        return false;
    };
    let Some(month) = value[5..7].parse::<u8>().ok() else {
        return false;
    };
    let Some(day) = value[8..10].parse::<u8>().ok() else {
        return false;
    };
    if year < 1970 || !(1..=12).contains(&month) {
        return false;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max_day = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    (1..=max_day).contains(&day)
}

// ---------------------------------------------------------------------------
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
) -> Result<ProviderTestResponse, String> {
    let provider = state
        .database
        .providers()
        .get_ai(&provider_id)
        .map_err(|error| error.to_string())?;
    let api_key = resolve_api_key(&state, &provider)?;
    let provider_config = runtime_provider_config(&provider, &provider.model, api_key, 30_000);
    let response = state
        .supervisor
        .request(
            "provider.test",
            provider_config,
            std::time::Duration::from_secs(35),
        )
        .await
        .map_err(|error| format!("模型测试失败：{error}"))?;
    let ok = response
        .get("ok")
        .and_then(Value::as_bool)
        .ok_or_else(|| "agent sidecar returned an invalid provider.test response".to_string())?;
    Ok(ProviderTestResponse { ok })
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

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderActivateResponse {
    pub provider: AiProviderConfig,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDeleteResponse {
    pub deleted: bool,
    /// 那份 keychain 条目在删除后不再被别的 Provider 引用，于是被一并移除。
    pub credential_reclaimed: bool,
}

/// 删除一个 Provider，并回收它**独占的**密钥条目。
///
/// 两条规则来自同一处先例（`server_delete` 的 `reclaim_if_unshared`）：
///
/// 1. 密钥引用**可以被多行共享** —— 库里就有四行 `deepseek` 指向同一份
///    `keychain://openai/ccswitch_codex_…`（导入留下的）。所以只有在没有别的 Provider
///    还引用它时才删；无条件删会把另外几行的密钥一起抽走，而那几行看起来一切正常，
///    直到下一次运行报「密钥读不出来」。
/// 2. 但也不能留下没人引用的孤儿条目：`reclaim` 的注释里说过这件事（拿不回来，也没人
///    会清）。
///
/// 「删除当前启用的那一个」是**允许**的，而且不是半坏状态：`normalize_active_provider`
/// 会在下一次 `provider_list`（以及每次运行开始前）按同样的确定性规则挑出新的当前项。
/// 所以这里既不阻止删除，也不在背后替用户启用另一个 —— 那会是一次没人要求的配置改动。
#[tauri::command]
pub async fn provider_delete(
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<ProviderDeleteResponse, String> {
    // 先确认它存在：一个不存在的 id 该报错，而不是回一句「删了」。这条错误消息与
    // `provider_activate` 逐字相同 —— 界面把它直接显示给用户，两处不该有两种说法。
    let providers = state
        .database
        .providers()
        .list_ai()
        .map_err(|error| error.to_string())?;
    let provider = providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| format!("未找到 Provider `{provider_id}`"))?;
    let reference = provider.api_key_credential_ref.clone();
    // 「还有别人引用它吗」必须在删除**之前**问：行一删，这条信息就没了。这条守卫在数据库层
    // 问的是整张表（AI 与 infra 两族都算），所以二次确认里那句话是字面成立的。
    let shared = match reference.as_deref() {
        Some(reference) => state
            .database
            .providers()
            .credential_ref_used_elsewhere(reference, &provider_id)
            .map_err(|error| error.to_string())?,
        None => false,
    };

    // 顺序与 `server_delete` 相同：先删行、再回收引用。回收失败会作为错误返回，而那一行
    // 已经删掉了 —— 这个取舍照抄自那处先例，代价是「删了配置、留了一个没人引用的条目」，
    // 而不是反过来「密钥没了、留了一行用不了的配置」。
    state
        .database
        .providers()
        .delete(&provider_id)
        .map_err(|error| error.to_string())?;

    let credential_reclaimed = match reference.as_deref() {
        Some(reference) if !shared => {
            let reference = CredentialRef::parse(reference).map_err(|error| error.to_string())?;
            state
                .credentials
                .delete(&reference)
                .map_err(|error| error.to_string())?;
            true
        }
        _ => false,
    };

    record_user_activity(
        &state,
        None,
        ActivityType::Configuration,
        "已删除 AI Provider",
        None,
        ActivityOutcome::Success,
    )?;
    Ok(ProviderDeleteResponse {
        deleted: true,
        credential_reclaimed,
    })
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

/// 「这份 provider 的凭据现在解析得出来吗」。
///
/// 判定规则（没有 ref 时只有本地端点算可用、ref 解不出来即不可用）住在
/// [`yukinal_core::provider::provider_is_usable`]；这里只负责把 keychain 查一次交给它 ——
/// 凭据存储不属于 `yukinal-core`，所以解析动作是本层的参数。
fn provider_is_usable(state: &AppState, provider: &AiProviderConfig) -> bool {
    yukinal_core::provider::provider_is_usable(provider, |reference| {
        let reference = CredentialRef::parse(reference).ok()?;
        let secret = state.credentials.get(&reference).ok()?;
        secret.as_utf8().ok().map(std::borrow::Cow::into_owned)
    })
}

#[cfg(test)]
mod tests {
    use super::is_iso_date;
    use super::ProviderDeleteResponse;
    use serde_json::{json, Value};
    use yukinal_database::models::AiProviderKind;

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

    /// 契约 fixture 是两侧共同解析的那一份 JSON；这里断言 Rust 那一半 —— serde 的输出必须
    /// 等于 TypeScript 那一半（`schemas/ipc.test.ts`）解析的文件。
    ///
    /// `provider_delete` 是新命令，所以顺手把它做成两侧都钉住的那一半：provider 这一族此前
    /// 一份 Rust 断言都没有（`docs/limitations.md` 的「当前限制」就是这么写的）。
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

        // 两个字段都是必填的：只回一个 `{deleted:true}` 等于把「密钥没删」和「不知道」
        // 变成同一个值，而二次确认里问的正是这件事。
        assert!(
            actual.get("credentialReclaimed").is_some(),
            "the caller cannot tell whether the key is gone without this field"
        );
    }

    #[test]
    fn anthropic_api_version_validation_rejects_plausible_looking_dates() {
        for valid in ["2023-06-01", "2024-02-29", "2026-12-31"] {
            assert!(is_iso_date(valid), "{valid} must be accepted");
        }
        for invalid in [
            "",
            "2023-6-1",
            "2023-13-01",
            "2023-02-29",
            "2023-04-31",
            "not-a-date",
        ] {
            assert!(!is_iso_date(invalid), "{invalid} must be rejected");
        }
    }
}
