//! Repository layer: the only code that reads/writes rows.
//!
//! Every repository is a thin view over `Database` with one responsibility, so the
//! audit-relevant writes (tool executions, activities) sit on exactly one path.

mod activities;
mod executions;
mod identities;
mod providers;
mod servers;
mod snapshots;
mod workspaces;

pub use activities::ActivitiesRepository;
pub use executions::ToolExecutionsRepository;
pub use identities::IdentitiesRepository;
pub use providers::{McpServersRepository, ProviderConfigsRepository};
pub use servers::ServersRepository;
pub use snapshots::SnapshotsRepository;
pub use workspaces::WorkspacesRepository;
