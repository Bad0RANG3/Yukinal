//! Shared process state owned by Rust only.
//!
//! Holds the database, OS credential store, local host-key trust store, C++ PTY
//! sessions, authorization ledgers, and process supervisors. Sidecar supervision
//! itself lives in `yukinal_core::supervisor`; nothing here is reachable from
//! React except through `commands`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use yukinal_core::mcp::McpSupervisor;
use yukinal_core::supervisor::Supervisor;
#[cfg(test)]
use yukinal_core::terminal::TerminalService;
use yukinal_credentials::os::OsCredentialStore;
use yukinal_database::Database;
use yukinal_ssh::RusshBackend;
use yukinal_terminal::TerminalManager;

mod approval;
mod auth;
pub(crate) mod credential_cleanup;
pub(crate) mod host_key;
mod oauth;

pub(crate) use approval::{
    is_host_approval_tool_name, HostActionExecutionBinding, HostActionTicketError,
    HostApprovalLedger,
};
pub use auth::AuthChallengeBroker;
pub use host_key::HostKeyProbeBroker;
pub use oauth::OAuthFlowBroker;

/// Bounded live responses for host action retries. The durable ledger lives in
/// SQLite; this cache only lets the same desktop process replay a completed
/// response without persisting raw remote output.
pub(crate) type HostToolReplayCache = Arc<Mutex<HashMap<(String, String), Value>>>;

pub struct AppState {
    /// Process-lifetime cancellation fence. Once Tauri asks the host to exit,
    /// every background loop and long-running command observes this token and
    /// stops admitting new work.
    pub shutdown: CancellationToken,
    pub supervisor: Supervisor,
    /// SQLite（servers / identities / provider_configs / tool_executions …）。
    pub database: Database,
    /// Host-side action responses keyed by `(traceId, callId)`. A missing cache
    /// entry is intentionally handled as a fail-closed duplicate, not a reason
    /// to execute the remote action again.
    pub(crate) host_tool_replays: HostToolReplayCache,
    /// Process-local, one-time host tickets created only after the user approves a
    /// concrete host action request. Automatic task grants remain in SQLite.
    pub(crate) host_action_approvals: HostApprovalLedger,
    /// OS Keychain / Credential Manager / Secret Service。
    pub credentials: Arc<OsCredentialStore>,
    /// Local OpenSSH known_hosts store used by the trust UI; remote SSH runs in C++.
    pub ssh: Arc<RusshBackend>,
    /// Test-only Rust SSH session manager used to cover removed compatibility paths.
    #[cfg(test)]
    pub terminals: TerminalService,
    /// C++-owned interactive SSH PTYs; all UI terminal session IDs route here.
    pub cpp_terminals: TerminalManager<crate::commands::ssh_service::SshTerminalPty>,
    /// One-shot SSH keyboard-interactive challenges and responses.
    pub auth: AuthChallengeBroker,
    /// One-shot, endpoint-bound host-key probes awaiting explicit user trust.
    pub host_keys: HostKeyProbeBroker,
    /// In-flight MCP OAuth authorizations, so the UI can stop one it started.
    pub oauth: OAuthFlowBroker,
    /// MCP 服务进程（ADR 0014）。与 sidecar 的 `Supervisor` 并列：sidecar 管**一个**进程，
    /// MCP 管**多个**，两者由同一个 crate 负责，谁派生进程这件事仍然只有 Rust。
    pub mcp: McpSupervisor,
}

impl AppState {
    /// 在 Tauri setup 中一次性装配 SQLite、加载本机 known_hosts、启动 C++ PTY 管理。
    pub fn bootstrap(data_dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(data_dir).map_err(|error| error.to_string())?;
        let database =
            Database::open(data_dir.join("yukinal.db")).map_err(|error| error.to_string())?;
        let ssh =
            Arc::new(RusshBackend::from_data_dir(data_dir).map_err(|error| error.to_string())?);
        #[cfg(test)]
        let terminals = TerminalService::new(Arc::clone(&ssh));
        let cpp_terminals = TerminalManager::with_id_prefix("tc");
        let credentials = Arc::new(OsCredentialStore);
        for failure in credential_cleanup::reconcile(&database, credentials.as_ref()) {
            tracing::warn!("credential cleanup reconciliation: {failure}");
        }
        Ok(Self {
            shutdown: CancellationToken::new(),
            supervisor: Supervisor::new(),
            database,
            host_tool_replays: Arc::new(Mutex::new(HashMap::new())),
            host_action_approvals: HostApprovalLedger::default(),
            credentials,
            ssh,
            #[cfg(test)]
            terminals,
            cpp_terminals,
            auth: AuthChallengeBroker::new(),
            host_keys: HostKeyProbeBroker::new(),
            // 空的：授权流程只在用户按下「连接 OAuth」时存在，装配阶段不派生轮询。
            oauth: OAuthFlowBroker::new(),
            // 空 supervisor：**不在这里**启动任何 MCP 服务器。启动第三方进程是用户按下的
            // 动作（`mcp_server_start`），或者第一次要目录时（`mcp::catalog`，只对从未启动
            // 过的服务器）。装配阶段派生进程会让「打开应用」变成一次副作用。
            mcp: McpSupervisor::new(),
        })
    }
}
