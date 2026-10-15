//! Sidecar lifecycle and Agent event forwarding.
//!
//! This module owns the native process seam. It does not define the shared IPC contract;
//! it adapts the supervisor's typed events to the Tauri window and delegates host requests.

use super::*;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use yukinal_credentials::{CredentialRef, CredentialStore};
use yukinal_filesystem::{AgentReadRequest, DEFAULT_AGENT_READ_BYTES};

/// Smoke test: proves the IPC round trip without pretending to do real work.
#[tauri::command]
pub fn core_ping() -> PingResponse {
    PingResponse {
        version: env!("CARGO_PKG_VERSION"),
        os: std::env::consts::OS,
    }
}

/// Launch the agent sidecar and handshake with it. React never spawns processes:
/// ownership of the child stays on this side of the boundary (ADR 0001).
#[tauri::command]
pub async fn agent_spawn(app: AppHandle) -> Result<AgentSpawnResponse, String> {
    start_sidecar(&app).await
}

/// The only code path that starts a sidecar. The dev autostart hook calls this same
/// function, so an automated run exercises exactly what a user click does (config
/// resolution, app-data dir, handshake, event forwarding).
pub(crate) async fn start_sidecar(app: &AppHandle) -> Result<AgentSpawnResponse, String> {
    let config = resolve_config(app)?;
    let outcome = app
        .state::<AppState>()
        .supervisor
        .start(&config)
        .await
        .map_err(|error| error.to_string())?;

    // Nothing to wire up here: the event forwarder belongs to the window (see
    // `forward_sidecar_events`), so a start — from the UI, from the dev autostart hook, or
    // from the supervisor's own restart path — reuses the one that is already running.

    // 逐字段手抄改成 `From`：那条映射现在住在 `crates/core/src/ipc.rs`（契约的所在地），
    // 见那里的注释 —— 手抄的问题不是风格，而是给 RuntimeInfo 加一个字段却忘了这里时，
    // 响应照样编译、照样序列化，只是悄悄少一个字段。
    Ok(AgentSpawnResponse::from(&outcome))
}

#[tauri::command]
pub async fn agent_status(state: State<'_, AppState>) -> Result<SupervisorStatus, String> {
    Ok(state.supervisor.status().await)
}

#[tauri::command]
pub async fn agent_kill(state: State<'_, AppState>) -> Result<AgentKillResponse, String> {
    Ok(AgentKillResponse {
        killed: state.supervisor.stop().await,
    })
}

/// Recent sidecar stderr, for the "why did it die" affordance.
#[tauri::command]
pub async fn agent_logs(state: State<'_, AppState>) -> Result<AgentLogsResponse, String> {
    Ok(AgentLogsResponse {
        lines: state.supervisor.logs().await,
        capacity: LOG_HISTORY,
    })
}

/// Decide what to launch. Resolution order lives in
/// `SidecarConfig::from_env_with_resources` (ADR 0008, ADR 0013); this only tells it where
/// this build keeps its resources and supplies the app data dir when the caller did not set
/// one.
///
/// The resource dir is passed in both shapes, and the original comment here was wrong about
/// dev: `tauri dev` *does* stage `bundle.resources` into the target directory
/// (Python under `target/debug/python-agent/`, plus the transition Node resource), so in a
/// dev run staged resources can win and the repo walk-up is the fallback (a bare `cargo run`
/// without Tauri's staging). One resolution path for both shapes is still the point — it is
/// what makes the installed case the exercised one instead of a debug-only branch.
///
/// Whatever this returns has passed through `SidecarConfig::for_command_line`:
/// `resource_dir()` is canonicalised, and the installed executable and any script path must
/// be passed in a form their runtime can resolve.
fn resolve_config(app: &AppHandle) -> Result<SidecarConfig, String> {
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let resources = app.path().resource_dir().ok();
    let mut config = SidecarConfig::from_env_with_resources(&cwd, resources.as_deref())
        .map_err(|error| error.to_string())?;

    if config.data_dir.trim().is_empty() {
        let data_dir = app
            .path()
            .app_data_dir()
            .map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&data_dir).map_err(|error| error.to_string())?;
        config.data_dir = data_dir.display().to_string();
    }
    let data_dir = config.data_dir.clone();
    // The Agent normally gets a minimal, credential-free environment. The C++ service is
    // started only when an operator supplies its config; pass only the config's explicitly
    // named credential variables, and only to the Python Agent which launches the service.
    if config.is_python_agent {
        if let Some(config_path) =
            std::env::var_os("YUKINAL_SSH_SERVICE_CONFIG").filter(|value| !value.is_empty())
        {
            let service =
                resolve_ssh_service_runtime(Path::new(&config_path), &cwd, resources.as_deref())?;
            config = config
                .with_python_ssh_service_env(Some(&service.token), Some(&service.base_url))
                .with_python_ssh_service_process(
                    &service.binary,
                    &service.config,
                    &service.referenced_environment,
                );
        } else {
            let ssh_token = std::env::var("YUKINAL_SSH_API_TOKEN").ok();
            let ssh_api_url = std::env::var("YUKINAL_SSH_API_URL").ok();
            if ssh_token.as_deref().is_some_and(|token| !token.is_empty()) {
                config = config
                    .with_python_ssh_service_env(ssh_token.as_deref(), ssh_api_url.as_deref());
            } else if let Some(binary) = resolve_ssh_service_binary(&cwd, resources.as_deref())? {
                config = config.with_python_ssh_service_autoconfig(&binary);
            } else {
                eprintln!(
                    "[yukinal] C++ SSH service is unavailable; desktop SSH Agent tools are disabled"
                );
            }
        }
    }
    // 「为什么起不来」的一半答案是「到底起了什么」。一条启动行比事后猜文件名便宜得多：
    // `node C:` 这种崩法在日志里看起来完全不像路径解析问题，而它确实是。
    eprintln!(
        "[yukinal] sidecar command: {} {:?} (data dir {})",
        config.program.display(),
        config.args,
        data_dir
    );
    Ok(config.with_env("YUKINAL_DATA_DIR", &data_dir))
}

struct SshServiceRuntime {
    binary: PathBuf,
    config: PathBuf,
    token: String,
    base_url: String,
    referenced_environment: Vec<(String, String)>,
}

fn resolve_ssh_service_runtime(
    config_path: &Path,
    cwd: &Path,
    resources: Option<&Path>,
) -> Result<SshServiceRuntime, String> {
    let config = std::fs::canonicalize(config_path).map_err(|_| {
        "YUKINAL_SSH_SERVICE_CONFIG must point to a readable SSH service JSON file".to_string()
    })?;
    let metadata = std::fs::metadata(&config).map_err(|_| {
        "YUKINAL_SSH_SERVICE_CONFIG must point to a readable SSH service JSON file".to_string()
    })?;
    if metadata.len() > 1024 * 1024 {
        return Err("the SSH service configuration exceeds the 1 MiB size limit".into());
    }
    let bytes = std::fs::read(&config).map_err(|_| {
        "YUKINAL_SSH_SERVICE_CONFIG must point to a readable SSH service JSON file".to_string()
    })?;
    let document: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| "the SSH service configuration is not valid JSON".to_string())?;
    let port = document
        .get("listen_port")
        .and_then(serde_json::Value::as_u64)
        .filter(|port| (1..=u16::MAX as u64).contains(port))
        .ok_or_else(|| "the SSH service configuration has an invalid listen_port".to_string())?;
    let token_name = document
        .get("api_token_env")
        .and_then(serde_json::Value::as_str)
        .filter(|name| *name == "YUKINAL_SSH_API_TOKEN")
        .ok_or_else(|| {
            "desktop SSH service config must use YUKINAL_SSH_API_TOKEN for api_token_env"
                .to_string()
        })?;
    let targets = document
        .get("targets")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            "the SSH service configuration has an invalid target allowlist".to_string()
        })?;

    let mut names = BTreeSet::from([token_name.to_string()]);
    let uses_agent = targets.iter().any(|target| {
        target
            .get("auth")
            .and_then(serde_json::Value::as_object)
            .and_then(|auth| auth.get("type"))
            .and_then(serde_json::Value::as_str)
            == Some("agent")
    });
    for target in targets {
        let Some(auth) = target.get("auth").and_then(serde_json::Value::as_object) else {
            continue;
        };
        for key in ["password_env", "passphrase_env", "private_key_env"] {
            if let Some(value) = auth.get(key) {
                let name = value
                    .as_str()
                    .filter(|name| valid_ssh_credential_environment_name(name))
                    .ok_or_else(|| format!("the SSH service configuration has an invalid {key}"))?;
                if names
                    .iter()
                    .any(|existing| existing != name && existing.eq_ignore_ascii_case(name))
                {
                    return Err(
                        "SSH service environment references differ only by letter case".into(),
                    );
                }
                names.insert(name.to_string());
            }
        }
    }
    let agent_socket = if uses_agent {
        if names
            .iter()
            .any(|name| name.eq_ignore_ascii_case("SSH_AUTH_SOCK"))
        {
            return Err("SSH_AUTH_SOCK cannot also be used as a credential reference".into());
        }
        match std::env::var_os("SSH_AUTH_SOCK") {
            Some(value) => {
                let value = value
                    .into_string()
                    .map_err(|_| "SSH_AUTH_SOCK is not a valid Unicode socket path".to_string())?;
                if value.is_empty() || value.len() > 4096 || value.contains('\0') {
                    return Err("SSH_AUTH_SOCK is not a valid socket path".into());
                }
                Some(value)
            }
            None if cfg!(windows) => None,
            None => {
                return Err("set SSH_AUTH_SOCK for the configured SSH agent target".into());
            }
        }
    } else {
        None
    };
    let token = std::env::var(token_name).map_err(|_| {
        format!("set the SSH service API token in the {token_name} environment variable")
    })?;
    if token.is_empty() {
        return Err(format!(
            "the SSH service API token in {token_name} is empty"
        ));
    }
    let mut referenced_environment = names
        .into_iter()
        .filter(|name| name != token_name)
        .map(|name| {
            std::env::var(&name)
                .map(|value| (name.clone(), value))
                .map_err(|_| {
                    format!("set the configured SSH credential in the {name} environment variable")
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(socket) = agent_socket {
        referenced_environment.push(("SSH_AUTH_SOCK".into(), socket));
    }

    let binary = resolve_ssh_service_binary(cwd, resources)?.ok_or_else(|| {
            "the C++ SSH service executable is unavailable; build it with `pnpm build:ssh-service` or set YUKINAL_SSH_SERVICE_BINARY".to_string()
        })?;
    let base_url =
        std::env::var("YUKINAL_SSH_API_URL").unwrap_or_else(|_| format!("http://127.0.0.1:{port}"));

    Ok(SshServiceRuntime {
        binary,
        config,
        token,
        base_url,
        referenced_environment,
    })
}

pub(super) fn resolve_ssh_service_binary(
    cwd: &Path,
    resources: Option<&Path>,
) -> Result<Option<PathBuf>, String> {
    let explicit = std::env::var_os("YUKINAL_SSH_SERVICE_BINARY")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    if explicit.as_ref().is_some_and(|path| !path.is_file()) {
        return Err(
            "YUKINAL_SSH_SERVICE_BINARY does not point to a C++ SSH service executable".into(),
        );
    }
    let packaged = resources.map(yukinal_core::sidecar::packaged_ssh_service_entry);
    let Some(binary) = explicit
        .or_else(|| packaged.filter(|path| path.is_file()))
        .or_else(|| find_dev_ssh_service(cwd))
    else {
        return Ok(None);
    };
    std::fs::canonicalize(binary)
        .map(Some)
        .map_err(|_| "the C++ SSH service executable could not be resolved".to_string())
}

fn find_dev_ssh_service(cwd: &Path) -> Option<PathBuf> {
    let executable = if cfg!(windows) {
        "yukinal-ssh-service.exe"
    } else {
        "yukinal-ssh-service"
    };
    for directory in cwd.ancestors() {
        for candidate in [
            directory
                .join("native/build-desktop/Release")
                .join(executable),
            directory.join("native/build-desktop").join(executable),
            directory
                .join("native/build-nmake/Release")
                .join(executable),
            directory.join("native/build-nmake").join(executable),
            directory.join("native/build/Release").join(executable),
            directory.join("native/build").join(executable),
        ] {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn valid_environment_name(name: &str) -> bool {
    let mut characters = name.chars();
    matches!(characters.next(), Some('_' | 'A'..='Z' | 'a'..='z'))
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
        && name.len() <= 128
}

fn valid_ssh_credential_environment_name(name: &str) -> bool {
    valid_environment_name(name)
        && !name.to_ascii_uppercase().starts_with("YUKINAL_")
        && ![
            "PATH",
            "SYSTEMROOT",
            "WINDIR",
            "TEMP",
            "TMP",
            "HOME",
            "USERPROFILE",
            "TMPDIR",
            "LD_LIBRARY_PATH",
            "DYLD_LIBRARY_PATH",
        ]
        .iter()
        .any(|reserved| name.eq_ignore_ascii_case(reserved))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SshServiceTargetRequest {
    server_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SshServiceFilesystemReadTargetRequest {
    server_id: String,
    path: String,
    max_bytes: Option<usize>,
}

/// Resolve one selected desktop target for a short-lived C++ SSH service process.
/// Secrets leave the OS credential store only for this local sidecar request and
/// are returned separately from the non-secret target allowlist.
pub(super) fn resolve_ssh_service_target(state: &AppState, params: Value) -> Result<Value, String> {
    let request: SshServiceTargetRequest = serde_json::from_value(params)
        .map_err(|_| "the SSH service target request is invalid".to_string())?;
    if request.server_id.len() <= 4
        || request.server_id.len() > 160
        || !request.server_id.starts_with("srv_")
        || !request.server_id[4..]
            .chars()
            .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
    {
        return Err("the SSH service target ID is invalid".into());
    }

    let server = state
        .database
        .servers()
        .get(&request.server_id)
        .map_err(|_| "the selected SSH server is not available".to_string())?;
    if server.connection.host_certificate_authority.is_some() {
        return Err("C++ SSH service does not yet support host certificate authorities".into());
    }
    let fingerprint = state
        .ssh
        .host_key_pin(&server.connection.host, server.connection.port)
        .map_err(|_| "the selected server host-key pin could not be read".to_string())?
        .ok_or_else(|| "the selected server has no confirmed host-key pin".to_string())?;
    let proxy_route = super::ssh_service::resolve_cpp_proxy_route(state)?;
    if matches!(&proxy_route, super::ssh_service::CppProxyRoute::Unsupported) {
        return Err("the configured proxy route is not supported by the C++ SSH service".into());
    }

    let identity_id = server
        .connection
        .identity_id
        .as_deref()
        .ok_or_else(|| "the selected server has no SSH identity".to_string())?;
    let identity = state
        .database
        .identities()
        .get(identity_id)
        .map_err(|_| "the selected server SSH identity is unavailable".to_string())?;

    let mut credentials = serde_json::Map::new();
    let auth = match identity.method.as_str() {
        "password" => {
            credentials.insert(
                "YK_SSH_PASSWORD".into(),
                serde_json::Value::String(read_ssh_secret(
                    state.credentials.as_ref(),
                    &identity.credential_ref,
                    "password",
                )?),
            );
            json!({ "type": "password", "password_env": "YK_SSH_PASSWORD" })
        }
        "privateKey" | "certificate" => {
            credentials.insert(
                "YK_SSH_PRIVATE_KEY".into(),
                serde_json::Value::String(read_ssh_secret(
                    state.credentials.as_ref(),
                    &identity.credential_ref,
                    "private key",
                )?),
            );
            let passphrase_env = if let Some(reference) = identity.passphrase_ref.as_deref() {
                credentials.insert(
                    "YK_SSH_PASSPHRASE".into(),
                    serde_json::Value::String(read_ssh_secret(
                        state.credentials.as_ref(),
                        reference,
                        "private-key passphrase",
                    )?),
                );
                Some("YK_SSH_PASSPHRASE")
            } else {
                None
            };
            let mut auth = json!({
                "type": "private_key",
                "private_key_env": "YK_SSH_PRIVATE_KEY",
            });
            if let Some(passphrase_env) = passphrase_env {
                auth["passphrase_env"] = json!(passphrase_env);
            }
            if identity.method == "certificate" {
                let certificate_path = identity
                    .certificate_path
                    .clone()
                    .or_else(|| {
                        identity
                            .private_key_path
                            .as_deref()
                            .map(|path| format!("{path}-cert.pub"))
                    })
                    .filter(|path| {
                        !path.is_empty()
                            && path.len() <= 4096
                            && !path.chars().any(char::is_control)
                            && !path.contains('\0')
                    })
                    .ok_or_else(|| {
                        "the selected SSH certificate identity has no valid public certificate file"
                            .to_string()
                    })?;
                let certificate_path = std::path::Path::new(&certificate_path);
                let certificate_path = if certificate_path.is_absolute() {
                    certificate_path.to_path_buf()
                } else {
                    std::env::current_dir()
                        .map_err(|_| "the desktop working directory is unavailable")?
                        .join(certificate_path)
                };
                let certificate_path = certificate_path.to_string_lossy().into_owned();
                if certificate_path.len() > 4096 {
                    return Err("the SSH public certificate path is too long".into());
                }
                auth["public_key_file"] = json!(certificate_path);
            }
            auth
        }
        "agent" => {
            if let Some(socket) = std::env::var_os("SSH_AUTH_SOCK") {
                let socket = socket
                    .into_string()
                    .map_err(|_| "the SSH agent socket path is not valid Unicode".to_string())?;
                if socket.is_empty() || socket.len() > 4096 || socket.contains('\0') {
                    return Err("the SSH agent socket path is invalid".into());
                }
                credentials.insert("SSH_AUTH_SOCK".into(), serde_json::Value::String(socket));
            } else if !cfg!(windows) {
                return Err("SSH_AUTH_SOCK is not set for the selected SSH agent identity".into());
            }
            json!({ "type": "agent" })
        }
        _ => {
            return Err(
                "the selected SSH identity method is not supported by the C++ service".into(),
            );
        }
    };

    let mut target = json!({
            "target_id": server.id,
            "host": server.connection.host,
            "port": server.connection.port,
            "username": server.connection.username,
            "host_key_fingerprint": fingerprint,
            "auth": auth,
        });
    match proxy_route {
        super::ssh_service::CppProxyRoute::Direct => {}
        super::ssh_service::CppProxyRoute::HttpConnect {
            host,
            port,
            credential,
            no_proxy,
        } => {
            let mut proxy = json!({
                "type": "http_connect",
                "host": host,
                "port": port,
                "no_proxy": no_proxy,
            });
            if let Some(credential) = credential {
                credentials.insert(
                    "YK_SSH_PROXY_CREDENTIAL".into(),
                    serde_json::Value::String(credential),
                );
                proxy["auth_env"] = json!("YK_SSH_PROXY_CREDENTIAL");
            }
            target["proxy"] = proxy;
        }
        super::ssh_service::CppProxyRoute::Unsupported => unreachable!("route checked above"),
    }

    Ok(json!({
        "target": target,
        "credentials": credentials,
    }))
}

/// Resolve a C++ target only after the existing host-owned Agent file policy accepts the exact read.
pub(super) fn resolve_ssh_service_filesystem_read_target(
    state: &AppState,
    params: Value,
) -> Result<Value, String> {
    let request: SshServiceFilesystemReadTargetRequest = serde_json::from_value(params)
        .map_err(|_| "the SSH filesystem read target request is invalid".to_string())?;
    let (path, max_bytes) = checked_agent_filesystem_read(&request.path, request.max_bytes)?;
    let mut payload =
        resolve_ssh_service_target(state, json!({ "serverId": request.server_id }))?;
    payload["filesystemRead"] = json!({ "path": path, "maxBytes": max_bytes });
    Ok(payload)
}

fn checked_agent_filesystem_read(
    path: &str,
    max_bytes: Option<usize>,
) -> Result<(String, usize), String> {
    AgentReadRequest::check(path, max_bytes).map_err(|error| error.to_string())?;
    Ok((path.to_string(), max_bytes.unwrap_or(DEFAULT_AGENT_READ_BYTES)))
}

/// Resolve a single endpoint into a target that can perform only a host-key handshake.
/// It has no SSH identity and no configured trust source; the result is reported to the
/// existing user-confirmation flow and is never persisted by this resolver.
pub(super) fn resolve_ssh_service_probe_target(
    state: &AppState,
    params: Value,
) -> Result<Value, String> {
    let request: SshServiceTargetRequest = serde_json::from_value(params)
        .map_err(|_| "the SSH host-key probe request is invalid".to_string())?;
    if request.server_id.len() <= 4
        || request.server_id.len() > 160
        || !request.server_id.starts_with("srv_")
        || !request.server_id[4..]
            .chars()
            .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
    {
        return Err("the SSH host-key probe target ID is invalid".into());
    }
    let server = state
        .database
        .servers()
        .get(&request.server_id)
        .map_err(|_| "the selected SSH server is not available".to_string())?;
    let proxy_route = super::ssh_service::resolve_cpp_proxy_route(state)?;
    if matches!(&proxy_route, super::ssh_service::CppProxyRoute::Unsupported) {
        return Err("the configured proxy route is not supported by the C++ SSH service".into());
    }

    let mut target = json!({
            "target_id": server.id,
            "host": server.connection.host,
            "port": server.connection.port,
            "username": "ssh_probe",
            "probe_only": true,
            "auth": { "type": "probe" },
        });
    let mut credentials = serde_json::Map::new();
    match proxy_route {
        super::ssh_service::CppProxyRoute::Direct => {}
        super::ssh_service::CppProxyRoute::HttpConnect {
            host,
            port,
            credential,
            no_proxy,
        } => {
            let mut proxy = json!({
                "type": "http_connect",
                "host": host,
                "port": port,
                "no_proxy": no_proxy,
            });
            if let Some(credential) = credential {
                credentials.insert(
                    "YK_SSH_PROXY_CREDENTIAL".into(),
                    serde_json::Value::String(credential),
                );
                proxy["auth_env"] = json!("YK_SSH_PROXY_CREDENTIAL");
            }
            target["proxy"] = proxy;
        }
        super::ssh_service::CppProxyRoute::Unsupported => unreachable!("route checked above"),
    }

    Ok(json!({ "target": target, "credentials": credentials }))
}

fn read_ssh_secret(
    credentials: &dyn CredentialStore,
    reference: &str,
    label: &str,
) -> Result<String, String> {
    let reference = CredentialRef::parse(reference)
        .map_err(|_| format!("the selected server {label} reference is invalid"))?;
    let secret = credentials.get(&reference).map_err(|_| {
        format!("the selected server {label} is unavailable in the OS credential store")
    })?;
    let value = secret
        .as_utf8()
        .map_err(|_| format!("the selected server {label} is not valid UTF-8"))?
        .into_owned();
    if value.is_empty() || value.len() > 1024 * 1024 {
        return Err(format!(
            "the selected server {label} is empty or exceeds the size limit"
        ));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::checked_agent_filesystem_read;
    use yukinal_filesystem::DEFAULT_AGENT_READ_BYTES;

    #[test]
    fn cpp_file_read_binding_uses_the_validated_default_and_limit() {
        assert_eq!(
            checked_agent_filesystem_read("/etc/hosts", None).expect("default read is valid"),
            ("/etc/hosts".to_string(), DEFAULT_AGENT_READ_BYTES)
        );
        assert_eq!(
            checked_agent_filesystem_read("/etc/hosts", Some(64)).expect("bounded read is valid"),
            ("/etc/hosts".to_string(), 64)
        );
    }

    #[test]
    fn cpp_file_read_binding_rejects_blocked_paths_and_oversized_limits() {
        assert!(checked_agent_filesystem_read("/home/deploy/.ssh/id_rsa", None).is_err());
        assert!(checked_agent_filesystem_read("relative/path", None).is_err());
        assert!(checked_agent_filesystem_read("/etc/hosts", Some(1_048_577)).is_err());
    }
}

/// One task per launched sidecar: keeps stderr visible and maps sidecar notifications
/// onto the desktop event channels.
///
/// Created **once**, from the window setup, not per start. It used to be called by
/// `start_sidecar` for every non-reused start and relied on `SidecarEvent::Exited` to end
/// the task; the supervisor deliberately did not republish that event, so after a crash
/// and a restart two forwarders were attached to the same broadcast channel. Every frame
/// was then forwarded twice, and every `host.tool.execute` request — which is answered by a
/// task spawned per received event — was *executed twice* on the target.
pub(crate) fn forward_sidecar_events(app: AppHandle) {
    let supervisor = app.state::<AppState>().supervisor.clone();
    let shutdown = app.state::<AppState>().shutdown.clone();
    let mut receiver = supervisor.subscribe();
    let cancellations: host::HostCancellationRegistry =
        Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    tauri::async_runtime::spawn(async move {
        // The sidecar's startup lines are written before this task exists, and a
        // broadcast channel does not replay them. Print the retained tail first so
        // "what the agent said when it booted" is never invisible.
        for line in supervisor.logs().await {
            eprintln!("[agent] {line}");
        }
        loop {
            let event = tokio::select! {
                biased;
                _ = shutdown.cancelled() => {
                    if let Ok(pending) = cancellations.lock() {
                        for token in pending.values() {
                            token.cancel();
                        }
                    }
                    break;
                }
                event = receiver.recv() => event,
            };
            match event {
                Ok(event) => match event {
                    SidecarEvent::Log(line) => eprintln!("[agent] {line}"),
                    // 上行通知：`agent.stream` 的 payload 是 AgentStreamEvent，按
                    // 其 type 原样转成 Tauri 事件（agent.thinking / tool_call / …）。
                    SidecarEvent::Frame(frame) => forward_agent_frame(&app, &frame),
                    SidecarEvent::Request { id, method, params } => {
                        // Register the token before spawning the request task. This closes the
                        // race where a cancellation frame arrives immediately after execute.
                        let registration = if method == host::HOST_TOOL_CANCEL {
                            Ok(None)
                        } else {
                            let token = CancellationToken::new();
                            match cancellations.lock() {
                                Ok(mut pending) => {
                                    pending.insert(id, token.clone());
                                    Ok(Some(token))
                                }
                                Err(_) => Err("host cancellation registry is poisoned".to_string()),
                            }
                        };
                        let app = app.clone();
                        let supervisor = supervisor.clone();
                        let cancellations = Arc::clone(&cancellations);
                        tauri::async_runtime::spawn(async move {
                            let is_cancel = method == host::HOST_TOOL_CANCEL;
                            let outcome = if is_cancel {
                                host::cancel_sidecar_request(&cancellations, params)
                            } else {
                                let outcome = match registration {
                                    Ok(Some(token)) => {
                                        let state = app.state::<AppState>();
                                        host::handle_sidecar_request_with_app_and_cancel(
                                            &app, &state, &method, params, token,
                                        )
                                        .await
                                    }
                                    Ok(None) => {
                                        Err("host request was not registered for cancellation"
                                            .to_string())
                                    }
                                    Err(error) => Err(error),
                                };
                                if let Ok(mut pending) = cancellations.lock() {
                                    pending.remove(&id);
                                }
                                outcome
                            };
                            if let Some(handle) = supervisor.handle().await {
                                if let Err(error) = handle.respond(id, outcome).await {
                                    eprintln!("[agent] host response failed: {error}");
                                }
                            }
                        });
                    }
                    SidecarEvent::Exited { code, signal } => {
                        // Observed, not a reason to leave: this forwarder is created once for
                        // the life of the window, and the supervisor may already be bringing
                        // a new agent up behind it. Breaking here would silently stop
                        // forwarding the *restarted* agent's frames, and the next start would
                        // have to create a second forwarder — which is how one `host.*`
                        // request ends up executed twice.
                        eprintln!("[agent] exited code={code:?} signal={signal:?}");
                        let state = app.state::<AppState>();
                        let now = yukinal_core::sidecar::iso8601_now();
                        if let Err(error) = state
                            .database
                            .investigations()
                            .interrupt_active_investigation_runs(&now, "sidecar_exited")
                        {
                            eprintln!("[agent] investigation recovery failed: {error}");
                        }
                    }
                },
                Err(error) => {
                    eprintln!("[agent] event stream closed: {error}");
                    break;
                }
            }
        }
    });
}
