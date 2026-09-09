//! OpenCode configuration import.
//!
//! OpenCode keeps provider metadata in `opencode.json`/`opencode.jsonc` and
//! credentials in a separate `auth.json`.  We only read those files.  Secrets stay
//! inside this module until the Tauri provider command stores them in the OS
//! credential store; the UI receives metadata only.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::ccswitch::WireApi;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodeProvider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub model: String,
    pub wire_api: WireApi,
    pub models: Vec<OpenCodeModel>,
    pub custom_headers: BTreeMap<String, String>,
    api_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodeModel {
    pub id: String,
    pub label: String,
    pub context_window: Option<u64>,
}

impl OpenCodeProvider {
    #[must_use]
    pub fn has_api_key(&self) -> bool {
        self.api_key.is_some()
    }

    /// The key is intentionally only exposed to the importing command.
    #[must_use]
    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OpenCodeError {
    #[error("OpenCode 配置不存在（已配置过 OpenCode 吗？）：{0}")]
    NotFound(String),
    #[error("读取 OpenCode 配置失败：{0}")]
    Io(String),
    #[error("OpenCode 配置无法解析：{0}")]
    Malformed(String),
}

/// Read OpenCode's global provider configuration.  Missing files are reported as
/// `NotFound` so startup auto-import can quietly try the next local source.
pub fn read_opencode(home: &Path) -> Result<Vec<OpenCodeProvider>, OpenCodeError> {
    let config_path = config_path(home).ok_or_else(|| {
        OpenCodeError::NotFound(
            "未找到 ~/.config/opencode/opencode.json(c) 或 %APPDATA%\\opencode\\opencode.json(c)"
                .into(),
        )
    })?;
    let config_text = std::fs::read_to_string(&config_path)
        .map_err(|error| OpenCodeError::Io(format!("{}: {error}", config_path.display())))?;
    let config = parse_jsonc(&config_text)
        .map_err(|error| OpenCodeError::Malformed(format!("{}: {error}", config_path.display())))?;

    let auth = auth_object(home)?;
    let providers = config
        .get("provider")
        .and_then(Value::as_object)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|(id, value)| parse_provider(id, value, &config, &auth, &config_path))
                .collect()
        })
        .unwrap_or_default();
    Ok(providers)
}

fn config_path(home: &Path) -> Option<PathBuf> {
    let configured = std::env::var_os("OPENCODE_CONFIG").map(PathBuf::from);
    let mut candidates = Vec::new();
    if let Some(path) = configured {
        candidates.push(path);
    }
    candidates.extend([
        home.join(".config/opencode/opencode.jsonc"),
        home.join(".config/opencode/opencode.json"),
        home.join("AppData/Roaming/opencode/opencode.jsonc"),
        home.join("AppData/Roaming/opencode/opencode.json"),
    ]);
    candidates.into_iter().find(|path| path.is_file())
}

fn auth_object(home: &Path) -> Result<Map<String, Value>, OpenCodeError> {
    let configured = std::env::var_os("OPENCODE_AUTH_JSON").map(PathBuf::from);
    let candidates = configured.into_iter().chain([
        home.join(".local/share/opencode/auth.json"),
        home.join("AppData/Local/opencode/auth.json"),
        home.join("AppData/Roaming/opencode/auth.json"),
    ]);
    let Some(path) = candidates.into_iter().find(|path| path.is_file()) else {
        return Ok(Map::new());
    };
    let text = std::fs::read_to_string(&path)
        .map_err(|error| OpenCodeError::Io(format!("{}: {error}", path.display())))?;
    let value = parse_jsonc(&text)
        .map_err(|error| OpenCodeError::Malformed(format!("{}: {error}", path.display())))?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| OpenCodeError::Malformed(format!("{}: 需要 JSON 对象", path.display())))
}

fn parse_provider(
    id: &str,
    value: &Value,
    config: &Value,
    auth: &Map<String, Value>,
    config_path: &Path,
) -> Option<OpenCodeProvider> {
    let object = value.as_object()?;
    let options = object.get("options").and_then(Value::as_object);
    let base_url = string_field(options, "baseURL")
        .or_else(|| string_field(options, "baseUrl"))
        .or_else(|| string_field(Some(object), "baseURL"))
        .or_else(|| string_field(Some(object), "baseUrl"))
        .or_else(|| default_base_url(id))?;

    let models = object
        .get("models")
        .and_then(Value::as_object)
        .map(|entries| {
            entries
                .iter()
                .map(|(model_id, model)| OpenCodeModel {
                    id: model_id.clone(),
                    label: model
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or(model_id)
                        .to_string(),
                    context_window: model
                        .get("limit")
                        .and_then(Value::as_object)
                        .and_then(|limit| limit.get("context"))
                        .and_then(as_u64),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let configured_model = config
        .get("model")
        .and_then(Value::as_str)
        .and_then(|model| model_for_provider(model, id));
    let model = configured_model.or_else(|| models.first().map(|model| model.id.clone()))?;

    let headers = options
        .and_then(|value| value.get("headers"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let header_api_key = headers.iter().find_map(|(key, value)| {
        let key_lower = key.to_ascii_lowercase();
        if !matches!(
            key_lower.as_str(),
            "authorization" | "api-key" | "x-api-key"
        ) {
            return None;
        }
        let value = value.as_str()?.trim();
        Some(value.strip_prefix("Bearer ").unwrap_or(value).to_string())
    });
    let api_key = string_field(options, "apiKey")
        .and_then(|value| {
            resolve_secret(&value, config_path.parent().unwrap_or(Path::new(".")), auth)
        })
        .or(header_api_key)
        .or_else(|| auth_key(auth, id));
    let custom_headers = headers
        .into_iter()
        .filter(|(key, _)| !is_sensitive_header(key))
        .filter_map(|(key, value)| value.as_str().map(|value| (key, value.to_string())))
        .collect::<BTreeMap<_, _>>();
    let wire_api = string_field(options, "wireApi")
        .or_else(|| string_field(Some(object), "wireApi"))
        .map(|wire| {
            if wire.eq_ignore_ascii_case("responses") {
                WireApi::Responses
            } else {
                WireApi::Chat
            }
        })
        .unwrap_or_else(|| {
            if object
                .get("npm")
                .and_then(Value::as_str)
                .is_some_and(|npm| npm == "@ai-sdk/openai")
            {
                WireApi::Responses
            } else {
                WireApi::Chat
            }
        });

    Some(OpenCodeProvider {
        id: format!("opencode:{id}"),
        name: string_field(Some(object), "name").unwrap_or_else(|| id.to_string()),
        base_url: base_url.trim_end_matches('/').to_string(),
        model,
        wire_api,
        models,
        custom_headers,
        api_key,
    })
}

fn string_field(object: Option<&Map<String, Value>>, key: &str) -> Option<String> {
    object
        .and_then(|object| object.get(key))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
}

fn model_for_provider(model: &str, provider: &str) -> Option<String> {
    let (prefix, model_id) = model.split_once('/')?;
    (prefix == provider && !model_id.is_empty()).then(|| model_id.to_string())
}

fn default_base_url(provider: &str) -> Option<String> {
    match provider {
        "openai" => Some("https://api.openai.com/v1".into()),
        "openrouter" => Some("https://openrouter.ai/api/v1".into()),
        "groq" => Some("https://api.groq.com/openai/v1".into()),
        "deepseek" => Some("https://api.deepseek.com/v1".into()),
        _ => None,
    }
}

fn auth_key(auth: &Map<String, Value>, provider: &str) -> Option<String> {
    let value = auth.get(provider)?;
    value
        .get("key")
        .and_then(Value::as_str)
        .or_else(|| value.as_str())
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
}

fn is_sensitive_header(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|character| character.to_ascii_lowercase())
        .collect::<String>();
    matches!(
        normalized.as_str(),
        "authorization" | "apikey" | "xapikey" | "token" | "secret"
    )
}

fn resolve_secret(value: &str, base_dir: &Path, auth: &Map<String, Value>) -> Option<String> {
    if let Some(name) = value
        .strip_prefix("{env:")
        .and_then(|value| value.strip_suffix('}'))
    {
        return std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty());
    }
    if let Some(path) = value
        .strip_prefix("{file:")
        .and_then(|value| value.strip_suffix('}'))
    {
        let path = path.strip_prefix("~/").map_or_else(
            || PathBuf::from(path),
            |relative| {
                std::env::var_os("USERPROFILE")
                    .or_else(|| std::env::var_os("HOME"))
                    .map(PathBuf::from)
                    .unwrap_or_default()
                    .join(relative)
            },
        );
        let path = if path.is_absolute() {
            path
        } else {
            base_dir.join(path)
        };
        return std::fs::read_to_string(path)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
    }
    if let Some(provider) = value
        .strip_prefix("{cred:")
        .and_then(|value| value.strip_suffix('}'))
    {
        return auth_key(auth, provider);
    }
    Some(value.to_string()).filter(|value| !value.trim().is_empty())
}

fn as_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

/// Parse JSON with the comments and trailing commas accepted by OpenCode's JSONC
/// config files. The scanner keeps quoted strings byte-for-byte intact.
fn parse_jsonc(raw: &str) -> Result<Value, String> {
    let mut cleaned = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(ch) = chars.next() {
        if in_string {
            cleaned.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            cleaned.push(ch);
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'/') {
            chars.next();
            for next in chars.by_ref() {
                if next == '\n' {
                    cleaned.push('\n');
                    break;
                }
            }
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut previous = '\0';
            for next in chars.by_ref() {
                if previous == '*' && next == '/' {
                    break;
                }
                previous = next;
            }
            continue;
        }
        cleaned.push(ch);
    }

    let mut without_trailing = String::with_capacity(cleaned.len());
    let mut chars = cleaned.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(ch) = chars.next() {
        if in_string {
            without_trailing.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            without_trailing.push(ch);
            continue;
        }
        if ch == ',' {
            let mut lookahead = chars.clone();
            while matches!(lookahead.peek(), Some(value) if value.is_whitespace()) {
                lookahead.next();
            }
            if matches!(lookahead.peek(), Some('}') | Some(']')) {
                continue;
            }
        }
        without_trailing.push(ch);
    }
    serde_json::from_str(&without_trailing).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{parse_jsonc, resolve_secret};
    use serde_json::{json, Map};

    #[test]
    fn parses_jsonc_comments_and_trailing_commas() {
        let value = parse_jsonc(
            r#"{
          // provider config
          "provider": { "openai": { "models": { "gpt": {}, }, }, },
        }"#,
        )
        .expect("jsonc");
        assert_eq!(value["provider"]["openai"]["models"]["gpt"], json!({}));
    }

    #[test]
    fn resolves_env_and_credential_placeholders_without_changing_literals() {
        std::env::set_var("YUKINAL_OPENCODE_TEST_KEY", "env-secret");
        let auth = Map::from_iter([(String::from("demo"), json!({ "key": "auth-secret" }))]);
        assert_eq!(
            resolve_secret(
                "{env:YUKINAL_OPENCODE_TEST_KEY}",
                std::path::Path::new("."),
                &auth
            ),
            Some("env-secret".into())
        );
        assert_eq!(
            resolve_secret("{cred:demo}", std::path::Path::new("."), &auth),
            Some("auth-secret".into())
        );
        assert_eq!(
            resolve_secret("literal", std::path::Path::new("."), &auth),
            Some("literal".into())
        );
        std::env::remove_var("YUKINAL_OPENCODE_TEST_KEY");
    }
}
