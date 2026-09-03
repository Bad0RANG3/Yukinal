//! Shared process state owned by Rust only.
//!
//! Grows in order: the SQLite pool, a credential store handle, `SshManager`,
//! `PtyManager`, then the collector scheduler. Sidecar supervision itself lives in
//! `yukinal_core::supervisor`; this struct only holds the instances so commands can
//! reach them. Nothing here is reachable from React except through `commands`.

use std::path::Path;
use std::sync::Arc;

use yukinal_core::supervisor::Supervisor;
use yukinal_core::terminal::TerminalService;
use yukinal_credentials::os::OsCredentialStore;
use yukinal_database::Database;
use yukinal_ssh::RusshBackend;

pub struct AppState {
    pub supervisor: Supervisor,
    /// SQLite（servers / identities / provider_configs / tool_executions …）。
    pub database: Database,
    /// OS Keychain / Credential Manager / Secret Service。
    pub credentials: Arc<OsCredentialStore>,
    pub ssh: Arc<RusshBackend>,
    /// PTY Manager（terminal_open/write/resize/close + 事件广播）。
    pub terminals: TerminalService,
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
        })
    }
}
