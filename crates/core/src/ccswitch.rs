//! CC Switch 导入：读第三方供应商切换工具（cc-switch）的本地库，把它配置的
//! 供应商（如 codex 的 `My Codex`）转成 Yukinal 可用的 provider 素材。
//!
//! 规则：
//! - 只读打开 `~/.cc-switch/cc-switch.db`（用户自己的文件，绝不写它）；
//! - 列表命令不返回 key —— UI 只能看到 baseUrl/model 等元数据；
//! - apply 时由本模块**在进程内**取出 apiKey，交给 keychain；key 不落 SQLite、
//!   不进日志、不进 UI 往返。
//!
//! codex 的 `settings_config` 结构（实测）：
//!   { "auth": {"OPENAI_API_KEY": "..."}, "config": "<TOML>", "modelCatalog": {...} }
//!   config 是 TOML：顶层 `model = "..."` / `model_provider = "custom"`，
//!   `[model_providers.custom]` 里 `base_url = "..."` / `wire_api = "responses"`
//!   / `experimental_bearer_token = "..."`。

use std::path::Path;

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireApi {
    Chat,
    Responses,
}

impl WireApi {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Responses => "responses",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CcSwitchProvider {
    /// 稳定引用：`{app_type}:{provider_id}`（apply 时用它再取 key）。
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub model: String,
    pub wire_api: WireApi,
    pub has_api_key: bool,
    /// 只在 apply 路径内存在；绝不跨出本模块分界线。
    api_key: Option<String>,
}

impl CcSwitchProvider {
    /// 仅元数据形态（列表命令返回）；key 留在结构体里由 apply 取用。
    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CcSwitchError {
    #[error("cc-switch 数据库不存在（已装 cc-switch 并配置过供应商吗？）：{0}")]
    NotFound(String),
    #[error("读取 cc-switch 数据库失败：{0}")]
    Db(String),
    #[error("cc-switch 行无法解析：{0}")]
    Malformed(String),
}

/// 读取全部 codex 类供应商（含官方与自定义中转）。
pub fn read_ccswitch(home: &Path) -> Result<Vec<CcSwitchProvider>, CcSwitchError> {
    let path = home.join(".cc-switch").join("cc-switch.db");
    if !path.is_file() {
        return Err(CcSwitchError::NotFound(path.display().to_string()));
    }

    let connection =
        rusqlite::Connection::open(&path).map_err(|error| CcSwitchError::Db(error.to_string()))?;
    connection
        .pragma_update(None, "query_only", true)
        .map_err(|error| CcSwitchError::Db(error.to_string()))?;

    let mut statement = connection
        .prepare("SELECT id, name, settings_config FROM providers WHERE app_type = 'codex'")
        .map_err(|error| CcSwitchError::Db(error.to_string()))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| CcSwitchError::Db(error.to_string()))?;

    let mut providers = Vec::new();
    for row in rows {
        let (id, name, settings) = row.map_err(|error| CcSwitchError::Db(error.to_string()))?;
        match parse_provider(&id, &name, &settings) {
            Ok(provider) => providers.push(provider),
            Err(error) => {
                // 单个供应商解析失败不炸整页：跳过并继续（UI 层面无感知）。
                tracing::warn!("cc-switch provider {name} skipped: {error}");
            }
        }
    }
    Ok(providers)
}

fn parse_provider(
    provider_id: &str,
    name: &str,
    settings: &str,
) -> Result<CcSwitchProvider, CcSwitchError> {
    let value: Value = serde_json::from_str(settings)
        .map_err(|error| CcSwitchError::Malformed(format!("{name}: {error}")))?;
    let auth = value.get("auth").and_then(Value::as_object);
    let config_toml = value.get("config").and_then(Value::as_str).unwrap_or("");

    let parsed = parse_codex_toml(config_toml);
    let base_url = parsed
        .provider
        .as_ref()
        .and_then(|block| block.get("base_url"))
        .cloned()
        .filter(|value: &String| !value.is_empty());
    let model = parsed.model.filter(|value: &String| !value.is_empty());

    let api_key = parsed
        .provider
        .as_ref()
        .and_then(|block| block.get("experimental_bearer_token"))
        .cloned()
        .or_else(|| {
            auth.and_then(|auth| auth.get("OPENAI_API_KEY"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(|value: &String| !value.trim().is_empty());

    let wire_api = match parsed
        .provider
        .as_ref()
        .and_then(|block| block.get("wire_api"))
        .map(String::as_str)
    {
        Some("responses") => WireApi::Responses,
        _ => WireApi::Chat,
    };

    let Some(base_url) = base_url else {
        return Err(CcSwitchError::Malformed(format!(
            "{name}: no compatible base_url in settings"
        )));
    };
    let Some(model) = model else {
        return Err(CcSwitchError::Malformed(format!(
            "{name}: no model in settings"
        )));
    };

    Ok(CcSwitchProvider {
        id: format!("codex:{provider_id}"),
        name: parsed
            .provider_name
            .map(|display| display.to_string())
            .unwrap_or_else(|| name.to_string()),
        base_url,
        model,
        wire_api,
        has_api_key: api_key.is_some(),
        api_key,
    })
}

/// codex TOML 子集的解析结果。`provider` = 命中的 `[model_providers.<X>]` 块。
pub struct CodexToml {
    pub model: Option<String>,
    pub provider_name: Option<String>,
    pub provider: Option<std::collections::HashMap<String, String>>,
}

/// 迷你 TOML：只解析我们需要的键（顶层 model/model_provider，块内的
/// base_url/wire_api/name/experimental_bearer_token）。解析失败记 None，
/// 因为个别供应商格式再怪也要能跳过而不是炸掉整个导入。
pub fn parse_codex_toml(text: &str) -> CodexToml {
    let mut top = std::collections::HashMap::new();
    let mut sections: std::collections::HashMap<String, std::collections::HashMap<String, String>> =
        std::collections::HashMap::new();
    let mut current: &str = "";

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            current = &line[1..line.len() - 1];
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = unquote(value.trim());
        if current.is_empty() {
            top.insert(key.to_string(), value);
        } else {
            sections
                .entry(current.to_string())
                .or_default()
                .insert(key.to_string(), value);
        }
    }

    let provider_name = top.get("model_provider").cloned();
    let provider = provider_name
        .as_ref()
        .and_then(|name| sections.get(&format!("model_providers.{name}")))
        .cloned();

    CodexToml {
        model: top.get("model").cloned(),
        provider_name,
        provider,
    }
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

/// 从 apply 参数恢复稳定引用。
pub fn parse_reference(reference: &str) -> Option<&str> {
    reference.strip_prefix("codex:")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_TOML: &str = r#"model_provider = "custom"
model = "gpt-5.6-terra"
model_reasoning_effort = "high"

[model_providers.custom]
name = "My Codex"
base_url = "https://gateway.example.com/v1"
wire_api = "responses"
requires_openai_auth = false
experimental_bearer_token = "sk-demo-token"
"#;

    #[test]
    fn parses_codex_toml_settings() {
        let parsed = parse_codex_toml(SAMPLE_TOML);
        assert_eq!(parsed.model.as_deref(), Some("gpt-5.6-terra"));
        assert_eq!(parsed.provider_name.as_deref(), Some("custom"));
        let block = parsed.provider.expect("block");
        assert_eq!(
            block.get("base_url").map(String::as_str),
            Some("https://gateway.example.com/v1")
        );
        assert_eq!(block.get("wire_api").map(String::as_str), Some("responses"));
        assert_eq!(
            block.get("experimental_bearer_token").map(String::as_str),
            Some("sk-demo-token")
        );
    }

    #[test]
    fn parses_quoted_and_bare_values() {
        let toml = r#"model = bare-model
model_provider = "x"
[model_providers.x]
base_url = "http://127.0.0.1:11434/v1"
wire_api = chat
"#;
        let parsed = parse_codex_toml(toml);
        assert_eq!(parsed.model.as_deref(), Some("bare-model"));
        assert_eq!(
            parsed.provider.unwrap().get("base_url").map(String::as_str),
            Some("http://127.0.0.1:11434/v1")
        );
    }

    #[test]
    fn provider_without_compatible_block_is_rejected() {
        let toml = "model = \"m\"\nmodel_provider = \"missing\"\n[model_providers.other]\nbase_url = \"x\"\n";
        let parsed = parse_codex_toml(toml);
        assert!(
            parsed.provider.is_none(),
            "missing block must not silently map"
        );
    }

    #[test]
    fn json_wrapper_extracts_wire_api_and_key_precedence() {
        let config = "model = \"m\"
model_provider = \"custom\"
[model_providers.custom]
base_url = \"https://x/v1\"
wire_api = \"responses\"
experimental_bearer_token = \"token-key\"
";
        let settings =
            serde_json::json!({ "auth": { "OPENAI_API_KEY": "auth-key" }, "config": config })
                .to_string();
        let provider = parse_provider("id_1", "My Codex", &settings).expect("parse");
        assert_eq!(provider.wire_api, WireApi::Responses);
        assert_eq!(
            provider.api_key.as_deref(),
            Some("token-key"),
            "token wins over auth for codex"
        );
        assert_eq!(provider.base_url, "https://x/v1");
    }

    #[test]
    fn falls_back_to_auth_key_when_no_token() {
        let config = "model = \"m\"
model_provider = \"custom\"
[model_providers.custom]
base_url = \"https://x/v1\"
";
        let settings =
            serde_json::json!({ "auth": { "OPENAI_API_KEY": "auth-key" }, "config": config })
                .to_string();
        let provider = parse_provider("id_2", "X", &settings).expect("parse");
        assert_eq!(provider.wire_api, WireApi::Chat);
        assert_eq!(provider.api_key.as_deref(), Some("auth-key"));
    }
}
