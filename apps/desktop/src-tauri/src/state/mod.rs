//! Shared process state owned by Rust only.
//!
//! Grows in order: the SQLite pool, a credential store handle, `SshManager`,
//! `PtyManager`, then the collector scheduler. Sidecar supervision itself lives in
//! `yukinal_core::supervisor`; this struct only holds the instances so commands can
//! reach them. Nothing here is reachable from React except through `commands`.

use yukinal_core::supervisor::Supervisor;

#[derive(Debug, Default)]
pub struct AppState {
    pub supervisor: Supervisor,
}

impl AppState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            supervisor: Supervisor::new(),
        }
    }
}
