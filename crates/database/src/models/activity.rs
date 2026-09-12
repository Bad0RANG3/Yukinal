//! activity rows and their typed columns.

use serde::{Deserialize, Serialize};

enum_as_str!(ActivityType, Connection => "connection", Authentication => "authentication", Configuration => "configuration", Deployment => "deployment", Service => "service", Container => "container", FileChange => "file_change", AgentAction => "agent_action", Approval => "approval", Health => "health");
enum_as_str!(ActivitySource, Agent => "agent", User => "user", System => "system", Docker => "docker", Git => "git", Cloud => "cloud");
enum_as_str!(ActivityOutcome, Success => "success", Failure => "failure", Cancelled => "cancelled", Denied => "denied");

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
