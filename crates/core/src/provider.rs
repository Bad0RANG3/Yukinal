//! Provider 选择的纯规则与请求头消毒。
//!
//! 这些规则原先住在 `apps/desktop/src-tauri/src/commands/provider.rs` 里，而按本仓库的
//! 分层规则（`apps/desktop/src-tauri` 只做参数编组与事件转发，真逻辑放 `crates/*`，这样
//! 不开窗口也能测），它们属于本 crate：它们是对 [`AiProviderConfig`] 的纯函数，不碰
//! Tauri、不碰数据库、不碰 keychain。
//!
//! 唯一需要外部世界的两处被显式收成参数：
//! - [`provider_is_usable`] 不知道凭据存储长什么样，所以「这份引用解不解得出来」由调用方
//!   用一个闭包回答；
//! - [`primary_provider_id`] 同理，接受一个「可用吗」的谓词。
//!
//! 因此本模块的每一条规则都能在没有窗口、没有 keychain 的情况下被测试。

use serde_json::Value;
use yukinal_database::models::{AiProviderConfig, AiProviderKind};

/// 可以安全随请求带出去的网关元数据头白名单。
///
/// 这是**白名单**而不是黑名单，方向是刻意的：黑名单要穷举所有凭据头的写法
/// （`Authorization`、`X-Api-Key`、各家网关的自定义 token 头……），漏一个就是把密钥
/// 写进 SQLite；白名单漏一个只会让某个无害的头不被保留，代价小得多，而且是可见的。
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
#[must_use]
pub fn runtime_provider_config(
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

/// 当前该启用哪一份 provider（`None` = 没有任何一份可用）。
///
/// 偏好「已启用」的那一份，但它必须**同时**可用；否则退到可用集合里最新的那份。这样一个
/// 指向已删除 keychain 条目的陈旧行不会把整族 provider 拖成不可用。
///
/// 淘汰顺序是 `updatedAt` → `createdAt` → `id`：三层都不同才可能平手，而最后那层让结果
/// 与 order-by 无关 —— 库里的旧数据真的会有多行 `enabled = 1`，那时候「谁是当前」必须由
/// 数据决定，不能由这一行是哪次查询先返回决定。
#[must_use]
pub fn primary_provider_id<F>(providers: &[AiProviderConfig], is_usable: F) -> Option<String>
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

/// 这份 provider 现在能不能用：没有凭据引用时，只有本地端点算可用；有引用时，问 `lookup`
/// 它解不解得出一份非空 secret。
///
/// 凭据存储不属于本 crate，所以解析动作由调用方给出；**判定规则**留在这里 ——
/// 「没有 ref 是合法状态」「ref 解不出来 = 不可用」这两条是两个不同的结论，不该由每个
/// 调用点各自重新推导一遍。
///
/// `lookup` 对形状不合法的引用返回 `None`，与「条目不存在」不可区分：两者对调用方的含义
/// 都是「拿不到密钥」，所以这一层不需要更强的类型。
#[must_use]
pub fn provider_is_usable<F>(provider: &AiProviderConfig, lookup: F) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    let Some(reference) = provider.api_key_credential_ref.as_deref() else {
        return is_local_endpoint(&provider.base_url);
    };
    lookup(reference).is_some_and(|secret| !secret.trim().is_empty())
}

/// 这个 base URL 指向本机吗。
///
/// 它决定「没有密钥也算可用」这条豁免能不能适用：本机的 Ollama / LM Studio 不需要 key，
/// 而任何**非**本机端点没有密钥就是配错了。
#[must_use]
pub fn is_local_endpoint(base_url: &str) -> bool {
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

/// 只留下可以安全持久化的网关元数据头。
///
/// 它同时用在两处：写进 payload，以及**从数据库行里清掉**已经被存下来的凭据头
/// （见调用点的 `normalize_active_provider`）。两处用同一个函数是刻意的 —— 只有一处做
/// 消毒，另一处就会成为「旧数据里的密钥永远留在库里」的那条路。
#[must_use]
pub fn sanitize_custom_headers(
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

/// 单独一个头算不算「安全的元数据」。
///
/// 除了白名单，还拒绝：空值、超长值、含 CR/LF 的值（头注入），以及看起来就是一份
/// `Bearer` / `Basic` 凭据的值。最后一条挡的是「用户把 token 填进了 `User-Agent`」这种
/// 白名单拦不住的写法 —— 名字合法不代表值不敏感。
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
