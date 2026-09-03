//! `provider_configs` (AI + infrastructure families) and `mcp_servers`.
//!
//! Family is a column, not a table per type: the two configs share the same
//! lifecycle (enable/disable, label, credential reference) and only differ in
//! which extra fields are populated.

use rusqlite::{params, OptionalExtension, Row};

use crate::models::{
    AiProviderConfig, AiProviderKind, InfrastructureProviderConfig, McpServerConfig,
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
                    enabled, custom_headers, max_input_tokens, wire_api, created_at, updated_at
                 ) VALUES (?1, 'ai', 'openai-compatible', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(id) DO UPDATE SET
                    label = ?2, base_url = ?3, model = ?4, api_key_credential_ref = ?5,
                    enabled = ?6, custom_headers = ?7, max_input_tokens = ?8, wire_api = ?9, updated_at = ?11",
                params![
                    config.id,
                    config.label,
                    config.base_url,
                    config.model,
                    config.api_key_credential_ref,
                    config.enabled,
                    optional_json_string(&config.custom_headers)?,
                    config.max_input_tokens,
                    config.wire_api,
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

fn row_to_ai(row: &Row<'_>) -> rusqlite::Result<AiProviderConfig> {
    Ok(AiProviderConfig {
        id: row.get(0)?,
        kind: AiProviderKind::OpenaiCompatible,
        label: row.get(2)?,
        base_url: row.get(3)?,
        model: row.get(4)?,
        api_key_credential_ref: row.get(5)?,
        enabled: row.get::<_, i64>(6)? != 0,
        custom_headers: optional_json(row.get::<_, Option<String>>(7)?)
            .map_err(|error| err(7, error))?,
        max_input_tokens: row.get::<_, Option<i64>>(8)?.map(|v| v as u32),
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
        wire_api: row
            .get::<_, Option<String>>(14)?
            .unwrap_or_else(|| "chat".to_string()),
    })
}

fn row_to_infra(row: &Row<'_>) -> rusqlite::Result<InfrastructureProviderConfig> {
    Ok(InfrastructureProviderConfig {
        id: row.get(0)?,
        kind: row.get(1)?,
        label: row.get(2)?,
        credential_ref: row.get(9)?,
        enabled: row.get::<_, i64>(6)? != 0,
        settings: optional_json(row.get::<_, Option<String>>(10)?)
            .map_err(|error| err(10, error))?,
    })
}

fn row_to_provider(row: &Row<'_>) -> rusqlite::Result<ProviderRow> {
    match row.get::<_, String>(13)?.as_str() {
        "ai" => row_to_ai(row).map(ProviderRow::Ai),
        "infra" => row_to_infra(row).map(ProviderRow::Infra),
        other => Err(err(13, format!("unknown provider family {other}"))),
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

fn err(index: usize, error: impl std::fmt::Display) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        error.to_string().into(),
    )
}

// ---------------------------------------------------------------------------
// mcp_servers

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
            let mut statement = connection.prepare(
                "SELECT id, label, transport, command, args, url, enabled, allowed_tools, trust_level
                 FROM mcp_servers ORDER BY label",
            )?;
            let rows = statement.query_map([], |row| {
                Ok(McpServerConfig {
                    id: row.get(0)?,
                    label: row.get(1)?,
                    transport: row.get(2)?,
                    command: row.get(3)?,
                    args: optional_json(row.get::<_, Option<String>>(4)?)
                        .map_err(|error| err(4, error))?,
                    url: row.get(5)?,
                    enabled: row.get::<_, i64>(6)? != 0,
                    allowed_tools: serde_json::from_str(&row.get::<_, String>(7)?)
                        .map_err(|error| err(7, error))?,
                    trust_level: row.get(8)?,
                })
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
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
