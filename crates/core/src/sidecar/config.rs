//! Resolve the Python Agent process and the environment passed to it.
//!
//! The desktop runtime launches the frozen Python Agent in packaged builds and the
//! repository virtual environment during development. A custom executable remains
//! available for diagnostics and integration harnesses; there is no implicit Node route.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarConfig {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(String, String)>,
    pub request_timeout: Duration,
    pub entry_label: String,
    pub client_version: String,
    pub data_dir: String,
    /// Whether this process is the Python Agent and may receive its C++ service settings.
    pub is_python_agent: bool,
}

impl SidecarConfig {
    pub fn from_env_with_cwd(cwd: &Path) -> Result<Self, SidecarError> {
        Self::from_env_with_resources(cwd, None)
    }

    /// Resolution order: explicit command, packaged Python Agent, then the repository's
    /// `agent/.venv`. Installed applications cannot discover source-tree runtimes; dev
    /// launches use `dev-python-agent.mjs`, which supplies the explicit command.
    pub fn from_env_with_resources(
        cwd: &Path,
        resources: Option<&Path>,
    ) -> Result<Self, SidecarError> {
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
        let data_dir = lookup("YUKINAL_DATA_DIR").unwrap_or_default();
        let client_version = default_client_version();

        if let Some(command) = lookup("YUKINAL_AGENT_COMMAND") {
            let args: Vec<OsString> = lookup("YUKINAL_AGENT_ARGS")
                .map(|raw| raw.split(';').map(OsString::from).collect())
                .unwrap_or_default();
            let is_python_agent = is_python_module_invocation(&args);
            return Ok(Self {
                program: PathBuf::from(command),
                args,
                env: Vec::new(),
                request_timeout,
                entry_label: String::from("custom command"),
                client_version,
                data_dir,
                is_python_agent,
            });
        }

        if let Some(path) = resources
            .map(packaged_python_entry)
            .filter(|candidate| candidate.is_file())
        {
            let executable = for_command_line(path);
            return Ok(Self {
                program: executable.clone(),
                args: Vec::new(),
                env: Vec::new(),
                request_timeout,
                entry_label: executable.display().to_string(),
                client_version,
                data_dir,
                is_python_agent: true,
            });
        }

        if let Some(path) = find_dev_python(cwd) {
            let executable = for_command_line(path);
            return Ok(Self {
                program: executable.clone(),
                args: vec![
                    OsString::from("-m"),
                    OsString::from("yukinal_agent.sidecar"),
                ],
                env: Vec::new(),
                request_timeout,
                entry_label: executable.display().to_string(),
                client_version,
                data_dir,
                is_python_agent: true,
            });
        }

        let mut searched = Vec::new();
        if let Some(resources) = resources {
            searched.push(packaged_python_entry(resources).display().to_string());
        }
        searched.extend(ancestors(cwd).map(|dir| dev_python_entry(&dir).display().to_string()));
        Err(SidecarError::NotFound {
            searched: searched.join(", "),
        })
    }

    #[must_use]
    pub fn with_env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_string(), value.to_string()));
        self
    }

    /// Pass C++ service credentials only to the Python Agent process.
    #[must_use]
    pub fn with_python_ssh_service_env(
        mut self,
        token: Option<&str>,
        base_url: Option<&str>,
    ) -> Self {
        if self.is_python_agent {
            if let Some(token) = token.filter(|value| !value.is_empty()) {
                self = self.with_env("YUKINAL_SSH_API_TOKEN", token);
            }
            if let Some(base_url) = base_url.filter(|value| !value.is_empty()) {
                self = self.with_env("YUKINAL_SSH_API_URL", base_url);
            }
        }
        self
    }

    /// Pass the packaged C++ service and the exact config credential references to Python.
    #[must_use]
    pub fn with_python_ssh_service_process(
        mut self,
        binary: &Path,
        config: &Path,
        referenced_environment: &[(String, String)],
    ) -> Self {
        if self.is_python_agent {
            self = self.with_env("YUKINAL_SSH_SERVICE_BINARY", &binary.display().to_string());
            self = self.with_env("YUKINAL_SSH_SERVICE_CONFIG", &config.display().to_string());
            for (name, value) in referenced_environment {
                self = self.with_env(name, value);
            }
        }
        self
    }

    /// Enable per-operation target resolution through the desktop host RPC.
    #[must_use]
    pub fn with_python_ssh_service_autoconfig(mut self, binary: &Path) -> Self {
        if self.is_python_agent {
            self = self.with_env("YUKINAL_SSH_SERVICE_BINARY", &binary.display().to_string());
            self = self.with_env("YUKINAL_SSH_SERVICE_AUTOCONFIG", "1");
        }
        self
    }

    #[must_use]
    pub fn resolved_through_path(&self) -> bool {
        self.program.components().count() == 1
    }

    #[must_use]
    pub fn launch_error(&self, error: &std::io::Error) -> SidecarError {
        if error.kind() == std::io::ErrorKind::NotFound && self.resolved_through_path() {
            return SidecarError::Launch(format!(
                "Python Agent command `{}` was not found on PATH; check the configured Python environment or set YUKINAL_AGENT_COMMAND to an executable path",
                self.program.display()
            ));
        }
        SidecarError::Launch(format!("{}: {error}", self.program.display()))
    }
}

fn default_client_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn is_python_module_invocation(args: &[OsString]) -> bool {
    args.windows(2).any(|arguments| {
        arguments[0] == std::ffi::OsStr::new("-m")
            && arguments[1] == std::ffi::OsStr::new("yukinal_agent.sidecar")
    })
}

fn dev_python_entry(repository: &Path) -> PathBuf {
    let mut path = repository.join("agent").join(".venv");
    if cfg!(windows) {
        path.push("Scripts");
        path.push("python.exe");
    } else {
        path.push("bin");
        path.push("python");
    }
    path
}

fn find_dev_python(cwd: &Path) -> Option<PathBuf> {
    ancestors(cwd)
        .map(|directory| dev_python_entry(&directory))
        .find(|candidate| candidate.is_file())
}

/// Convert Windows verbatim disk and UNC paths to equivalent ordinary paths before
/// passing them to child processes. Volume GUID paths have no plain equivalent.
#[must_use]
pub fn for_command_line(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    if let Some(plain) = plain_windows_path(&path) {
        return plain;
    }
    path
}

#[cfg(windows)]
fn plain_windows_path(path: &Path) -> Option<PathBuf> {
    use std::path::{Component, Prefix};

    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return None;
    };
    let rest: PathBuf = components.collect();
    match prefix.kind() {
        Prefix::VerbatimDisk(letter) => {
            let mut plain = PathBuf::from(format!("{}:\\", char::from(letter)));
            plain.push(rest);
            Some(plain)
        }
        Prefix::VerbatimUNC(server, share) => {
            let mut plain = PathBuf::from(r"\\");
            plain.push(server);
            plain.push(share);
            plain.push(rest);
            Some(plain)
        }
        _ => None,
    }
}

/// Frozen Python Agent path inside Tauri's resource directory.
#[must_use]
pub fn packaged_python_entry(resources: &Path) -> PathBuf {
    resources
        .join("python-agent")
        .join("yukinal-agent")
        .join(if cfg!(windows) {
            "yukinal-agent.exe"
        } else {
            "yukinal-agent"
        })
}

/// Standalone C++ SSH service executable inside Tauri's resource directory.
#[must_use]
pub fn packaged_ssh_service_entry(resources: &Path) -> PathBuf {
    resources.join("ssh-service").join(if cfg!(windows) {
        "yukinal-ssh-service.exe"
    } else {
        "yukinal-ssh-service"
    })
}

fn ancestors(start: &Path) -> impl Iterator<Item = PathBuf> {
    let mut current = Some(start.to_path_buf());
    std::iter::from_fn(move || {
        let path = current.take()?;
        current = path.parent().map(Path::to_path_buf);
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

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn clear_launch_env() {
        for key in [
            "YUKINAL_AGENT_COMMAND",
            "YUKINAL_AGENT_ARGS",
            "YUKINAL_AGENT_TIMEOUT_SECS",
            "YUKINAL_DATA_DIR",
        ] {
            std::env::remove_var(key);
        }
    }

    fn temp_tagged_dir(tag: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("yukinal-sidecar-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn packaged_python_agent_wins_over_a_development_environment() {
        let _guard = env_guard();
        clear_launch_env();
        let root = temp_tagged_dir("packaged-python");
        let resources = root.join("resources");
        let packaged = packaged_python_entry(&resources);
        let dev_python = dev_python_entry(&root);
        std::fs::create_dir_all(packaged.parent().expect("packaged parent")).unwrap();
        std::fs::create_dir_all(dev_python.parent().expect("venv parent")).unwrap();
        std::fs::write(&packaged, "frozen python fixture").unwrap();
        std::fs::write(&dev_python, "python fixture").unwrap();

        let config = SidecarConfig::from_env_with_resources(&root, Some(&resources))
            .expect("packaged Python Agent resolves");
        assert_eq!(config.program, packaged);
        assert!(config.args.is_empty());
        assert!(config.is_python_agent);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn development_resolves_the_repository_virtual_environment() {
        let _guard = env_guard();
        clear_launch_env();
        let root = temp_tagged_dir("dev-python");
        let python = dev_python_entry(&root);
        std::fs::create_dir_all(python.parent().expect("venv parent")).unwrap();
        std::fs::write(&python, "python fixture").unwrap();
        let nested = root.join("apps").join("desktop").join("src-tauri");
        std::fs::create_dir_all(&nested).unwrap();

        let config = SidecarConfig::from_env_with_cwd(&nested).expect("dev Python Agent resolves");
        assert_eq!(config.program, python);
        assert_eq!(
            config.args,
            [
                OsString::from("-m"),
                OsString::from("yukinal_agent.sidecar")
            ]
        );
        assert!(config.is_python_agent);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_python_command_wins_and_splits_arguments() {
        let _guard = env_guard();
        clear_launch_env();
        std::env::set_var("YUKINAL_AGENT_COMMAND", "python-test");
        std::env::set_var(
            "YUKINAL_AGENT_ARGS",
            "-m;yukinal_agent.sidecar;two with space",
        );
        std::env::set_var("YUKINAL_DATA_DIR", "agent-data");
        let config =
            SidecarConfig::from_env_with_cwd(Path::new(".")).expect("explicit command resolves");
        assert_eq!(config.program, PathBuf::from("python-test"));
        assert_eq!(config.args.len(), 3);
        assert_eq!(config.args[2], OsString::from("two with space"));
        assert_eq!(config.data_dir, "agent-data");
        assert!(config.is_python_agent);
        clear_launch_env();
    }

    #[test]
    fn cxx_credentials_are_not_forwarded_to_a_non_python_command() {
        let _guard = env_guard();
        let custom = SidecarConfig {
            program: PathBuf::from("custom-agent"),
            args: Vec::new(),
            env: Vec::new(),
            request_timeout: Duration::from_secs(10),
            entry_label: "custom command".into(),
            client_version: "test".into(),
            data_dir: String::new(),
            is_python_agent: false,
        }
        .with_python_ssh_service_env(Some("secret-token"), Some("http://127.0.0.1:8765"));
        assert!(custom.env.is_empty());

        let python = SidecarConfig {
            is_python_agent: true,
            ..custom
        }
        .with_python_ssh_service_env(Some("secret-token"), Some("http://127.0.0.1:8765"));
        assert_eq!(python.env.len(), 2);
        assert_eq!(python.env[0].0, "YUKINAL_SSH_API_TOKEN");
    }

    #[test]
    fn not_found_error_lists_python_paths_and_setup_command() {
        let _guard = env_guard();
        clear_launch_env();
        let root = temp_tagged_dir("missing-python");
        let resources = root.join("resources");
        let error = SidecarConfig::from_env_with_resources(&root, Some(&resources))
            .expect_err("missing Python runtime must fail");
        let message = error.to_string();
        assert!(message.contains(&packaged_python_entry(&resources).display().to_string()));
        assert!(message.contains(&dev_python_entry(&root).display().to_string()));
        assert!(message.contains("agent[agent,models]"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn verbatim_paths_are_normalized_before_launch() {
        assert_eq!(
            for_command_line(PathBuf::from(r"\\?\C:\Users\me\python.exe")),
            PathBuf::from(r"C:\Users\me\python.exe")
        );
        assert_eq!(
            for_command_line(PathBuf::from(r"\\?\UNC\server\share\python.exe")),
            PathBuf::from(r"\\server\share\python.exe")
        );
        let volume = PathBuf::from(r"\\?\Volume{123}\python.exe");
        assert_eq!(for_command_line(volume.clone()), volume);
    }

    #[test]
    fn packaged_ssh_service_entry_uses_the_platform_executable_name() {
        let path = packaged_ssh_service_entry(Path::new("resources"));
        assert!(path.starts_with("resources/ssh-service"));
        assert!(path.to_string_lossy().contains("yukinal-ssh-service"));
    }
}
