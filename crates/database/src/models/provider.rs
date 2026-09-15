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
    /// Anthropic 的日期版本头。只有 native Anthropic 协议使用它，其他 kind 必须是
    /// `None`；共享 schema 与保存命令都会拒绝越界组合。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_version: Option<String>,
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
    #[serde(default)]
    pub http_auth_headers: Vec<McpHttpAuthHeaderConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<McpOAuthConfig>,
    pub enabled: bool,
    pub allowed_tools: Vec<String>,
    /// "reviewed" | "unreviewed"
    pub trust_level: String,
}

/// One ordered static HTTP authentication header. The name is public; the
/// credential reference resolves through the OS credential store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpHttpAuthHeaderConfig {
    pub name: String,
    pub credential_ref: String,
}

/// Non-secret OAuth configuration plus an opaque reference to the token bundle.
/// The token endpoint is cached after discovery so request-time refresh does not
/// need to fetch metadata again.
///
/// `flow` is part of the stored identity: switching between the browser redirect and
/// the device-code flow changes which requests the authorization server saw, so it
/// invalidates a stored token exactly like changing the client id does
/// (`commands/mcp.rs::resolve_oauth_config`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpOAuthConfig {
    pub issuer: String,
    pub client_id: String,
    #[serde(default)]
    pub flow: McpOAuthFlow,
    /// How this client authenticates at the token endpoint.
    ///
    /// A dynamically registered client is always `none`: the server may hand back a
    /// secret in its registration response, and using it would silently turn a public
    /// client into one that only works while that extra value survives.
    #[serde(default)]
    pub client_auth: McpOAuthClientAuth,
    /// Credential-store reference for the hand-entered client secret. The secret itself
    /// never enters SQLite.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret_ref: Option<String>,
    /// Whether access tokens must be sender-constrained (RFC 9449 DPoP).
    ///
    /// Off by default: only a server that actually validates proofs benefits, and a client
    /// that sends them to one that does not has only added a header. When it is on, the
    /// token response must say `token_type: DPoP` — see ADR 0018.
    #[serde(default)]
    pub dpop: bool,
    /// Credential-store reference for the DPoP private key. The key itself never enters
    /// SQLite, and losing it costs a re-authorization rather than a silent downgrade.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dpop_key_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
}

/// How the desktop obtains the first token bundle.
///
/// Both flows end in the same refreshable bundle and the same credential-store entry;
/// they differ only in how the user proves they are present. Kept as a closed enum so
/// an unknown value in a stored row or an IPC payload is refused instead of silently
/// falling back to one of them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpOAuthFlow {
    /// Redirect to the authorization endpoint and listen on a loopback callback.
    #[default]
    AuthorizationCode,
    /// RFC 8628: show a `user_code`, then poll the token endpoint.
    DeviceCode,
}

/// Client authentication at the token endpoint (RFC 6749 §2.3).
///
/// `none` is a public client: it sends only its `client_id`. The two secret methods are
/// the ones a server can actually distinguish, and the choice has to be explicit because
/// sending a secret the server did not ask for is a credential leak with no upside.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpOAuthClientAuth {
    /// Public client: `client_id` in the form, no secret anywhere.
    #[default]
    None,
    /// `client_id` and `client_secret` as form parameters.
    ClientSecretPost,
    /// `client_id:client_secret` in the `Authorization` header, and nothing in the form.
    ClientSecretBasic,
}

impl McpOAuthClientAuth {
    /// Does this method require a stored secret?
    pub fn needs_secret(self) -> bool {
        !matches!(self, Self::None)
    }
}
