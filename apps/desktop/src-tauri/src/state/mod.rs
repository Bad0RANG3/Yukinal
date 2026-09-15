//! Shared process state owned by Rust only.
//!
//! Grows in order: the SQLite pool, a credential store handle, `SshManager`,
//! `PtyManager`, then the collector scheduler. Sidecar supervision itself lives in
//! `yukinal_core::supervisor`; this struct only holds the instances so commands can
//! reach them. Nothing here is reachable from React except through `commands`.

use std::path::Path;
use std::sync::Arc;

use yukinal_core::mcp::McpSupervisor;
use yukinal_core::supervisor::Supervisor;
use yukinal_core::terminal::TerminalService;
use yukinal_credentials::os::OsCredentialStore;
use yukinal_database::Database;
use yukinal_ssh::RusshBackend;

mod auth;
mod oauth;

pub use auth::AuthChallengeBroker;
pub use oauth::OAuthFlowBroker;

pub struct AppState {
    pub supervisor: Supervisor,
    /// SQLite（servers / identities / provider_configs / tool_executions …）。
    pub database: Database,
    /// OS Keychain / Credential Manager / Secret Service。
    pub credentials: Arc<OsCredentialStore>,
    pub ssh: Arc<RusshBackend>,
    /// PTY Manager（terminal_open/write/resize/close + 事件广播）。
    pub terminals: TerminalService,
    /// One-shot SSH keyboard-interactive challenges and responses.
    pub auth: AuthChallengeBroker,
    /// In-flight MCP OAuth authorizations, so the UI can stop one it started.
    pub oauth: OAuthFlowBroker,
    /// MCP 服务进程（ADR 0014）。与 sidecar 的 `Supervisor` 并列：sidecar 管**一个**进程，
    /// MCP 管**多个**，两者由同一个 crate 负责，谁派生进程这件事仍然只有 Rust。
    pub mcp: McpSupervisor,
}

impl AppState {
    /// 在 Tauri setup 中一次性装配：数据目录下开 SQLite、加载 known_hosts、
    /// 挂上 ssh 后端与终端服务。
    pub fn bootstrap(data_dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(data_dir).map_err(|error| error.to_string())?;
        let database =
            Database::open(data_dir.join("yukinal.db")).map_err(|error| error.to_string())?;
        let ssh =
            Arc::new(RusshBackend::from_data_dir(data_dir).map_err(|error| error.to_string())?);
        let terminals = TerminalService::new(Arc::clone(&ssh));
        Ok(Self {
            supervisor: Supervisor::new(),
            database,
            credentials: Arc::new(OsCredentialStore),
            ssh,
            terminals,
            auth: AuthChallengeBroker::new(),
            // 空的：授权流程只在用户按下「连接 OAuth」时存在，装配阶段不派生轮询。
            oauth: OAuthFlowBroker::new(),
            // 空 supervisor：**不在这里**启动任何 MCP 服务器。启动第三方进程是用户按下的
            // 动作（`mcp_server_start`），或者第一次要目录时（`mcp::catalog`，只对从未启动
            // 过的服务器）。装配阶段派生进程会让「打开应用」变成一次副作用。
            mcp: McpSupervisor::new(),
        })
    }
}
