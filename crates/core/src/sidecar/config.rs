//! `SidecarConfig`：**启动什么**与**怎么启动**，以及从环境与目录树解析出它的那些规则。
//!
//! 从 `sidecar.rs` 拆出来，因为这一段与「跟子进程说话」无关：它全是纯逻辑（读环境变量、
//! 在祖先目录里找 dev bundle、拼 node 程序名），没有 IO、不碰 tokio，也不依赖 `SidecarHandle`。
//! 单独成文件之后，判断「路径是怎么定下来的」不必再翻过 spawn 与 JSON-RPC 那五百行。

use super::*;

/// What to launch and how. Kept as data (not a magic env read inside the spawn path)
/// so tests can point it at anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarConfig {
    /// Executable, normally `node`.
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(String, String)>,
    pub request_timeout: Duration,
    /// Path reported back to the UI, so "what actually started" is visible.
    pub entry_label: String,
    /// Desktop version sent during `initialize` (audit + capability negotiation).
    pub client_version: String,
    /// Handed to the sidecar so it can find its local spool. Never a secret and never
    /// a credential.
    pub data_dir: String,
}

impl SidecarConfig {
    /// Resolution order (ADR 0008):
    /// 1. `YUKINAL_AGENT_COMMAND` (+ optional `YUKINAL_AGENT_ARGS`, `;`-separated)
    /// 2. `YUKINAL_AGENT_ENTRY` (+ optional `YUKINAL_NODE`)
    /// 3. dev fallback: nearest `apps/agent/dist/index.js` walking up from `cwd`
    ///
    /// Never a silent default: if nothing resolves, the caller gets a message naming
    /// the build step to run.
    pub fn from_env_with_cwd(cwd: &Path) -> Result<Self, SidecarError> {
        let lookup = |key: &str| {
            std::env::var(key)
                .ok()
                .filter(|value| !value.trim().is_empty())
        };

        let request_timeout = Duration::from_secs(
            lookup("YUKINAL_AGENT_TIMEOUT_SECS")
                .and_then(|raw| raw.parse::<u64>().ok())
                .unwrap_or(10),
        );

        if let Some(command) = lookup("YUKINAL_AGENT_COMMAND") {
            let args = match lookup("YUKINAL_AGENT_ARGS") {
                Some(raw) => raw.split(';').map(OsString::from).collect(),
                None => Vec::new(),
            };
            return Ok(Self {
                program: PathBuf::from(command),
                args,
                env: Vec::new(),
                request_timeout,
                entry_label: String::from("custom command"),
                client_version: default_client_version(),
                data_dir: lookup("YUKINAL_DATA_DIR").unwrap_or_default(),
            });
        }

        if let Some(entry) = lookup("YUKINAL_AGENT_ENTRY") {
            let path = PathBuf::from(&entry);
            if !path.is_file() {
                return Err(SidecarError::NotFound {
                    searched: path.display().to_string(),
                });
            }
            return Ok(Self {
                program: node_program(lookup("YUKINAL_NODE").as_deref()),
                args: vec![path.into_os_string()],
                env: Vec::new(),
                request_timeout,
                entry_label: entry,
                client_version: default_client_version(),
                data_dir: lookup("YUKINAL_DATA_DIR").unwrap_or_default(),
            });
        }

        match find_dev_bundle(cwd) {
            Some(path) => Ok(Self {
                program: node_program(None),
                args: vec![path.clone().into_os_string()],
                env: Vec::new(),
                request_timeout,
                entry_label: path.display().to_string(),
                client_version: default_client_version(),
                data_dir: lookup("YUKINAL_DATA_DIR").unwrap_or_default(),
            }),
            None => Err(SidecarError::NotFound {
                searched: ancestors(cwd)
                    .map(|dir| format!("{}/apps/agent/dist/index.js", dir.display()))
                    .collect::<Vec<_>>()
                    .join(", "),
            }),
        }
    }

    #[must_use]
    pub fn with_env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_string(), value.to_string()));
        self
    }
}

fn default_client_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn node_program(override_path: Option<&str>) -> PathBuf {
    match override_path {
        Some(explicit) if !explicit.trim().is_empty() => PathBuf::from(explicit),
        _ => PathBuf::from(if cfg!(windows) { "node.exe" } else { "node" }),
    }
}

fn find_dev_bundle(cwd: &Path) -> Option<PathBuf> {
    ancestors(cwd)
        .map(|dir| dir.join("apps").join("agent").join("dist").join("index.js"))
        .find(|candidate| candidate.is_file())
}

fn ancestors(start: &Path) -> impl Iterator<Item = PathBuf> {
    let mut current = Some(start.to_path_buf());
    std::iter::from_fn(move || {
        let path = current.take()?;
        let parent = path.parent().map(Path::to_path_buf);
        current = parent;
        Some(if path.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            path
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_tagged_dir(tag: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let unique = format!("yukinal-sidecar-test-{tag}-{}", std::process::id());
        path.push(unique);
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn finds_the_dev_bundle_in_an_ancestor() {
        let root = temp_tagged_dir("bundle");
        let bundle = root.join("apps").join("agent").join("dist");
        std::fs::create_dir_all(&bundle).expect("create bundle dir");
        std::fs::write(bundle.join("index.js"), "console.log('x')").expect("write bundle");

        let nested = root.join("apps").join("desktop").join("src-tauri");
        std::fs::create_dir_all(&nested).expect("create nested dir");

        let found = find_dev_bundle(&nested);
        assert_eq!(found, Some(bundle.join("index.js")));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn missing_bundle_error_names_the_fix() {
        let root = temp_tagged_dir("empty");
        let error =
            SidecarConfig::from_env_with_cwd(&root).expect_err("should fail when nothing resolves");
        let message = error.to_string();
        assert!(
            message.contains("pnpm --filter @yukinal/agent build"),
            "{message}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn explicit_command_override_wins_and_splits_args_on_semicolon() {
        // Guarded: these env vars are process-global, so this test owns them.
        std::env::set_var("YUKINAL_AGENT_COMMAND", "/usr/bin/true");
        std::env::set_var("YUKINAL_AGENT_ARGS", "one;two with space");
        let config = SidecarConfig::from_env_with_cwd(Path::new(".")).expect("explicit config");
        assert_eq!(config.program, PathBuf::from("/usr/bin/true"));
        assert_eq!(config.args.len(), 2);
        assert_eq!(config.args[1], OsString::from("two with space"));
        std::env::remove_var("YUKINAL_AGENT_COMMAND");
        std::env::remove_var("YUKINAL_AGENT_ARGS");
    }
}
