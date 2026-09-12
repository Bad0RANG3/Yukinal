//! Row types mirroring `@yukinal/shared` (same names, same camelCase serialisation).
//!
//! These are the *wire shapes*: `serde_json::to_value(server_row)` produces exactly
//! what the IPC contract expects, so the command layer can hand rows to the UI
//! without a second translation. Enum string values are the dot-free literals from
//! the shared package; serde rejects anything else, so a typo fails at load time,
//! not at display time.

use serde::{Deserialize, Serialize};

/// Wire-string for enum columns without allocating JSON just to unquote it.
///
/// Also emits `ALL`, the complete variant list, so tests can assert the wire
/// contract over every variant without keeping a second copy of the list. That
/// second copy is what let `ActivityType` ship a serde rule that disagreed with
/// `as_str` for `FileChange` and `AgentAction`: the variant list lived in three
/// places and only two of them were checked.
///
/// **`from_db` is generated from the same literal list**, which is the third place
/// closed. It used to be written out by hand as a reverse `match` after every
/// invocation — ten tables restating the mapping declared three lines above them.
/// A test (`every_enum_agrees_across_serde_as_str_and_from_db`) did catch drift, but
/// a test is a check, not a construction: the bug was still *representable*, and the
/// fix for a wrong entry was to notice the failure and edit the second table. Now
/// the reverse direction cannot disagree with the forward one, because there is only
/// one list.
///
/// The match arm uses `$str` as the pattern, so the two directions are generated
/// from the same token. Adding a variant means adding one `Variant => "literal"` pair
/// and nothing else.
macro_rules! enum_as_str {
    ($ty:ident, $($variant:ident => $str:literal),+ $(,)?) => {
        impl $ty {
            /// Every variant, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            #[must_use]
            pub fn as_str(&self) -> &'static str {
                match self {
                    $(Self::$variant => $str),+
                }
            }

            /// Inverse of [`Self::as_str`]: parse a value read back from SQLite.
            ///
            /// `None` for an unrecognised string, so a row written by a newer schema
            /// (or corrupted by hand) surfaces as a typed error at the call site
            /// rather than being silently coerced to a default variant.
            #[must_use]
            pub fn from_db(raw: &str) -> Option<Self> {
                match raw {
                    $($str => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

enum_as_str!(ServerStatus, Connecting => "connecting", Connected => "connected", Disconnected => "disconnected", Error => "error");
enum_as_str!(Environment, Local => "local", Development => "development", Staging => "staging", Production => "production", Unknown => "unknown");

enum_as_str!(HealthState, Healthy => "healthy", Warning => "warning", Critical => "critical", Unknown => "unknown");
enum_as_str!(ActivityType, Connection => "connection", Authentication => "authentication", Configuration => "configuration", Deployment => "deployment", Service => "service", Container => "container", FileChange => "file_change", AgentAction => "agent_action", Approval => "approval", Health => "health");
enum_as_str!(ActivitySource, Agent => "agent", User => "user", System => "system", Docker => "docker", Git => "git", Cloud => "cloud");
enum_as_str!(ActivityOutcome, Success => "success", Failure => "failure", Cancelled => "cancelled", Denied => "denied");
enum_as_str!(ChatMessageRole, User => "user", Assistant => "assistant", Tool => "tool", System => "system");
enum_as_str!(ToolExecutionStatus, Pending => "pending", Running => "running", WaitingApproval => "waiting_approval", Success => "success", Failed => "failed", Cancelled => "cancelled");
enum_as_str!(PermissionMode, Auto => "auto", Ask => "ask", Deny => "deny");
enum_as_str!(RiskLevel, Read => "read", Low => "low", Medium => "medium", High => "high", Critical => "critical");
// `kind` 是 `provider_configs` 的一列，所以它属于这张表：wire 拼写与列里的拼写是同一份。
enum_as_str!(AiProviderKind, OpenaiCompatible => "openai-compatible", Anthropic => "anthropic", Gemini => "gemini");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatMessageRole {
    User,
    Assistant,
    Tool,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerStatus {
    Connecting,
    Connected,
    Disconnected,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    Local,
    Development,
    Staging,
    Production,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthState {
    Healthy,
    Warning,
    Critical,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    Read,
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionMode {
    Auto,
    Ask,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionStatus {
    Pending,
    Running,
    WaitingApproval,
    Success,
    Failed,
    Cancelled,
}

/// Activity type, as sent over IPC.
///
/// `snake_case`, not `lowercase`: `FileChange` and `AgentAction` are multi-word,
/// and `lowercase` is a plain `to_ascii_lowercase()` that turned them into
/// `"filechange"`/`"agentaction"`. The shared contract
/// (`packages/shared/src/types/activity.ts`) and this file's own `as_str` /
/// `from_db` both use `"file_change"`/`"agent_action"`, so the IPC payload was
/// the only place with the wrong spelling — and nothing consumed it, which is
/// why the mismatch survived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityType {
    Connection,
    Authentication,
    Configuration,
    Deployment,
    Service,
    Container,
    FileChange,
    AgentAction,
    Approval,
    Health,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivitySource {
    Agent,
    User,
    System,
    Docker,
    Git,
    Cloud,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivityOutcome {
    Success,
    Failure,
    Cancelled,
    Denied,
}

// ---------------------------------------------------------------------------
// servers

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConnection {
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linux: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docker: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub systemd: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nginx: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub postgres: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redis: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kubernetes: Option<bool>,
}

impl ServerCapabilities {
    #[must_use]
    pub fn with(mut self, key: &str, value: bool) -> Self {
        match key {
            "linux" => self.linux = Some(value),
            "docker" => self.docker = Some(value),
            "systemd" => self.systemd = Some(value),
            "nginx" => self.nginx = Some(value),
            "postgres" => self.postgres = Some(value),
            "redis" => self.redis = Some(value),
            "kubernetes" => self.kubernetes = Some(value),
            _ => {}
        }
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerMetadata {
    pub environment: Environment,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_ids: Option<Vec<String>>,
}

/// One row of `servers`. This is also the API shape of `server_list`/`server_add`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Server {
    pub id: String,
    pub name: String,
    pub connection: ServerConnection,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    pub capabilities: ServerCapabilities,
    pub status: ServerStatus,
    pub metadata: ServerMetadata,
    pub created_at: String,
    pub updated_at: String,
}

// ---------------------------------------------------------------------------
// groups / workspaces

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerGroup {
    pub id: String,
    pub name: String,
    pub server_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRepository {
    pub id: String,
    pub name: String,
    /// "local" | "remote" — never guessed, mis-targeting a repo is an incident.
    pub host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub server_ids: Vec<String>,
    pub repositories: Vec<WorkspaceRepository>,
    pub provider_ids: Vec<String>,
    pub default_environment: Environment,
}

// ---------------------------------------------------------------------------
// identities (only the reference and metadata; secret material never lands here)

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub id: String,
    pub label: String,
    /// "password" | "privateKey" | "agent"
    pub method: String,
    /// 凭据引用。agent 身份**没有**凭据条目，这里是空串 —— `identities.credential_ref`
    /// 是 `NOT NULL`，空串表示「这个身份没有 secret」，而不是编一个指向不存在条目的
    /// 假引用（那会让「引用存在」与「条目存在」这两件事对不上）。
    pub credential_ref: String,
    /// 加密私钥口令的**引用**（口令材料在 OS keychain）。`None` = 这个身份没有口令：
    /// 明文 key、密码认证、ssh-agent 都是这个形状。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passphrase_ref: Option<String>,
    pub created_at: String,
}

// ---------------------------------------------------------------------------
// provider configs (AI + infrastructure) and MCP servers

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

// ---------------------------------------------------------------------------
// add-server input（带 secret 的瞬时输入；secret 只进 keychain，不落 SQLite）

/// add-server 的认证输入（带 secret 的瞬时输入；secret 只进 keychain，不落 SQLite）。
///
/// `rename_all_fields = "camelCase"` **不是装饰**：枚举上的 `rename_all` 只改**变体名**，
/// 不变体里的字段名。少了它，`PrivateKey` 变体要的是 `private_key_pem`、`Identity`
/// 变体要的是 `identity_id`，而共享契约（`packages/shared/src/schemas/server.ts`）发出
/// 的一直是 `privateKeyPem` / `identityId` —— 于是「SSH 私钥」和「引用已有身份」两条路
/// 在 `server_add`/`server_update` 上永远只会得到 `missing field`，只有密码认证能通。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(
    tag = "method",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AuthenticationInput {
    Password {
        password: String,
    },
    PrivateKey {
        private_key_pem: String,
        /// 加密私钥的口令。空 / 纯空白 = 「没有口令」，与 `crates/ssh` 的规则一致
        /// （`load_private_key` 也把空口令过滤掉），由调用点决定是否落 keychain。
        passphrase: Option<String>,
    },
    /// ssh-agent 认证：**不携带任何 secret**，也不写 keychain 条目。
    ///
    /// agent 持有的身份由远端 agent 自己保管，Yukinal 只转交签名请求；所以这个
    /// 变体没有字段 —— 一个只描述「用哪条路径发现 agent」的 socket path 属于连接
    /// 期决策（`Authentication::Agent { socket_path: None }` = 按平台约定发现），
    /// 不该在新增服务器时被固化进数据库。
    Agent,
    /// 引用已存在的身份（不改凭据）。
    Identity {
        identity_id: String,
    },
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddServerInput {
    pub name: String,
    pub host: String,
    pub port: Option<u16>,
    pub username: String,
    pub environment: Environment,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    pub authentication: AuthenticationInput,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateServerInput {
    pub server_id: String,
    pub name: String,
    pub host: String,
    pub port: Option<u16>,
    pub username: String,
    pub environment: Environment,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    pub authentication: Option<AuthenticationInput>,
}

impl UpdateServerInput {
    pub fn from_value(value: &serde_json::Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }
}

impl AddServerInput {
    /// 从跨层 JSON 反序列化（与 `@yukinal/shared` 的 AddServerInput 同形）。
    pub fn from_value(value: &serde_json::Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }
}

// ---------------------------------------------------------------------------
// snapshots / activities / tool executions

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectorSample {
    pub collector_id: String,
    pub collected_at: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerInfo {
    pub name: String,
    pub image: String,
    pub state: String,
    pub status: String,
    pub restart_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerSnapshot {
    pub id: String,
    pub server_id: String,
    pub collected_at: String,
    pub health: HealthState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disks: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docker: Option<serde_json::Value>,
    pub capabilities: ServerCapabilities,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collectors: Option<Vec<CollectorSample>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub r#type: ActivityType,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub source: ActivitySource,
    pub actor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ActivityOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSession {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    pub message_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_message_preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: String,
    pub session_id: String,
    pub role: ChatMessageRole,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionRecord {
    pub trace_id: String,
    pub step_id: String,
    pub call_id: String,
    pub tool_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    pub environment: Environment,
    pub risk_level: RiskLevel,
    pub decision: PermissionMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
    pub status: ToolExecutionStatus,
    pub input: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serde, `as_str` and `from_db` are three encodings of one contract, and
    /// the IPC payload is produced by whichever one the call site happens to use.
    /// This asserts all three agree for *every* variant of every enum, which is
    /// the check that was missing when `ActivityType` serialised `FileChange` as
    /// `"filechange"` while the database and the shared TypeScript contract both
    /// said `"file_change"`.
    ///
    /// Driven by the same macro invocation that defines `as_str`, so a new
    /// variant cannot be added without being covered here.
    ///
    /// What each assertion still buys, now that `from_db` is generated from the
    /// same literal list as `as_str`:
    ///
    /// - the serde comparison remains an **independent** check. `serde`'s
    ///   `rename_all` rule is its own encoder; nothing generates it from the macro
    ///   list, so a multi-word variant can still drift — which is the original bug.
    /// - the `from_db` round-trip is no longer proof that the two directions agree
    ///   (the macro makes that structural). It is retained because it still catches
    ///   a **duplicate literal** in the invocation: two variants sharing a string
    ///   make the earlier pattern shadow the later one, so the shadowed variant does
    ///   not round-trip. Note this is now defence in depth rather than the sole
    ///   detector — a duplicated literal also breaks the serde comparison above,
    ///   since `rename_all` derives each variant's own spelling while `as_str` was
    ///   handed a shared one. Kept because it is one cheap assertion and states the
    ///   round-trip property directly.
    macro_rules! assert_wire_contract {
        ($ty:ident) => {
            for variant in $ty::ALL {
                let expected = variant.as_str();

                let serialised = serde_json::to_value(variant).unwrap_or_else(|error| {
                    panic!(
                        "{}::{} failed to serialise: {error}",
                        stringify!($ty),
                        expected
                    )
                });
                assert_eq!(
                    serialised,
                    serde_json::Value::String(expected.to_string()),
                    "{} serialises as {serialised} but as_str() says {expected:?}",
                    stringify!($ty),
                );

                assert_eq!(
                    $ty::from_db(expected),
                    Some(*variant),
                    "{}::from_db({expected:?}) does not round-trip",
                    stringify!($ty),
                );
            }
        };
    }

    #[test]
    fn every_enum_agrees_across_serde_as_str_and_from_db() {
        assert_wire_contract!(ServerStatus);
        assert_wire_contract!(Environment);
        assert_wire_contract!(HealthState);
        assert_wire_contract!(RiskLevel);
        assert_wire_contract!(PermissionMode);
        assert_wire_contract!(ActivityType);
        assert_wire_contract!(ActivitySource);
        assert_wire_contract!(ActivityOutcome);
        assert_wire_contract!(ChatMessageRole);
        assert_wire_contract!(ToolExecutionStatus);
        // `AiProviderKind` 是 `kebab-case`，不是上面那批 `lowercase`/`snake_case` 里的任何一个：
        // 它是唯一一个多词变体由连字符拼接的枚举，也正因为如此才需要这条断言。
        assert_wire_contract!(AiProviderKind);
    }

    /// The multi-word variants specifically: these are the ones a `lowercase`
    /// serde rule silently mangles, and the regression this guards against.
    #[test]
    fn multi_word_activity_types_use_snake_case() {
        assert_eq!(
            serde_json::to_value(ActivityType::FileChange).unwrap(),
            serde_json::json!("file_change"),
        );
        assert_eq!(
            serde_json::to_value(ActivityType::AgentAction).unwrap(),
            serde_json::json!("agent_action"),
        );
    }

    /// Single-word variants must not gain underscores from the `snake_case` rule.
    #[test]
    fn single_word_activity_types_stay_flat() {
        for variant in ActivityType::ALL {
            let wire = variant.as_str();
            if !wire.contains('_') {
                assert_eq!(
                    serde_json::to_value(variant).unwrap(),
                    serde_json::Value::String(wire.to_string()),
                );
            }
        }
    }

    fn add_server(authentication: serde_json::Value) -> Result<AddServerInput, String> {
        AddServerInput::from_value(&serde_json::json!({
            "name": "db",
            "host": "10.0.0.5",
            "username": "root",
            "environment": "staging",
            "authentication": authentication,
        }))
    }

    /// `authentication` 是 React ↔ Rust 的一处**联合类型契约**，而两端各自声明它
    /// （`packages/shared/src/schemas/server.ts` 的 `discriminatedUnion` 与这里的
    /// 内部标签枚举）。两边的拼写只能靠用例对上：TS 侧丢进来的就是下面这几个 JSON，
    /// 任何一个变体名或字段名漂了，命令层会在运行时才报「invalid add-server input」。
    ///
    /// 左边的形状就是共享 schema 产出的形状（见 `schemas/server.test.ts`）。
    #[test]
    fn authentication_input_accepts_the_shared_wire_shapes() {
        let parsed = add_server(serde_json::json!({ "method": "password", "password": "hunter2" }))
            .expect("password");
        assert!(matches!(
            parsed.authentication,
            AuthenticationInput::Password { password } if password == "hunter2"
        ));

        let parsed = add_server(serde_json::json!({
            "method": "privateKey",
            "privateKeyPem": "-----BEGIN OPENSSH PRIVATE KEY-----",
            "passphrase": "hunter2",
        }))
        .expect("encrypted private key");
        assert!(matches!(
            parsed.authentication,
            AuthenticationInput::PrivateKey { passphrase: Some(passphrase), .. } if passphrase == "hunter2"
        ));

        // 明文 key：`passphrase` **缺席**，不是空串。
        let parsed = add_server(serde_json::json!({
            "method": "privateKey",
            "privateKeyPem": "-----BEGIN OPENSSH PRIVATE KEY-----",
        }))
        .expect("plaintext private key");
        assert!(matches!(
            parsed.authentication,
            AuthenticationInput::PrivateKey {
                passphrase: None,
                ..
            }
        ));

        // ssh-agent：一个字段都没有的变体 —— 这正是 `z.strictObject({ method })` 的形状。
        let parsed = add_server(serde_json::json!({ "method": "agent" })).expect("agent");
        assert!(matches!(parsed.authentication, AuthenticationInput::Agent));

        let parsed = add_server(serde_json::json!({ "method": "identity", "identityId": "idn_1" }))
            .expect("identity");
        assert!(matches!(
            parsed.authentication,
            AuthenticationInput::Identity { identity_id } if identity_id == "idn_1"
        ));

        // 未知 method 必须失败，而不是落到某个默认变体上。
        assert!(add_server(serde_json::json!({ "method": "certificate" })).is_err());
    }

    /// `Identity` 的线形：`passphraseRef` 只在存在时出现，`agent` 身份的
    /// `credentialRef` 是空串（它没有凭据条目）。
    #[test]
    fn identity_serialises_the_passphrase_reference_only_when_present() {
        let encrypted = Identity {
            id: "idn_1".into(),
            label: "deploy key".into(),
            method: "privateKey".into(),
            credential_ref: "keychain://ssh/srv_1".into(),
            passphrase_ref: Some("keychain://ssh/srv_1-passphrase".into()),
            created_at: "2026-01-01T00:00:00.000Z".into(),
        };
        assert_eq!(
            serde_json::to_value(&encrypted).unwrap()["passphraseRef"],
            serde_json::json!("keychain://ssh/srv_1-passphrase"),
        );

        let agent = Identity {
            passphrase_ref: None,
            credential_ref: String::new(),
            method: "agent".into(),
            ..encrypted
        };
        let value = serde_json::to_value(&agent).unwrap();
        assert!(value.get("passphraseRef").is_none());
        assert_eq!(value["credentialRef"], serde_json::json!(""));
    }
}
