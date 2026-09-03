//! yukinal-database — SQLite 本地存储。
//!
//! 表：servers / groups / workspaces / identities / server_identities / snapshots
//! / services / activities / chat_sessions / chat_messages / tool_executions
//! / provider_configs / mcp_servers
//!
//! 约束：
//! - 只存 `credential_ref`，不存 secret 本体。
//! - offline-first：无 Cloud 也必须完整可用。
//! - 所有写操作与 activities / tool_executions 一起构成审计链。
//!
//! 当前为契约占位。
