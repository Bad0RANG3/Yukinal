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
    ///
    /// No packaged resources: see [`SidecarConfig::from_env_with_resources`] for the
    /// installer case. This entry point stays because every test and the dev loop use it.
    pub fn from_env_with_cwd(cwd: &Path) -> Result<Self, SidecarError> {
        Self::from_env_with_resources(cwd, None)
    }

    /// Resolution order for a **packaged** app:
    /// 1. `YUKINAL_AGENT_COMMAND` (+ optional `YUKINAL_AGENT_ARGS`, `;`-separated)
    /// 2. `YUKINAL_AGENT_ENTRY` (+ optional `YUKINAL_NODE`)
    /// 3. `<resources>/agent/index.js` — where `tauri.conf.json` puts the bundle
    /// 4. dev fallback: nearest `apps/agent/dist/index.js` walking up from `cwd`
    ///
    /// The packaged path outranks the dev one on purpose: an installed app has no repo
    /// checkout to walk up from, while a dev run has no staged resources, so each order
    /// picks the right answer in the case that matters and neither can shadow the other.
    ///
    /// `resources` is the Tauri resource directory. It is a parameter rather than an
    /// `env!`-style constant because only the caller knows where it is, and the crate must
    /// stay launchable from tests.
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
            // 标签跟着命令行走，而不是照抄环境变量：见下面 packaged 分支的注释。
            let entry = for_command_line(path);
            return Ok(Self {
                program: node_program(lookup("YUKINAL_NODE").as_deref()),
                args: vec![entry.clone().into_os_string()],
                env: Vec::new(),
                request_timeout,
                entry_label: entry.display().to_string(),
                client_version: default_client_version(),
                data_dir: lookup("YUKINAL_DATA_DIR").unwrap_or_default(),
            });
        }

        let packaged = resources.map(packaged_entry);
        let found = packaged
            .as_ref()
            .filter(|candidate| candidate.is_file())
            .cloned()
            .or_else(|| find_dev_bundle(cwd));

        match found {
            Some(path) => {
                // 交给 Node 的路径与报给界面的路径必须是**同一条**。
                //
                // `resource_dir()` 在 Windows 上是规范化路径，带 `\\?\` 前缀；把它交给
                // Node 会让 agent 一行都跑不起来（见 `for_command_line`），所以那里做了清洗。
                // 但 `entry_label` 原先用的是**清洗之前**的路径 —— 于是设置页的「Sidecar
                // entry」显示 `\\?\C:\...\agent\index.js`：一段 Node 读不懂、也从来没被
                // 真正执行过的路径，而用户就是靠这一行确认「到底跑了哪个文件」。
                let entry = for_command_line(path);
                Ok(Self {
                    program: node_program(None),
                    args: vec![entry.clone().into_os_string()],
                    env: Vec::new(),
                    request_timeout,
                    entry_label: entry.display().to_string(),
                    client_version: default_client_version(),
                    data_dir: lookup("YUKINAL_DATA_DIR").unwrap_or_default(),
                })
            }
            None => {
                let mut searched: Vec<String> = packaged
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect();
                searched.extend(
                    ancestors(cwd).map(|dir| format!("{}/apps/agent/dist/index.js", dir.display())),
                );
                Err(SidecarError::NotFound {
                    searched: searched.join(", "),
                })
            }
        }
    }

    #[must_use]
    pub fn with_env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_string(), value.to_string()));
        self
    }

    /// True when `program` is a bare command name for the OS to resolve through `PATH`,
    /// rather than a path some caller chose.
    ///
    /// Only this case may be blamed on `PATH` in an error message: `YUKINAL_NODE` and
    /// `YUKINAL_AGENT_COMMAND` always produce a path with a separator (or should), and
    /// telling a user to fix `PATH` when they set an explicit override sends them to the
    /// wrong place.
    #[must_use]
    pub fn resolved_through_path(&self) -> bool {
        self.program.components().count() == 1
    }

    /// Turn a failed `Command::spawn` into something the user can act on.
    ///
    /// A packaged app does not ship a Node runtime, so "no Node installed" is the first
    /// failure a new user is likely to hit, and the bare OS error — `program not found` —
    /// names neither the prerequisite nor a way out. That case gets a real message; every
    /// other spawn failure keeps the plain `program: error` form, because inventing advice
    /// for a permissions error or a bad interpreter would be worse than saying nothing.
    ///
    /// Deliberately *not* a pre-flight `node --version` check: that would spawn a second
    /// process on every start, and it still cannot catch a Node that exists but is too old
    /// (which fails as a parse error on stderr — visible in the retained log tail).
    #[must_use]
    pub fn launch_error(&self, error: &std::io::Error) -> SidecarError {
        if error.kind() == std::io::ErrorKind::NotFound && self.resolved_through_path() {
            return SidecarError::Launch(format!(
                "Node.js was not found on PATH (`{}`). This build does not bundle a Node \
                 runtime, so Node.js {REQUIRED_NODE_MAJOR} or newer must be installed \
                 (https://nodejs.org), or set YUKINAL_NODE to an absolute path to the \
                 executable",
                self.program.display()
            ));
        }
        SidecarError::Launch(format!("{}: {error}", self.program.display()))
    }
}

fn default_client_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// The Node major the agent bundle is built for.
///
/// The installer does **not** ship a Node runtime (ADR 0013), so this is a prerequisite the
/// user's machine has to satisfy. It is duplicated from three places that must agree, and a
/// test below pins it against the first of them: `engines.node` in the root `package.json`,
/// the `--target` in the agent's esbuild step, and the text of the error a user sees when
/// Node is missing.
pub const REQUIRED_NODE_MAJOR: u32 = 24;

fn node_program(override_path: Option<&str>) -> PathBuf {
    match override_path {
        Some(explicit) if !explicit.trim().is_empty() => PathBuf::from(explicit),
        _ => PathBuf::from(if cfg!(windows) { "node.exe" } else { "node" }),
    }
}

/// Turn a path into one Node.js can actually resolve.
///
/// `resource_dir()` comes back canonicalised, and on Windows that means the verbatim prefix
/// (`\\?\C:\...`). Win32 itself treats it as transparent, but Node does not: handed
/// `\\?\C:\...\agent\index.js` it ends up calling `lstat('C:')`, dies with `EISDIR` and never
/// runs a line of the agent. That is not a guess — it is what the app printed when this was
/// fixed (`node.exe ["\\?\C:\...\target\debug\agent\index.js"]` followed by the crash).
///
/// Only the two verbatim forms with a plain equivalent are rewritten. `\\?\Volume{…}` has
/// none: dropping the prefix would name a different path, so it is left alone rather than
/// silently pointed somewhere else.
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

fn find_dev_bundle(cwd: &Path) -> Option<PathBuf> {
    ancestors(cwd)
        .map(|dir| dir.join("apps").join("agent").join("dist").join("index.js"))
        .find(|candidate| candidate.is_file())
}

/// Where a bundled app keeps the agent. Must match `bundle.resources` in
/// `apps/desktop/src-tauri/tauri.conf.json`; `scripts/check.mjs` asserts the two agree,
/// because a rename on one side only would be discovered by an installed user, not by CI.
#[must_use]
pub fn packaged_entry(resources: &Path) -> PathBuf {
    resources.join("agent").join("index.js")
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

    /// `std::env` is process-global and the test harness runs these tests on parallel
    /// threads, so every test that *reads* the resolution environment has to hold this lock
    /// while one that *writes* it is running. Without it `missing_bundle_error_names_the_fix`
    /// intermittently sees the `YUKINAL_AGENT_COMMAND` another test just set.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Guards the bug that kept the desktop app from starting its sidecar at all.
    ///
    /// `resource_dir()` is canonicalised, so on Windows the entry arrives with a `\\?\`
    /// prefix, and Node dies on it before running anything. The app printed exactly this:
    /// `node.exe ["\\?\C:\...\target\debug\agent\index.js"]` and then `EISDIR ... lstat 'C:'`.
    #[cfg(windows)]
    #[test]
    fn a_verbatim_path_is_stripped_before_it_becomes_a_command_line_argument() {
        assert_eq!(
            for_command_line(PathBuf::from(r"\\?\C:\Users\me\agent\index.js")),
            PathBuf::from(r"C:\Users\me\agent\index.js")
        );
        assert_eq!(
            for_command_line(PathBuf::from(r"\\?\UNC\server\share\agent\index.js")),
            PathBuf::from(r"\\server\share\agent\index.js")
        );
        // Nothing to repair: returned unchanged, so this cannot quietly rewrite a good path.
        assert_eq!(
            for_command_line(PathBuf::from(r"C:\already\plain\index.js")),
            PathBuf::from(r"C:\already\plain\index.js")
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_packaged_entry_never_reaches_node_with_a_verbatim_prefix() {
        let entry = for_command_line(packaged_entry(Path::new(r"\\?\C:\app\resources")));
        assert!(
            !entry.to_string_lossy().starts_with(r"\\?\"),
            "{}",
            entry.display()
        );
    }

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
        let _guard = env_guard();
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
        let _guard = env_guard();
        std::env::set_var("YUKINAL_AGENT_COMMAND", "/usr/bin/true");
        std::env::set_var("YUKINAL_AGENT_ARGS", "one;two with space");
        let config = SidecarConfig::from_env_with_cwd(Path::new(".")).expect("explicit config");
        assert_eq!(config.program, PathBuf::from("/usr/bin/true"));
        assert_eq!(config.args.len(), 2);
        assert_eq!(config.args[1], OsString::from("two with space"));
        std::env::remove_var("YUKINAL_AGENT_COMMAND");
        std::env::remove_var("YUKINAL_AGENT_ARGS");
    }

    #[test]
    fn a_packaged_bundle_is_found_in_the_resource_directory() {
        let _guard = env_guard();
        let root = temp_tagged_dir("packaged");
        let resources = root.join("resources");
        let staged = resources.join("agent");
        std::fs::create_dir_all(&staged).expect("create staged dir");
        std::fs::write(staged.join("index.js"), "console.log('packaged')").expect("write bundle");

        let config = SidecarConfig::from_env_with_resources(&root, Some(&resources))
            .expect("packaged bundle resolves");
        assert_eq!(config.args, vec![staged.join("index.js").into_os_string()]);
        assert_eq!(
            config.entry_label,
            staged.join("index.js").display().to_string()
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// Windows 上 `resource_dir()` 是规范化路径，带 `\\?\` 前缀。设置页显示的入口必须
    /// 与真正交给 Node 的那条命令一致 —— 前缀段是 Node 读不懂、也从没被执行过的字。
    #[cfg(windows)]
    #[test]
    fn the_entry_label_is_the_path_that_was_handed_to_node() {
        let _guard = env_guard();
        let root = temp_tagged_dir("verbatim");
        let resources = root.join("resources");
        let staged = resources.join("agent");
        std::fs::create_dir_all(&staged).expect("create staged dir");
        std::fs::write(staged.join("index.js"), "console.log('packaged')").expect("write bundle");

        // 这正是 Tauri 在 Windows 上给出的形状：`\\?\C:\...\resources`。
        let verbatim = PathBuf::from(format!(r"\\?\{}", resources.display()));
        let config = SidecarConfig::from_env_with_resources(&root, Some(&verbatim))
            .expect("a verbatim resource dir still resolves");

        let entry = staged.join("index.js");
        assert_eq!(config.args, vec![entry.clone().into_os_string()]);
        assert_eq!(config.entry_label, entry.display().to_string());
        assert!(
            !config.entry_label.contains(r"\\?\"),
            "the UI must not be shown a prefix Node cannot use: {}",
            config.entry_label
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_packaged_bundle_outranks_a_dev_checkout_and_an_absent_one_falls_back() {
        let _guard = env_guard();
        let root = temp_tagged_dir("precedence");
        let dev_bundle = root.join("apps").join("agent").join("dist");
        std::fs::create_dir_all(&dev_bundle).expect("create dev bundle dir");
        std::fs::write(dev_bundle.join("index.js"), "console.log('dev')")
            .expect("write dev bundle");

        let resources = root.join("resources");
        let staged = resources.join("agent");
        std::fs::create_dir_all(&staged).expect("create staged dir");
        std::fs::write(staged.join("index.js"), "console.log('packaged')").expect("write bundle");

        let packaged = SidecarConfig::from_env_with_resources(&root, Some(&resources))
            .expect("packaged bundle resolves");
        assert_eq!(
            packaged.entry_label,
            staged.join("index.js").display().to_string(),
            "an installed app must run the bundle that was installed with it"
        );

        // A dev checkout has no staged resources, and that must not be an error: the
        // ancestor lookup still has to answer.
        std::fs::remove_file(staged.join("index.js")).expect("remove staged bundle");
        let dev = SidecarConfig::from_env_with_resources(&root, Some(&resources))
            .expect("dev bundle resolves");
        assert_eq!(
            dev.entry_label,
            dev_bundle.join("index.js").display().to_string()
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_not_found_error_names_the_packaged_path_too() {
        let _guard = env_guard();
        let root = temp_tagged_dir("searched");
        let resources = root.join("resources");
        let error = SidecarConfig::from_env_with_resources(&root, Some(&resources))
            .expect_err("nothing resolves");
        let message = error.to_string();
        let expected = packaged_entry(&resources).display().to_string();
        assert!(
            message.contains(&expected),
            "an installed user has to be told which packaged path was missing: {message}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// A config whose program is resolved through `PATH`, i.e. the packaged shape.
    ///
    /// Built as a literal instead of by reading the environment: the ambient
    /// `YUKINAL_AGENT_*` variables are process-global and the integration harness sets them,
    /// so "what does the resolution path produce here" would be a different answer under
    /// `cargo test --workspace` than under `cargo test -p yukinal-core`.
    fn path_resolved_config() -> SidecarConfig {
        SidecarConfig {
            program: node_program(None),
            args: vec![OsString::from("/opt/agent/index.js")],
            env: Vec::new(),
            request_timeout: Duration::from_secs(10),
            entry_label: String::from("/opt/agent/index.js"),
            client_version: default_client_version(),
            data_dir: String::new(),
        }
    }

    #[test]
    fn a_missing_path_node_is_reported_as_a_missing_prerequisite() {
        let config = path_resolved_config();
        assert!(config.resolved_through_path());

        let error = config.launch_error(&std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "program not found",
        ));
        let message = error.to_string();
        // The version has to be in the message: "install Node" without a floor sends the
        // user to a download page that may hand them something too old to run the bundle.
        assert!(
            message.contains(&format!("Node.js {REQUIRED_NODE_MAJOR}")),
            "{message}"
        );
        assert!(message.contains("YUKINAL_NODE"), "{message}");
    }

    #[test]
    fn an_explicit_program_path_is_never_blamed_on_path_lookup() {
        let mut config = path_resolved_config();
        config.program = PathBuf::from("/usr/local/bin/node");
        assert!(!config.resolved_through_path());
        let error = config.launch_error(&std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "program not found",
        ));
        let message = error.to_string();
        assert!(
            !message.contains("PATH"),
            "an explicit override must not be told to fix PATH: {message}"
        );
        assert!(message.contains("/usr/local/bin/node"), "{message}");
    }

    #[test]
    fn a_non_notfound_spawn_failure_keeps_the_plain_message() {
        let config = path_resolved_config();
        let error = config.launch_error(&std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "access is denied",
        ));
        let message = error.to_string();
        assert!(!message.contains("nodejs.org"), "{message}");
        assert!(message.contains("access is denied"), "{message}");
    }

    /// The Node floor is stated in four places that cannot import each other: this constant,
    /// the root `package.json` `engines.node`, the agent's esbuild `--target`, and the
    /// installed-app docs. They are only allowed to agree, and this is the one pair a test
    /// can actually check, so it is checked rather than trusted.
    #[test]
    fn the_node_floor_matches_the_declared_engine_range() {
        let manifest = include_str!("../../../../package.json");
        let expected = format!("\">={REQUIRED_NODE_MAJOR}\"");
        assert!(
            manifest.contains(&expected),
            "root package.json engines.node must stay {expected}; raise both together"
        );
    }
}
