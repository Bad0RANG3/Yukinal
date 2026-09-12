//! `provider_configs` (AI + infrastructure families) and `mcp_servers`.
//!
//! Family is a column, not a table per type: the two configs share the same
//! lifecycle (enable/disable, label, credential reference) and only differ in
//! which extra fields are populated.

use rusqlite::{params, OptionalExtension, Row};

use super::decode::decode_error;
use crate::models::{
    AiProviderConfig, AiProviderKind, InfrastructureProviderConfig, McpServerConfig,
    ProviderModelOption,
};
use crate::{optional_json, Database, DatabaseError, Result};

pub struct ProviderConfigsRepository<'a> {
    db: &'a Database,
}

impl<'a> ProviderConfigsRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn upsert_ai(&self, config: &AiProviderConfig) -> Result<()> {
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO provider_configs (
                    id, family, kind, label, base_url, model, api_key_credential_ref,
                    enabled, custom_headers, max_input_tokens, settings, wire_api, created_at, updated_at
                 ) VALUES (?1, 'ai', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT(id) DO UPDATE SET
                    kind = ?2, label = ?3, base_url = ?4, api_key_credential_ref = ?6,
                    enabled = ?7, custom_headers = ?8, max_input_tokens = ?9, settings = ?10, wire_api = ?11, updated_at = ?13",
                params![
                    config.id,
                    // `kind` 是**写出来的**，而不是写死的 `'openai-compatible'`：写死的那一版
                    // 让任何一个新 kind 存进去都变成 openai-compatible（ADR 0011 点名的那处错误）。
                    config.kind.as_str(),
                    config.label,
                    config.base_url,
                    config.model,
                    config.api_key_credential_ref,
                    config.enabled,
                    optional_json_string(&config.custom_headers)?,
                    config.max_input_tokens,
                    config
                        .models
                        .as_ref()
                        .map(|models| serde_json::to_string(&serde_json::json!({ "models": models })))
                        .transpose()
                        .map_err(DatabaseError::from)?,
                    // `wire_api` 是 `NOT NULL DEFAULT 'chat'` 的列，写不进 NULL：`None`（原生
                    // kind 没有方言轴）落成空串，读回来时按 kind 决定它是不是 `None`。
                    config.wire_api.as_deref().unwrap_or(""),
                    config.created_at,
                    config.updated_at,
                ],
            )?;
            Ok(())
        })
    }

    pub fn upsert_infra(&self, config: &InfrastructureProviderConfig) -> Result<()> {
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO provider_configs (
                    id, family, kind, label, credential_ref, enabled, settings
                 ) VALUES (?1, 'infra', ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO UPDATE SET
                    kind = ?2, label = ?3, credential_ref = ?4, enabled = ?5,
                    settings = ?6, updated_at = (strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
                params![
                    config.id,
                    config.kind,
                    config.label,
                    config.credential_ref,
                    config.enabled,
                    config
                        .settings
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()?
                        .unwrap_or_default(),
                ],
            )?;
            Ok(())
        })
    }

    pub fn list_ai(&self) -> Result<Vec<AiProviderConfig>> {
        self.list_where("family = 'ai'", row_to_ai)
    }

    pub fn list_infra(&self) -> Result<Vec<InfrastructureProviderConfig>> {
        self.list_where("family = 'infra'", row_to_infra)
    }

    pub fn list_all(&self) -> Result<Vec<ProviderRow>> {
        self.list_where("1 = 1", row_to_provider)
    }

    pub fn get_ai(&self, id: &str) -> Result<AiProviderConfig> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, kind, label, base_url, model, api_key_credential_ref, enabled,
                            custom_headers, max_input_tokens, credential_ref, settings, created_at, updated_at, family, wire_api
                     FROM provider_configs WHERE id = ?1 AND family = 'ai'",
                    params![id],
                    row_to_ai,
                )
                .optional()
                .map_err(DatabaseError::from)?
                .ok_or(DatabaseError::NotFound)
        })
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        self.db.with(|connection| {
            let changed =
                connection.execute("DELETE FROM provider_configs WHERE id = ?1", params![id])?;
            if changed == 0 {
                return Err(DatabaseError::NotFound);
            }
            Ok(())
        })
    }

    fn list_where<T>(
        &self,
        filter: &str,
        mapper: impl Fn(&Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<Vec<T>> {
        self.db.with(|connection| {
            let sql = format!(
                "SELECT id, kind, label, base_url, model, api_key_credential_ref, enabled,
                        custom_headers, max_input_tokens, credential_ref, settings, created_at, updated_at, family, wire_api
                 FROM provider_configs WHERE {filter} ORDER BY label"
            );
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map([], mapper)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
        })
    }
}

/// One provider row, either family. `command`-style length; serialised camelCase so
/// the UI can render `providerStatus` without a second mapping.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderRow {
    Ai(AiProviderConfig),
    Infra(InfrastructureProviderConfig),
}

/// One row of `provider_configs` with `family = 'ai'`.
///
/// 两个方向都必须经过 `kind`（ADR 0011 的后果一节点名了这里）：读路径曾经**无视**第 1 列、
/// 把每一行都解码成 `OpenaiCompatible`，于是写进去的 `gemini` 读回来变成 openai-compatible ——
/// 一个用 OpenAI 方言去调的 Gemini 端点，而配置看起来完全正常。写路径对应地把
/// `'openai-compatible'` 写死在 SQL 里，所以它连「存进去」都做不到。
///
/// **认不出的 kind 是硬错误，不是默认值。** 把一个未知字符串当成 openai-compatible，正是上面
/// 那个失败模式：错误信息里会带着真实的 base URL，而协议是错的。所以这里走本文件其他列
/// 同样的路（`decode_error`），让它在读的那一刻就失败，而不是等到请求打出去。
/// 代价是一行坏数据会让 `list_ai()` 整个失败 —— 那也比静默换一种协议好，而且行还在库里可查。
fn row_to_ai(row: &Row<'_>) -> rusqlite::Result<AiProviderConfig> {
    let raw_kind: String = row.get(1)?;
    let kind = AiProviderKind::from_db_column(&raw_kind)
        .ok_or_else(|| decode_error(1, format!("unknown AI provider kind {raw_kind:?}")))?;
    Ok(AiProviderConfig {
        id: row.get(0)?,
        kind,
        label: row.get(2)?,
        base_url: row.get(3)?,
        model: row.get(4)?,
        api_key_credential_ref: row.get(5)?,
        enabled: row.get::<_, i64>(6)? != 0,
        custom_headers: optional_json(row.get::<_, Option<String>>(7)?)
            .map_err(|error| decode_error(7, error))?,
        max_input_tokens: row.get::<_, Option<i64>>(8)?.map(|v| v as u32),
        models: parse_models(row.get::<_, Option<String>>(10)?)
            .map_err(|error| decode_error(10, error))?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
        wire_api: wire_api_for_kind(kind, row.get::<_, Option<String>>(14)?),
    })
}

/// `wireApi` 只属于 `openai-compatible`（ADR 0011 第 2 点），所以这一列要不要读由 `kind` 决定：
/// 原生 kind 一律是 `None`，即使库里因为历史原因写着 `chat`。反过来，openai-compatible 行缺值
/// （NULL 或空串）落到这一列自己的默认值 `chat`，与迁移 2 的写法一致。
fn wire_api_for_kind(kind: AiProviderKind, stored: Option<String>) -> Option<String> {
    if !kind.has_wire_api() {
        return None;
    }
    Some(
        stored
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "chat".to_string()),
    )
}

fn parse_models(
    raw: Option<String>,
) -> std::result::Result<Option<Vec<ProviderModelOption>>, String> {
    let Some(raw) = raw.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    let value: serde_json::Value = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
    let Some(models) = value
        .get("models")
        .cloned()
        .or_else(|| value.is_array().then_some(value))
    else {
        return Ok(None);
    };
    Ok(serde_json::from_value(models).ok())
}

fn row_to_infra(row: &Row<'_>) -> rusqlite::Result<InfrastructureProviderConfig> {
    Ok(InfrastructureProviderConfig {
        id: row.get(0)?,
        kind: row.get(1)?,
        label: row.get(2)?,
        credential_ref: row.get(9)?,
        enabled: row.get::<_, i64>(6)? != 0,
        settings: optional_json(row.get::<_, Option<String>>(10)?)
            .map_err(|error| decode_error(10, error))?,
    })
}

fn row_to_provider(row: &Row<'_>) -> rusqlite::Result<ProviderRow> {
    match row.get::<_, String>(13)?.as_str() {
        "ai" => row_to_ai(row).map(ProviderRow::Ai),
        "infra" => row_to_infra(row).map(ProviderRow::Infra),
        other => Err(decode_error(13, format!("unknown provider family {other}"))),
    }
}

fn optional_json_string(
    value: &Option<serde_json::Map<String, serde_json::Value>>,
) -> Result<Option<String>> {
    value
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(DatabaseError::from)
}

// ---------------------------------------------------------------------------
// mcp_servers

/// One statement, so `list` and `get` cannot drift apart in column order.
const SELECT_ALL: &str =
    "SELECT id, label, transport, command, args, url, enabled, allowed_tools, trust_level
     FROM mcp_servers ORDER BY label";

fn row_to_mcp(row: &Row<'_>) -> rusqlite::Result<McpServerConfig> {
    Ok(McpServerConfig {
        id: row.get(0)?,
        label: row.get(1)?,
        transport: row.get(2)?,
        command: row.get(3)?,
        args: optional_json(row.get::<_, Option<String>>(4)?)
            .map_err(|error| decode_error(4, error))?,
        url: row.get(5)?,
        enabled: row.get::<_, i64>(6)? != 0,
        allowed_tools: serde_json::from_str(&row.get::<_, String>(7)?)
            .map_err(|error| decode_error(7, error))?,
        trust_level: row.get(8)?,
    })
}

pub struct McpServersRepository<'a> {
    db: &'a Database,
}

impl<'a> McpServersRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn upsert(&self, config: &McpServerConfig) -> Result<()> {
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO mcp_servers (id, label, transport, command, args, url, enabled, allowed_tools, trust_level)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(id) DO UPDATE SET
                    label = ?2, transport = ?3, command = ?4, args = ?5, url = ?6,
                    enabled = ?7, allowed_tools = ?8, trust_level = ?9",
                params![
                    config.id,
                    config.label,
                    config.transport,
                    config.command,
                    config
                        .args
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()?
                        .unwrap_or_default(),
                    config.url,
                    config.enabled,
                    serde_json::to_string(&config.allowed_tools)?,
                    config.trust_level,
                ],
            )?;
            Ok(())
        })
    }

    pub fn list(&self) -> Result<Vec<McpServerConfig>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(SELECT_ALL)?;
            let rows = statement.query_map([], row_to_mcp)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
        })
    }

    /// One row, or [`DatabaseError::NotFound`].
    ///
    /// Added with the MCP wiring: `upsert` / `list` / `delete` were the whole surface, and
    /// every caller that acts on **one** server (start it, stop it, delete it) would have
    /// had to read the whole table and find the row itself — an O(n) search that also
    /// turns "this id does not exist" into a `None` the caller has to remember to check.
    /// Same id lookups already exist next door (`ServersRepository::get`,
    /// `ProviderConfigsRepository::get_ai`), so this is the missing member of an existing
    /// shape rather than a new one.
    pub fn get(&self, id: &str) -> Result<McpServerConfig> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, label, transport, command, args, url, enabled, allowed_tools, trust_level
                     FROM mcp_servers WHERE id = ?1",
                    params![id],
                    row_to_mcp,
                )
                .optional()
                .map_err(DatabaseError::from)?
                .ok_or(DatabaseError::NotFound)
        })
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        self.db.with(|connection| {
            let changed =
                connection.execute("DELETE FROM mcp_servers WHERE id = ?1", params![id])?;
            if changed == 0 {
                return Err(DatabaseError::NotFound);
            }
            Ok(())
        })
    }
}
