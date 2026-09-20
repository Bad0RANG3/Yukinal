//! Repository layer: the only code that reads/writes rows.
//!
//! Every repository is a thin view over `Database` with one responsibility, so the
//! audit-relevant writes (tool executions, activities) sit on exactly one path.

mod activities;
mod chat;
mod credential_cleanup;
mod decode;
mod executions;
mod filesystem_backups;
mod host_tool_calls;
mod identities;
mod investigations;
mod providers;
mod retention;
mod servers;
mod settings;
mod snapshots;
mod workspaces;

pub use activities::ActivitiesRepository;
pub use chat::ChatRepository;
pub use credential_cleanup::CredentialCleanupRepository;
pub use executions::ToolExecutionsRepository;
pub use filesystem_backups::{
    FilesystemBackupRecord, FilesystemBackupStatus, FilesystemBackupsRepository,
};
pub use host_tool_calls::{
    HostToolCallClaim, HostToolCallInput, HostToolCallStatus, HostToolCallsRepository,
};
pub use identities::IdentitiesRepository;
pub use investigations::{EvidenceSearchQuery, InvestigationsRepository, TaskProgressUpdate};
pub use providers::{McpServersRepository, ProviderConfigsRepository};
pub use retention::{
    InvestigationRetentionItem, InvestigationRetentionKind, InvestigationRetentionPreview,
    InvestigationRetentionPruneResult, InvestigationRetentionRepository,
    InvestigationRetentionRequestItem, InvestigationRetentionSkip,
};
pub use servers::ServersRepository;
pub use settings::AppSettingsRepository;
pub use snapshots::SnapshotsRepository;
pub use workspaces::WorkspacesRepository;
