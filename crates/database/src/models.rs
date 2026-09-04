//! Row types mirroring `@yukinal/shared` (same names, same camelCase serialisation).
//!
//! These are the *wire shapes*: `serde_json::to_value(server_row)` produces exactly
//! what the IPC contract expects, so the command layer can hand rows to the UI
//! without a second translation. Enum string values are the dot-free literals from
//! the shared package; serde rejects anything else, so a typo fails at load time,
//! not at display time.

use serde::{Deserialize, Serialize};

/// Wire-string for enum columns without allocating JSON just to unquote it.
macro_rules! enum_as_str {
    ($ty:ident, $($variant:ident => $str:literal),+ $(,)?) => {
        impl $ty {
            #[must_use]
            pub fn as_str(&self) -> &'static str {
                match self {
                    $(Self::$variant => $str),+
                }
            }
        }
    };
}

enum_as_str!(ServerStatus, Connecting => "connecting", Connected => "connected", Disconnected => "disconnected", Error => "error");
enum_as_str!(Environment, Local => "local", Development => "development", Staging => "staging", Production => "production", Unknown => "unknown");

impl ServerStatus {
    #[must_use]
    pub fn from_db(raw: &str) -> Option<Self> {
        match raw {
            "connecting" => Some(Self::Connecting),
            "connected" => Some(Self::Connected),
            "disconnected" => Some(Self::Disconnected),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

impl Environment {
    #[must_use]
    pub fn from_db(raw: &str) -> Option<Self> {
        match raw {
            "local" => Some(Self::Local),
            "development" => Some(Self::Development),
            "staging" => Some(Self::Staging),
            "production" => Some(Self::Production),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

impl RiskLevel {
    #[must_use]
    pub fn from_db(raw: &str) -> Option<Self> {
        match raw {
            "read" => Some(Self::Read),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "critical" => Some(Self::Critical),
            _ => None,
        }
    }
}

impl PermissionMode {
    #[must_use]
    pub fn from_db(raw: &str) -> Option<Self> {
        match raw {
            "auto" => Some(Self::Auto),
            "ask" => Some(Self::Ask),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }
}

impl ToolExecutionStatus {
    #[must_use]
    pub fn from_db(raw: &str) -> Option<Self> {
        match raw {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "waiting_approval" => Some(Self::WaitingApproval),
            "success" => Some(Self::Success),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}
enum_as_str!(HealthState, Healthy => "healthy", Warning => "warning", Critical => "critical", Unknown => "unknown");
enum_as_str!(ActivityType, Connection => "connection", Authentication => "authentication", Configuration => "configuration", Deployment => "deployment", Service => "service", Container => "container", FileChange => "file_change", AgentAction => "agent_action", Approval => "approval", Health => "health");
enum_as_str!(ActivitySource, Agent => "agent", User => "user", System => "system", Docker => "docker", Git => "git", Cloud => "cloud");
enum_as_str!(ActivityOutcome, Success => "success", Failure => "failure", Cancelled => "cancelled", Denied => "denied");
enum_as_str!(ToolExecutionStatus, Pending => "pending", Running => "running", WaitingApproval => "waiting_approval", Success => "success", Failed => "failed", Cancelled => "cancelled");
enum_as_str!(PermissionMode, Auto => "auto", Ask => "ask", Deny => "deny");
enum_as_str!(RiskLevel, Read => "read", Low => "low", Medium => "medium", High => "high", Critical => "critical");

impl ActivityType {
    #[must_use]
    pub fn from_db(raw: &str) -> Option<Self> {
        match raw {
            "connection" => Some(Self::Connection),
            "authentication" => Some(Self::Authentication),
            "configuration" => Some(Self::Configuration),
            "deployment" => Some(Self::Deployment),
            "service" => Some(Self::Service),
            "container" => Some(Self::Container),
            "file_change" => Some(Self::FileChange),
            "agent_action" => Some(Self::AgentAction),
            "approval" => Some(Self::Approval),
            "health" => Some(Self::Health),
            _ => None,
        }
    }
}

impl ActivitySource {
    #[must_use]
    pub fn from_db(raw: &str) -> Option<Self> {
        match raw {
            "agent" => Some(Self::Agent),
            "user" => Some(Self::User),
            "system" => Some(Self::System),
            "docker" => Some(Self::Docker),
            "git" => Some(Self::Git),
            "cloud" => Some(Self::Cloud),
            _ => None,
        }
    }
}

impl ActivityOutcome {
    #[must_use]
    pub fn from_db(raw: &str) -> Option<Self> {
        match raw {
            "success" => Some(Self::Success),
            "failure" => Some(Self::Failure),
            "cancelled" => Some(Self::Cancelled),
            "denied" => Some(Self::Denied),
            _ => None,
        }
    }
}

impl HealthState {
    #[must_use]
    pub fn from_db(raw: &str) -> Option<Self> {
        match raw {
            "healthy" => Some(Self::Healthy),
            "warning" => Some(Self::Warning),
            "critical" => Some(Self::Critical),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
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
#[serde(rename_all = "lowercase")]
pub enum ToolExecutionStatus {
    Pending,
    Running,
    WaitingApproval,
    Success,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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

fn default_wire_api() -> String {
    "chat".to_string()
}

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
    pub credential_ref: String,
    pub created_at: String,
}

// ---------------------------------------------------------------------------
// provider configs (AI + infrastructure) and MCP servers

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AiProviderKind {
    OpenaiCompatible,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProviderConfig {
    pub id: String,
    pub kind: AiProviderKind,
    pub label: String,
    pub base_url: String,
    pub model: String,
    /// "chat" | "responses" — codex 中转的 responses API 由 CC Switch 导入决定。
    #[serde(default = "default_wire_api")]
    pub wire_api: String,
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

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "method", rename_all = "camelCase")]
pub enum AuthenticationInput {
    Password {
        password: String,
    },
    PrivateKey {
        private_key_pem: String,
        passphrase: Option<String>,
    },
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
