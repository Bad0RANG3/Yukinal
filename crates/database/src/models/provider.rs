//! provider configs (AI + infrastructure) and MCP servers.

use serde::{Deserialize, Serialize};

/// Non-sensitive model metadata cached from a provider catalog. The API key is
/// deliberately absent so this value is safe to persist in SQLite and return
/// to the desktop UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelOption {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    pub supports_tool_calling: bool,
    pub supports_streaming: bool,
}

// `kind` 是 `provider_configs` 的一列，所以它属于这张表：wire 拼写与列里的拼写是同一份。
enum_as_str!(AiProviderKind, OpenaiCompatible => "openai-compatible", Anthropic => "anthropic", Gemini => "gemini");

/// Provider 身份（ADR 0011）：`kind` 决定**由谁翻译**，与 `AiProviderConfig::wire_api`
/// 那条「怎么翻译」的轴正交。三个取值必须一路走到数据库列、IPC 与设置界面 —— 一个存不进去
/// 或读不回来的 kind，等于「实现了一个用户配不出来的 Provider」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AiProviderKind {
    /// 写出去的一律是前端契约里的 `openai-compatible`；`openaicompatible` 是旧的
    /// `rename_all = "lowercase"` 曾经落盘的拼写，`kind` 是 TEXT 列且没有迁移改写它，
    /// 所以只放宽读取，避免升级后旧行直接反序列化失败。
    #[serde(alias = "openaicompatible")]
    OpenaiCompatible,
    /// Anthropic Messages API（原生协议，`anthropic.ts`）。
    Anthropic,
    /// Google Gemini `generateContent`（原生协议，`gemini.ts`）。
    Gemini,
}

impl AiProviderKind {
    /// `provider_configs.kind` 列的读入。比 serde 多一件事：旧拼写 `openaicompatible`
    /// 也在这里被接受（同一条「只放宽读取」的规则，两条路径不能分叉）。
    #[must_use]
    pub fn from_db_column(raw: &str) -> Option<Self> {
        Self::from_db(raw).or_else(|| (raw == "openaicompatible").then_some(Self::OpenaiCompatible))
    }

    /// 这个 kind 是否有 `wireApi` 方言轴。**只有 OpenAI-compatible 有**：原生协议的
    /// 翻译本身就是它唯一的形式（ADR 0011 第 2 点），给它配一个方言是无意义的。
    #[must_use]
    pub fn has_wire_api(self) -> bool {
        matches!(self, Self::OpenaiCompatible)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProviderConfig {
    pub id: String,
    pub kind: AiProviderKind,
    pub label: String,
    pub base_url: String,
    pub model: String,
    /// "chat" | "responses" — 由用户按服务商端点选择。**只对 `OpenaiCompatible` 有意义**：
    /// 原生协议没有方言可选，所以那两种 kind 这里是 `None`，序列化时整个省略（界面因此
    /// 不会给它们显示一个「Wire API」），共享 schema 也把「原生 kind 带 wireApi」判为非法输入。
    ///
    /// 列是 `NOT NULL DEFAULT 'chat'`（迁移 2），写不进 NULL，所以 `None` 落的是空串；
    /// 读回来时由 `kind` 决定这一列是否被读（见 `repositories/providers.rs`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_api: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_credential_ref: Option<String>,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_headers: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_input_tokens: Option<u32>,
    /// Cached model catalog. Stored under the existing `settings` column to
    /// keep the migration backwards compatible with existing databases.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<ProviderModelOption>>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InfrastructureProviderConfig {
    pub id: String,
    pub kind: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerConfig {
    pub id: String,
    pub label: String,
    /// "stdio" | "http"
    pub transport: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub enabled: bool,
    pub allowed_tools: Vec<String>,
    /// "reviewed" | "unreviewed"
    pub trust_level: String,
}
