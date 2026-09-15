//! servers / groups / workspaces / identities rows.

use serde::{Deserialize, Serialize};

enum_as_str!(ServerStatus, Connecting => "connecting", Connected => "connected", Disconnected => "disconnected", Error => "error");
enum_as_str!(Environment, Local => "local", Development => "development", Staging => "staging", Production => "production", Unknown => "unknown");
enum_as_str!(HealthState, Healthy => "healthy", Warning => "warning", Critical => "critical", Unknown => "unknown");

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostCertificateAuthority {
    /// OpenSSH public-key line for the CA that signs host certificates.
    pub ca_public_key: String,
    /// Host principals accepted from otherwise-valid certificates.
    pub principals: Vec<String>,
    /// Optional local OpenSSH KRL checked after CA, time and principal validation.
    pub revocation_list_path: Option<String>,
    /// Optional HTTPS OpenSSH KRL URL; mutually exclusive with the local path.
    pub revocation_list_url: Option<String>,
    /// Public keys trusted to sign the KRL independently of the host CA.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub revocation_list_signers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConnection {
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_certificate_authority: Option<HostCertificateAuthority>,
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
    /// Path to the OpenSSH user certificate (`*-cert.pub`). Public material.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate_path: Option<String>,
    /// Path of the private-key file. This is metadata, never the key bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private_key_path: Option<String>,
    pub created_at: String,
}
