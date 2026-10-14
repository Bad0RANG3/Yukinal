//! Short-lived desktop bridge to the native C++ SSH service.
//!
//! Each desktop request gets a one-target allowlist and credentials passed only in the child
//! environment. The temporary JSON contains target metadata and environment-variable names.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::TcpListener;
use std::pin::Pin;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex as StdMutex;
use std::task::{Context, Poll};
use std::time::Duration;

use base64::Engine as _;
use ring::rand::SecureRandom;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf,
};
use tokio::process::{Child as TokioChild, ChildStdin, ChildStdout, Command as TokioCommand};
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};
use yukinal_filesystem::transfer::{TransferError, TransferReader, TransferWriter};
use yukinal_filesystem::{
    RemoteEntryKind, RemoteStat, ReplaceError, ReplaceGuard, ReplacedFile, TransportError,
};
use yukinal_terminal::{TerminalPty, TerminalPtyEvent};

use crate::state::AppState;

pub(crate) async fn service_inventory(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
) -> Result<String, String> {
    run_operation(app, state, server_id, "service_inventory", json!({})).await
}

pub(crate) async fn docker_ps(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    include_all_containers: bool,
) -> Result<(i32, String), String> {
    run_operation_with_status(
        app,
        state,
        server_id,
        "docker_ps",
        json!({ "all": include_all_containers }),
        true,
    )
    .await
}

pub(crate) async fn docker_logs(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    container: &str,
    tail: usize,
) -> Result<(i32, String), String> {
    run_operation_with_status(
        app,
        state,
        server_id,
        "docker_logs",
        json!({ "container": container, "tail": tail }),
        true,
    )
    .await
}

pub(crate) async fn docker_inspect(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    container: &str,
) -> Result<(i32, String), String> {
    run_operation_with_status(
        app,
        state,
        server_id,
        "docker_inspect",
        json!({ "container": container }),
        true,
    )
    .await
}

pub(crate) async fn systemd_inspect(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    service: &str,
) -> Result<(i32, String), String> {
    run_operation_with_status(
        app,
        state,
        server_id,
        "systemd_inspect",
        json!({ "service": service }),
        true,
    )
    .await
}

pub(crate) async fn package_inspect(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    manager: &str,
    package: &str,
) -> Result<(i32, String), String> {
    run_operation_with_status(
        app,
        state,
        server_id,
        "package_inspect",
        json!({ "manager": manager, "package": package }),
        true,
    )
    .await
}

pub(crate) async fn docker_restart(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    container: &str,
    grace_period_seconds: usize,
) -> Result<(i32, String), String> {
    run_operation_with_status(
        app,
        state,
        server_id,
        "docker_restart",
        json!({ "container": container, "grace_period_seconds": grace_period_seconds }),
        true,
    )
    .await
}

pub(crate) async fn systemd_restart(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    service: &str,
    timeout_seconds: usize,
) -> Result<(i32, String), String> {
    run_operation_with_status(
        app,
        state,
        server_id,
        "systemd_restart",
        json!({ "service": service, "timeout_seconds": timeout_seconds }),
        true,
    )
    .await
}

pub(crate) async fn package_install(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    manager: &str,
    package: &str,
    version: Option<&str>,
    timeout_seconds: usize,
) -> Result<(i32, String), String> {
    run_operation_with_status(
        app,
        state,
        server_id,
        "package_install",
        json!({
            "manager": manager,
            "package": package,
            "version": version,
            "timeout_seconds": timeout_seconds,
        }),
        true,
    )
    .await
}

pub(crate) async fn connection_check(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
) -> Result<(), String> {
    let output = run_operation(app, state, server_id, "connection_check", json!({})).await?;
    let response: Value = serde_json::from_str(&output)
        .map_err(|_| "the C++ SSH service returned an invalid connection check result")?;
    if response.get("connected").and_then(Value::as_bool) == Some(true) {
        Ok(())
    } else {
        Err("the C++ SSH service did not confirm the SSH connection".into())
    }
}

pub(crate) async fn system_snapshot(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
) -> Result<HashMap<String, (i32, String)>, String> {
    const COMMAND_IDS: &[&str] = &[
        "os_detect",
        "os_release",
        "os_kernel",
        "os_arch",
        "os_hostname",
        "cpu_detect",
        "cpu_ticks_a",
        "cpu_ticks_b",
        "cpu_cores",
        "cpu_model",
        "cpu_load",
        "memory_detect",
        "memory_info",
        "uptime_detect",
        "uptime_info",
        "disk_detect",
        "disk_usage",
        "network_detect",
        "network_info",
        "docker_detect",
        "docker_info",
        "docker_ps",
    ];
    let mut random = [0_u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| "could not create a system snapshot request identifier")?;
    let nonce = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let output = run_operation(
        app,
        state,
        server_id,
        "system_snapshot",
        json!({ "nonce": nonce }),
    )
    .await?;

    let mut results = HashMap::with_capacity(COMMAND_IDS.len());
    for id in COMMAND_IDS {
        let start_marker = format!("__YK_SNAPSHOT_{nonce}_{id}_BEGIN__\n");
        let start = output.find(&start_marker).ok_or_else(|| {
            "the C++ SSH service returned an incomplete system snapshot".to_string()
        })?;
        if output[start + start_marker.len()..].contains(&start_marker) {
            return Err("the C++ SSH service returned an ambiguous system snapshot".into());
        }
        let data_start = start + start_marker.len();
        let end_prefix = format!("\n__YK_SNAPSHOT_{nonce}_{id}_END_");
        let end = output[data_start..]
            .find(&end_prefix)
            .map(|offset| data_start + offset)
            .ok_or_else(|| {
                "the C++ SSH service returned an incomplete system snapshot".to_string()
            })?;
        let status_start = end + end_prefix.len();
        let status_end = output[status_start..]
            .find("__\n")
            .map(|offset| status_start + offset)
            .ok_or_else(|| "the C++ SSH service returned an invalid system snapshot".to_string())?;
        let exit_code = output[status_start..status_end]
            .parse::<i32>()
            .ok()
            .filter(|status| (0..=255).contains(status))
            .ok_or_else(|| {
                "the C++ SSH service returned an invalid system snapshot status".to_string()
            })?;
        results.insert(
            (*id).to_string(),
            (exit_code, output[data_start..end].to_string()),
        );
    }
    Ok(results)
}

pub(crate) async fn probe_host_key(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
) -> Result<Option<String>, String> {
    if matches!(resolve_cpp_proxy_route(state)?, CppProxyRoute::Unsupported) {
        return Ok(None);
    }
    let target_payload =
        super::sidecar::resolve_ssh_service_probe_target(state, json!({ "serverId": server_id }))?;
    let response = match run_operation_response_with_target(
        app,
        server_id,
        "host_key_probe",
        json!({}),
        target_payload,
    )
    .await
    {
        Ok(response) => response,
        Err(error) if error.code == "host_key_unsupported" => return Ok(None),
        Err(error) => return Err(error.message),
    };
    validate_operation_response(&response, server_id, "host_key_probe")?;
    let fingerprint = response
        .get("stdout")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "the C++ SSH service returned an invalid host-key probe result")?;
    if !valid_host_key_fingerprint(&fingerprint) {
        return Err("the C++ SSH service returned an invalid host-key fingerprint".into());
    }
    Ok(Some(fingerprint))
}

fn valid_host_key_fingerprint(value: &str) -> bool {
    value.strip_prefix("SHA256:").is_some_and(|encoded| {
        encoded.len() == 43
            && encoded
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'))
    })
}

pub(crate) struct SshTerminalPty {
    input: AsyncMutex<ChildStdin>,
    child: AsyncMutex<TokioChild>,
    events: StdMutex<Option<mpsc::Receiver<TerminalPtyEvent>>>,
    closed: AtomicBool,
    _config_directory: tempfile::TempDir,
}

#[derive(Debug)]
pub(crate) enum TerminalOpenFailure {
    AuthenticationFailed,
    Failed(String),
}

pub(crate) enum CppProxyRoute {
    Direct,
    HttpConnect {
        host: String,
        port: u16,
        credential: Option<String>,
        no_proxy: Vec<String>,
    },
    Unsupported,
}

/// Resolve the app's selected proxy route for the C++ transport. The C++ service
/// applies supported bypass patterns against its one configured target.
pub(crate) fn resolve_cpp_proxy_route(state: &AppState) -> Result<CppProxyRoute, String> {
    let outbound =
        super::network::resolve_outbound_proxy(&state.database, state.credentials.as_ref())?;
    Ok(cpp_proxy_route(outbound))
}

fn cpp_proxy_route(outbound: yukinal_net::OutboundProxy) -> CppProxyRoute {
    match outbound.proxy {
        yukinal_net::NetworkProxy::Direct => CppProxyRoute::Direct,
        yukinal_net::NetworkProxy::Unusable { .. } => CppProxyRoute::Unsupported,
        yukinal_net::NetworkProxy::Proxy { url, no_proxy, .. } => {
            let no_proxy = match no_proxy.as_deref() {
                Some(value) => match parse_cpp_no_proxy(value) {
                    Some(entries) => entries,
                    None => return CppProxyRoute::Unsupported,
                },
                None => Vec::new(),
            };
            let Ok(url) = reqwest::Url::parse(&url) else {
                return CppProxyRoute::Unsupported;
            };
            if url.scheme() != "http"
                || !url.username().is_empty()
                || url.password().is_some()
                || url.path() != "/"
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return CppProxyRoute::Unsupported;
            }
            let Some(host) = url.host_str() else {
                return CppProxyRoute::Unsupported;
            };
            let host = host
                .strip_prefix('[')
                .and_then(|host| host.strip_suffix(']'))
                .unwrap_or(host)
                .to_string();
            let Some(port) = url.port_or_known_default() else {
                return CppProxyRoute::Unsupported;
            };
            CppProxyRoute::HttpConnect {
                host,
                port,
                credential: outbound
                    .credential
                    .as_ref()
                    .map(|credential| credential.expose().to_string()),
                no_proxy,
            }
        }
    }
}

fn parse_cpp_no_proxy(value: &str) -> Option<Vec<String>> {
    let entries = value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect::<Vec<_>>();
    if entries.len() > 256 || entries.iter().any(|entry| entry.len() > 253) {
        return None;
    }
    let valid_host = |host: &str| {
        let bracketed_ipv6 = host.len() >= 4
            && host.starts_with('[')
            && host.ends_with(']')
            && host.contains(':');
        !host.is_empty()
            && host.len() <= 253
            && !host.starts_with('.')
            && !host.ends_with('.')
            && !host.contains("..")
            && (!host.contains(':') || bracketed_ipv6)
            && host.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(byte, b'.' | b'-' | b'_' | b':' | b'[' | b']')
            })
    };
    entries
        .into_iter()
        .map(|entry| {
            if entry == "*" {
                return Some(entry.to_string());
            }
            if let Some(host) = entry.strip_prefix("*.") {
                return valid_host(host).then(|| entry.to_string());
            }
            if let Some(host) = entry.strip_prefix('.') {
                return valid_host(host).then(|| entry.to_string());
            }
            if let Some(prefix) = entry.strip_suffix(".*") {
                let octets = prefix.split('.').collect::<Vec<_>>();
                return (!prefix.is_empty()
                    && octets.len() <= 3
                    && octets.iter().all(|octet| {
                        !octet.is_empty()
                            && octet.len() <= 3
                            && octet.bytes().all(|byte| byte.is_ascii_digit())
                            && octet.parse::<u16>().is_ok_and(|value| value <= 255)
                    }))
                .then(|| entry.to_string());
            }
            valid_host(entry).then(|| entry.to_string())
        })
        .collect()
}

/// Whether the configured connection uses auth and trust modes supported by C++.
pub(crate) fn supports_cpp_target(state: &AppState, server_id: &str) -> Result<bool, String> {
    let server = state
        .database
        .servers()
        .get(server_id)
        .map_err(|error| error.to_string())?;
    if server.connection.host_certificate_authority.is_some() {
        return Ok(false);
    }
    let Some(identity_id) = server.connection.identity_id.as_deref() else {
        return Ok(false);
    };
    let identity = state
        .database
        .identities()
        .get(identity_id)
        .map_err(|error| error.to_string())?;
    let identity_supported = matches!(
        identity.method.as_str(),
        "password" | "privateKey" | "agent"
    ) || (identity.method == "certificate"
        && (identity
            .certificate_path
            .as_deref()
            .is_some_and(|path| !path.is_empty())
            || identity
                .private_key_path
                .as_deref()
                .is_some_and(|path| !path.is_empty())));
    if !identity_supported {
        return Ok(false);
    }
    if matches!(resolve_cpp_proxy_route(state)?, CppProxyRoute::Unsupported) {
        return Ok(false);
    }
    let pin = state
        .ssh
        .host_key_pin(&server.connection.host, server.connection.port)
        .map_err(|error| error.to_string())?;
    Ok(pin.is_some())
}

impl From<String> for TerminalOpenFailure {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl From<&str> for TerminalOpenFailure {
    fn from(message: &str) -> Self {
        Self::Failed(message.to_string())
    }
}

impl TerminalPty for SshTerminalPty {
    async fn write(&self, data: &[u8]) -> yukinal_terminal::Result<()> {
        if data.len() > 256 * 1024 {
            return Err(yukinal_terminal::TerminalError::Channel(
                "terminal input exceeded 256 KiB".into(),
            ));
        }
        let data = std::str::from_utf8(data).map_err(|_| {
            yukinal_terminal::TerminalError::Channel("terminal input is not valid UTF-8".into())
        })?;
        self.write_frame(&json!({ "type": "write", "data": data }))
            .await
    }

    async fn resize(&self, cols: u16, rows: u16) -> yukinal_terminal::Result<()> {
        self.write_frame(&json!({ "type": "resize", "columns": cols, "rows": rows }))
            .await
    }

    fn events(&self) -> mpsc::Receiver<TerminalPtyEvent> {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .unwrap_or_else(|| {
                let (_sender, receiver) = mpsc::channel(1);
                receiver
            })
    }

    async fn close(&self) -> yukinal_terminal::Result<()> {
        let mut input = self.input.lock().await;
        if !self.closed.swap(true, Ordering::AcqRel) {
            if let Ok(mut frame) = serde_json::to_vec(&json!({ "type": "close" })) {
                frame.push(b'\n');
                let _ = input.write_all(&frame).await;
                let _ = input.flush().await;
            }
        }
        drop(input);
        let mut child = self.child.lock().await;
        if tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .is_err()
        {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        Ok(())
    }
}

impl SshTerminalPty {
    async fn write_frame(&self, message: &Value) -> yukinal_terminal::Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(yukinal_terminal::TerminalError::Channel(
                "terminal process is closed".into(),
            ));
        }
        let mut frame = serde_json::to_vec(message).map_err(|_| {
            yukinal_terminal::TerminalError::Channel("could not encode terminal input".into())
        })?;
        frame.push(b'\n');
        let mut input = self.input.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return Err(yukinal_terminal::TerminalError::Channel(
                "terminal process is closed".into(),
            ));
        }
        input.write_all(&frame).await.map_err(|_| {
            yukinal_terminal::TerminalError::Channel(
                "could not write to the C++ terminal process".into(),
            )
        })?;
        input.flush().await.map_err(|_| {
            yukinal_terminal::TerminalError::Channel(
                "could not flush the C++ terminal process".into(),
            )
        })
    }
}

pub(crate) async fn open_terminal(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    columns: u16,
    rows: u16,
) -> Result<SshTerminalPty, TerminalOpenFailure> {
    if !supports_cpp_target(state, server_id)? {
        return Err(TerminalOpenFailure::Failed(
            "the selected SSH target requires the Rust compatibility path".into(),
        ));
    }
    let target_payload =
        super::sidecar::resolve_ssh_service_target(state, json!({ "serverId": server_id }))?;
    let target = target_payload
        .get("target")
        .filter(|target| target.get("target_id").and_then(Value::as_str) == Some(server_id))
        .cloned()
        .ok_or_else(|| "the desktop returned an invalid SSH terminal target".to_string())?;
    let credentials = target_payload
        .get("credentials")
        .and_then(Value::as_object)
        .ok_or_else(|| "the desktop returned invalid SSH terminal credentials".to_string())?;
    let cwd =
        std::env::current_dir().map_err(|_| "the desktop working directory is unavailable")?;
    let resources = app.path().resource_dir().ok();
    let binary = super::sidecar::resolve_ssh_service_binary(&cwd, resources.as_deref())?
        .ok_or_else(|| "the packaged C++ SSH service executable is unavailable".to_string())?;
    let directory = tempfile::Builder::new()
        .prefix("yukinal-ssh-terminal-")
        .tempdir()
        .map_err(|_| "could not create the temporary SSH terminal directory".to_string())?;
    let config_path = directory.path().join("terminal.json");
    let config = json!({
        "listen_port": 8765,
        "api_token_env": "YUKINAL_SSH_API_TOKEN",
        "targets": [target],
    });
    std::fs::write(
        &config_path,
        serde_json::to_vec(&config)
            .map_err(|_| "could not encode the SSH terminal configuration".to_string())?,
    )
    .map_err(|_| "could not write the SSH terminal configuration".to_string())?;

    let mut command = TokioCommand::new(binary);
    command
        .arg("--config")
        .arg(&config_path)
        .arg("--terminal")
        .arg("--cols")
        .arg(columns.to_string())
        .arg("--rows")
        .arg(rows.to_string())
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        command
            .env("SystemRoot", &system_root)
            .env("WINDIR", system_root);
    }
    for (name, value) in credentials {
        let agent_socket = target
            .get("auth")
            .and_then(|auth| auth.get("type"))
            .and_then(Value::as_str)
            == Some("agent")
            && name == "SSH_AUTH_SOCK";
        if !(agent_socket
            || matches!(
                name.as_str(),
                "YK_SSH_PASSWORD"
                    | "YK_SSH_PRIVATE_KEY"
                    | "YK_SSH_PASSPHRASE"
                    | "YK_SSH_PROXY_CREDENTIAL"
            ))
            || !value.is_string()
        {
            return Err("the desktop returned invalid SSH terminal credentials".into());
        }
        command.env(name, value.as_str().expect("validated string"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.as_std_mut().creation_flags(0x0800_0000);
    }

    let mut child = command
        .spawn()
        .map_err(|_| "could not start the packaged C++ SSH terminal process".to_string())?;
    drop(command);
    drop(target_payload);
    let input = child
        .stdin
        .take()
        .ok_or_else(|| "the C++ SSH terminal process has no input pipe".to_string())?;
    let output = child
        .stdout
        .take()
        .ok_or_else(|| "the C++ SSH terminal process has no output pipe".to_string())?;
    let mut output = BufReader::new(output);
    let mut first_line = String::new();
    let startup =
        tokio::time::timeout(Duration::from_secs(35), output.read_line(&mut first_line)).await;
    match startup {
        Ok(Ok(count)) if count > 0 && first_line.len() <= 32 * 1024 => {}
        Ok(Ok(_)) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err("the C++ SSH terminal process exited during startup".into());
        }
        Ok(Err(_)) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err("could not read the C++ SSH terminal startup response".into());
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err("the C++ SSH terminal did not connect within 35 seconds".into());
        }
    }
    let startup: Value = match serde_json::from_str(first_line.trim_end()) {
        Ok(startup) => startup,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err("the C++ SSH terminal returned an invalid startup response".into());
        }
    };
    if startup.get("type").and_then(Value::as_str) != Some("ready") {
        let message = startup
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("the C++ SSH terminal could not start");
        let authentication_failed =
            startup.get("code").and_then(Value::as_str) == Some("authentication_failed");
        let _ = child.kill().await;
        let _ = child.wait().await;
        return Err(if authentication_failed {
            TerminalOpenFailure::AuthenticationFailed
        } else {
            TerminalOpenFailure::Failed(message.to_string())
        });
    }

    let (sender, receiver) = mpsc::channel(256);
    tauri::async_runtime::spawn(async move {
        let mut line = String::new();
        loop {
            line.clear();
            match output.read_line(&mut line).await {
                Ok(count) if count > 0 && line.len() <= 32 * 1024 => {}
                _ => {
                    let _ = sender.send(TerminalPtyEvent::Closed { code: None }).await;
                    break;
                }
            }
            let message: Value = match serde_json::from_str(line.trim_end()) {
                Ok(message) => message,
                Err(_) => {
                    let _ = sender.send(TerminalPtyEvent::Closed { code: None }).await;
                    break;
                }
            };
            match message.get("type").and_then(Value::as_str) {
                Some("output") => {
                    let decoded = message
                        .get("data")
                        .and_then(Value::as_str)
                        .and_then(|encoded| {
                            base64::engine::general_purpose::STANDARD
                                .decode(encoded)
                                .ok()
                        })
                        .filter(|bytes| bytes.len() <= 8192);
                    let Some(bytes) = decoded else {
                        let _ = sender.send(TerminalPtyEvent::Closed { code: None }).await;
                        break;
                    };
                    if sender.send(TerminalPtyEvent::Output(bytes)).await.is_err() {
                        break;
                    }
                }
                Some("closed") => {
                    let code = message
                        .get("exit_code")
                        .and_then(Value::as_u64)
                        .and_then(|value| u32::try_from(value).ok());
                    let _ = sender.send(TerminalPtyEvent::Closed { code }).await;
                    break;
                }
                Some("error") => {
                    let _ = sender.send(TerminalPtyEvent::Closed { code: None }).await;
                    break;
                }
                _ => {
                    let _ = sender.send(TerminalPtyEvent::Closed { code: None }).await;
                    break;
                }
            }
        }
    });

    Ok(SshTerminalPty {
        input: AsyncMutex::new(input),
        child: AsyncMutex::new(child),
        events: StdMutex::new(Some(receiver)),
        closed: AtomicBool::new(false),
        _config_directory: directory,
    })
}

async fn spawn_sftp_stream_worker(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    mode: &str,
    path: &str,
) -> Result<
    (
        TokioChild,
        ChildStdin,
        BufReader<ChildStdout>,
        tempfile::TempDir,
    ),
    String,
> {
    if !supports_cpp_target(state, server_id)? {
        return Err("the selected SSH target requires the Rust compatibility path".into());
    }
    let target_payload =
        super::sidecar::resolve_ssh_service_target(state, json!({ "serverId": server_id }))?;
    let target = target_payload
        .get("target")
        .filter(|target| target.get("target_id").and_then(Value::as_str) == Some(server_id))
        .cloned()
        .ok_or_else(|| "the desktop returned an invalid SSH transfer target".to_string())?;
    let credentials = target_payload
        .get("credentials")
        .and_then(Value::as_object)
        .ok_or_else(|| "the desktop returned invalid SSH transfer credentials".to_string())?;
    let cwd =
        std::env::current_dir().map_err(|_| "the desktop working directory is unavailable")?;
    let resources = app.path().resource_dir().ok();
    let binary = super::sidecar::resolve_ssh_service_binary(&cwd, resources.as_deref())?
        .ok_or_else(|| "the packaged C++ SSH service executable is unavailable".to_string())?;
    let directory = tempfile::Builder::new()
        .prefix("yukinal-sftp-stream-")
        .tempdir()
        .map_err(|_| "could not create the temporary SFTP directory".to_string())?;
    let config_path = directory.path().join("transfer.json");
    let config = json!({
        "listen_port": 8765,
        "api_token_env": "YUKINAL_SSH_API_TOKEN",
        "targets": [target],
    });
    std::fs::write(
        &config_path,
        serde_json::to_vec(&config)
            .map_err(|_| "could not encode the SFTP transfer configuration".to_string())?,
    )
    .map_err(|_| "could not write the SFTP transfer configuration".to_string())?;

    let mut command = TokioCommand::new(binary);
    command
        .arg("--config")
        .arg(&config_path)
        .arg(mode)
        .arg("--path")
        .arg(path)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        command
            .env("SystemRoot", &system_root)
            .env("WINDIR", system_root);
    }
    for (name, value) in credentials {
        let agent_socket = target
            .get("auth")
            .and_then(|auth| auth.get("type"))
            .and_then(Value::as_str)
            == Some("agent")
            && name == "SSH_AUTH_SOCK";
        if !(agent_socket
            || matches!(
                name.as_str(),
                "YK_SSH_PASSWORD"
                    | "YK_SSH_PRIVATE_KEY"
                    | "YK_SSH_PASSPHRASE"
                    | "YK_SSH_PROXY_CREDENTIAL"
            ))
            || !value.is_string()
        {
            return Err("the desktop returned invalid SSH transfer credentials".into());
        }
        command.env(name, value.as_str().expect("validated string"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.as_std_mut().creation_flags(0x0800_0000);
    }

    let mut child = command
        .spawn()
        .map_err(|_| "could not start the packaged C++ SFTP worker".to_string())?;
    drop(command);
    drop(target_payload);
    let Some(input) = child.stdin.take() else {
        let _ = child.kill().await;
        let _ = child.wait().await;
        return Err("the C++ SFTP worker has no input pipe".into());
    };
    let Some(output) = child.stdout.take() else {
        let _ = child.kill().await;
        let _ = child.wait().await;
        return Err("the C++ SFTP worker has no output pipe".into());
    };
    Ok((child, input, BufReader::new(output), directory))
}

async fn expect_sftp_worker_ready(
    child: &mut TokioChild,
    output: &mut BufReader<ChildStdout>,
) -> Result<(), String> {
    let mut first_line = String::new();
    let startup =
        tokio::time::timeout(Duration::from_secs(35), output.read_line(&mut first_line)).await;
    match startup {
        Ok(Ok(count)) if count > 0 && first_line.len() <= 48 * 1024 => {}
        Ok(Ok(_)) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err("the C++ SFTP worker exited during startup".into());
        }
        Ok(Err(_)) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err("could not read the C++ SFTP startup response".into());
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err("the C++ SFTP worker did not connect within 35 seconds".into());
        }
    }
    let startup: Value = match serde_json::from_str(first_line.trim_end()) {
        Ok(startup) => startup,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err("the C++ SFTP worker returned an invalid startup response".into());
        }
    };
    if startup.get("type").and_then(Value::as_str) == Some("ready") {
        return Ok(());
    }
    let message = startup
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("the C++ SFTP worker could not start")
        .to_string();
    let _ = child.kill().await;
    let _ = child.wait().await;
    Err(message)
}

struct SftpStreamReader {
    receiver: mpsc::Receiver<io::Result<Vec<u8>>>,
    pending: Vec<u8>,
    pending_offset: usize,
}

impl AsyncRead for SftpStreamReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.pending_offset < self.pending.len() {
            let count = (self.pending.len() - self.pending_offset).min(output.remaining());
            output.put_slice(&self.pending[self.pending_offset..self.pending_offset + count]);
            self.pending_offset += count;
            return Poll::Ready(Ok(()));
        }
        self.pending.clear();
        self.pending_offset = 0;
        match self.receiver.poll_recv(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(Ok(())),
            Poll::Ready(Some(Err(error))) => Poll::Ready(Err(error)),
            Poll::Ready(Some(Ok(bytes))) => {
                let count = bytes.len().min(output.remaining());
                output.put_slice(&bytes[..count]);
                if count < bytes.len() {
                    self.pending = bytes;
                    self.pending_offset = count;
                }
                Poll::Ready(Ok(()))
            }
        }
    }
}

struct SftpStreamWriter {
    stream: tokio::io::DuplexStream,
    finish: Option<oneshot::Sender<()>>,
    completion: oneshot::Receiver<Result<(), String>>,
    shutdown_started: bool,
    shutdown_complete: bool,
}

impl AsyncWrite for SftpStreamWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(context, data)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        if !self.shutdown_started {
            match Pin::new(&mut self.stream).poll_shutdown(context) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Ready(Ok(())) => {
                    self.shutdown_started = true;
                    if let Some(finish) = self.finish.take() {
                        let _ = finish.send(());
                    }
                }
            }
        }
        if self.shutdown_complete {
            return Poll::Ready(Ok(()));
        }
        match Pin::new(&mut self.completion).poll(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(Ok(()))) => {
                self.shutdown_complete = true;
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Ok(Err(message))) => Poll::Ready(Err(io::Error::other(message))),
            Poll::Ready(Err(_)) => Poll::Ready(Err(io::Error::other(
                "the C++ SFTP upload worker ended unexpectedly",
            ))),
        }
    }
}

pub(crate) async fn sftp_download_stream(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    path: &str,
) -> Result<TransferReader, String> {
    let (mut child, input, mut output, directory) =
        spawn_sftp_stream_worker(app, state, server_id, "--sftp-download", path).await?;
    drop(input);
    expect_sftp_worker_ready(&mut child, &mut output).await?;
    let (sender, receiver) = mpsc::channel(8);
    tauri::async_runtime::spawn(async move {
        let _config_directory = directory;
        let mut line = String::new();
        loop {
            line.clear();
            let read_result = tokio::select! {
                biased;
                _ = sender.closed() => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    return;
                }
                result = output.read_line(&mut line) => result,
            };
            match read_result {
                Ok(count) if count > 0 && line.len() <= 48 * 1024 => {}
                _ => {
                    let _ = sender
                        .send(Err(io::Error::other(
                            "the C++ SFTP download ended without a final event",
                        )))
                        .await;
                    let _ = child.kill().await;
                    break;
                }
            }
            let message: Value = match serde_json::from_str(line.trim_end()) {
                Ok(message) => message,
                Err(_) => {
                    let _ = sender
                        .send(Err(io::Error::other(
                            "the C++ SFTP download returned invalid framing",
                        )))
                        .await;
                    let _ = child.kill().await;
                    break;
                }
            };
            match message.get("type").and_then(Value::as_str) {
                Some("data") => {
                    let decoded = message
                        .get("data")
                        .and_then(Value::as_str)
                        .and_then(|encoded| {
                            base64::engine::general_purpose::STANDARD
                                .decode(encoded)
                                .ok()
                        })
                        .filter(|bytes| !bytes.is_empty() && bytes.len() <= 32 * 1024);
                    let Some(bytes) = decoded else {
                        let _ = sender
                            .send(Err(io::Error::other(
                                "the C++ SFTP download returned an invalid data chunk",
                            )))
                            .await;
                        let _ = child.kill().await;
                        break;
                    };
                    if sender.send(Ok(bytes)).await.is_err() {
                        let _ = child.kill().await;
                        break;
                    }
                }
                Some("closed") if message.get("exit_code").and_then(Value::as_i64) == Some(0) => {
                    let status = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
                    match status {
                        Ok(Ok(status)) if status.success() => {}
                        _ => {
                            let _ = sender
                                .send(Err(io::Error::other(
                                    "the C++ SFTP download worker did not exit cleanly",
                                )))
                                .await;
                        }
                    }
                    break;
                }
                Some("error") => {
                    let message = message
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("the C++ SFTP download failed");
                    let _ = sender
                        .send(Err(io::Error::other(message.to_string())))
                        .await;
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    break;
                }
                _ => {
                    let _ = sender
                        .send(Err(io::Error::other(
                            "the C++ SFTP download returned an unexpected event",
                        )))
                        .await;
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    break;
                }
            }
        }
    });
    Ok(Box::new(SftpStreamReader {
        receiver,
        pending: Vec::new(),
        pending_offset: 0,
    }))
}

pub(crate) async fn sftp_create_exclusive_stream(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    path: &str,
) -> Result<TransferWriter, String> {
    let (mut child, mut input, mut output, directory) =
        spawn_sftp_stream_worker(app, state, server_id, "--sftp-upload-exclusive", path).await?;
    expect_sftp_worker_ready(&mut child, &mut output).await?;
    let (stream, mut bridge) = tokio::io::duplex(128 * 1024);
    let (finish_sender, finish_receiver) = oneshot::channel();
    let (completion_sender, completion_receiver) = oneshot::channel();
    tauri::async_runtime::spawn(async move {
        let _config_directory = directory;
        let result: Result<(), String> = async {
            let mut buffer = vec![0u8; 32 * 1024];
            loop {
                let count = bridge
                    .read(&mut buffer)
                    .await
                    .map_err(|error| error.to_string())?;
                if count == 0 {
                    finish_receiver.await.map_err(|_| {
                        "the SFTP upload writer was dropped before shutdown".to_string()
                    })?;
                    let mut close_frame = serde_json::to_vec(&json!({ "type": "close" }))
                        .map_err(|_| "could not encode the SFTP close frame".to_string())?;
                    close_frame.push(b'\n');
                    input
                        .write_all(&close_frame)
                        .await
                        .map_err(|_| "could not close the C++ SFTP upload".to_string())?;
                    input
                        .flush()
                        .await
                        .map_err(|_| "could not flush the C++ SFTP close frame".to_string())?;
                    break;
                }
                let frame = json!({
                    "type": "data",
                    "data": base64::engine::general_purpose::STANDARD.encode(&buffer[..count]),
                });
                let mut encoded = serde_json::to_vec(&frame)
                    .map_err(|_| "could not encode an SFTP upload frame".to_string())?;
                encoded.push(b'\n');
                input
                    .write_all(&encoded)
                    .await
                    .map_err(|_| "could not write to the C++ SFTP upload".to_string())?;
                input
                    .flush()
                    .await
                    .map_err(|_| "could not flush the C++ SFTP upload".to_string())?;
            }

            let mut line = String::new();
            loop {
                line.clear();
                match tokio::time::timeout(Duration::from_secs(90), output.read_line(&mut line))
                    .await
                {
                    Ok(Ok(count)) if count > 0 && line.len() <= 48 * 1024 => {}
                    _ => return Err("the C++ SFTP upload did not finish within 90 seconds".into()),
                }
                let message: Value = serde_json::from_str(line.trim_end())
                    .map_err(|_| "the C++ SFTP upload returned invalid framing".to_string())?;
                match message.get("type").and_then(Value::as_str) {
                    Some("closed")
                        if message.get("exit_code").and_then(Value::as_i64) == Some(0) =>
                    {
                        break
                    }
                    Some("error") => {
                        return Err(message
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("the C++ SFTP upload failed")
                            .to_string());
                    }
                    _ => return Err("the C++ SFTP upload returned an unexpected event".into()),
                }
            }
            let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .map_err(|_| "the C++ SFTP upload worker did not exit".to_string())?
                .map_err(|_| "could not wait for the C++ SFTP upload worker".to_string())?;
            if !status.success() {
                return Err("the C++ SFTP upload worker exited unsuccessfully".into());
            }
            Ok(())
        }
        .await;
        if result.is_err() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        let _ = completion_sender.send(result);
    });
    Ok(Box::new(SftpStreamWriter {
        stream,
        finish: Some(finish_sender),
        completion: completion_receiver,
        shutdown_started: false,
        shutdown_complete: false,
    }))
}

pub(crate) async fn sftp_list(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    path: &str,
) -> Result<Vec<(String, String, u64)>, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Entry {
        name: String,
        file_type: String,
        size: u64,
    }

    let output = run_operation(app, state, server_id, "sftp_list", json!({"path": path})).await?;
    let entries: Vec<Entry> = serde_json::from_str(&output).map_err(|_| {
        "the C++ SSH service returned an invalid SFTP directory listing".to_string()
    })?;
    if entries.len() > 4096
        || entries.iter().any(|entry| {
            entry.name.is_empty()
                || entry.name.len() > 4096
                || !matches!(
                    entry.file_type.as_str(),
                    "directory" | "file" | "symlink" | "other"
                )
        })
    {
        return Err("the C++ SSH service returned an invalid SFTP directory listing".into());
    }
    Ok(entries
        .into_iter()
        .map(|entry| (entry.name, entry.file_type, entry.size))
        .collect())
}

pub(crate) async fn server_logs(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    since_seconds: Option<u32>,
    unit: Option<&str>,
) -> Result<String, String> {
    let mut parameters = serde_json::Map::new();
    if let Some(seconds) = since_seconds {
        parameters.insert("since_seconds".into(), json!(seconds));
    }
    if let Some(unit) = unit {
        parameters.insert("unit".into(), json!(unit));
    }
    run_operation(
        app,
        state,
        server_id,
        "server_logs",
        Value::Object(parameters),
    )
    .await
}

pub(crate) async fn sftp_stat(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    path: &str,
) -> Result<RemoteStat, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Stat {
        kind: String,
        size: u64,
        modified: Option<u32>,
    }

    let output = run_operation(app, state, server_id, "sftp_stat", json!({"path": path})).await?;
    let stat: Stat = serde_json::from_str(&output)
        .map_err(|_| "the C++ SSH service returned invalid SFTP metadata".to_string())?;
    let kind = match stat.kind.as_str() {
        "file" => RemoteEntryKind::File,
        "directory" => RemoteEntryKind::Directory,
        "symlink" => RemoteEntryKind::Symlink,
        "other" => RemoteEntryKind::Other,
        _ => return Err("the C++ SSH service returned invalid SFTP metadata".into()),
    };
    Ok(RemoteStat {
        kind,
        size: stat.size,
        modified: stat.modified,
    })
}

pub(crate) async fn sftp_lstat_optional(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    path: &str,
) -> Result<Option<RemoteStat>, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Missing {
        exists: bool,
    }

    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Present {
        exists: bool,
        kind: String,
        size: u64,
        modified: Option<u32>,
    }

    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Lstat {
        Missing(Missing),
        Present(Present),
    }

    let output = run_operation(
        app,
        state,
        server_id,
        "sftp_lstat_optional",
        json!({"path": path}),
    )
    .await?;
    match serde_json::from_str::<Lstat>(&output)
        .map_err(|_| "the C++ SSH service returned invalid optional SFTP metadata".to_string())?
    {
        Lstat::Missing(Missing { exists: false }) => Ok(None),
        Lstat::Missing(Missing { exists: true }) => {
            Err("the C++ SSH service returned incomplete optional SFTP metadata".into())
        }
        Lstat::Present(Present {
            exists: true,
            kind,
            size,
            modified,
        }) => {
            let kind = match kind.as_str() {
                "file" => RemoteEntryKind::File,
                "directory" => RemoteEntryKind::Directory,
                "symlink" => RemoteEntryKind::Symlink,
                "other" => RemoteEntryKind::Other,
                _ => {
                    return Err(
                        "the C++ SSH service returned invalid optional SFTP metadata".into(),
                    )
                }
            };
            Ok(Some(RemoteStat {
                kind,
                size,
                modified,
            }))
        }
        Lstat::Present(Present { exists: false, .. }) => {
            Err("the C++ SSH service returned inconsistent optional SFTP metadata".into())
        }
    }
}

pub(crate) async fn sftp_create_dir(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    path: &str,
) -> Result<(), String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct CreateResult {
        created: bool,
    }

    let output = run_operation(
        app,
        state,
        server_id,
        "sftp_create_dir",
        json!({"path": path}),
    )
    .await?;
    let result: CreateResult = serde_json::from_str(&output)
        .map_err(|_| "the C++ SSH service returned an invalid SFTP directory result".to_string())?;
    if !result.created {
        return Err("the C++ SSH service did not confirm SFTP directory creation".into());
    }
    Ok(())
}

pub(crate) async fn sftp_publish_no_replace(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    source: &str,
    destination: &str,
) -> Result<(), TransferError> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PublishResult {
        published: bool,
    }

    let response = run_operation_response(
        app,
        state,
        server_id,
        "sftp_publish_no_replace",
        json!({"source": source, "destination": destination}),
    )
    .await
    .map_err(|error| match error.code.as_str() {
        "already_exists" => TransferError::AlreadyExists,
        "replace_unsupported" => TransferError::Unsupported(error.message),
        _ => TransferError::Remote(error.message),
    })?;
    validate_operation_response(&response, server_id, "sftp_publish_no_replace")
        .map_err(TransferError::Remote)?;
    let output = response
        .get("stdout")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            TransferError::Remote(
                "the C++ SSH service returned an invalid publication result".into(),
            )
        })?;
    let result: PublishResult = serde_json::from_str(output).map_err(|_| {
        TransferError::Remote("the C++ SSH service returned invalid publication metadata".into())
    })?;
    if !result.published {
        return Err(TransferError::Remote(
            "the C++ SSH service did not confirm no-replace publication".into(),
        ));
    }
    Ok(())
}

pub(crate) async fn sftp_read_bounded(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    path: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    if max_bytes > 1024 * 1024 {
        return Err("the requested SFTP read limit is too large".into());
    }
    let output = run_operation(
        app,
        state,
        server_id,
        "sftp_read",
        json!({"path": path, "max_bytes": max_bytes}),
    )
    .await?;
    decode_sftp_read_output(&output, max_bytes)
}

pub(crate) async fn sftp_write(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    path: &str,
    data: &[u8],
) -> Result<(), String> {
    if data.len() > 512 * 1024 {
        return Err("the requested SFTP write exceeds the 512 KiB limit".into());
    }
    let output = run_operation(
        app,
        state,
        server_id,
        "sftp_write",
        json!({
            "path": path,
            "content_base64": base64::engine::general_purpose::STANDARD.encode(data),
        }),
    )
    .await?;
    validate_sftp_write_output(&output, data.len())
}

pub(crate) async fn sftp_create_exclusive(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    path: &str,
    data: &[u8],
) -> Result<(), String> {
    if data.len() > 1024 * 1024 {
        return Err("the requested SFTP recovery file exceeds the 1 MiB limit".into());
    }
    let output = run_operation(
        app,
        state,
        server_id,
        "sftp_create_exclusive",
        json!({
            "path": path,
            "content_base64": base64::engine::general_purpose::STANDARD.encode(data),
        }),
    )
    .await?;
    validate_sftp_write_output(&output, data.len())
}

fn validate_sftp_write_output(output: &str, expected_bytes: usize) -> Result<(), String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct WriteResult {
        bytes_written: usize,
    }

    let result: WriteResult = serde_json::from_str(output)
        .map_err(|_| "the C++ SSH service returned an invalid SFTP write result".to_string())?;
    if result.bytes_written != expected_bytes {
        return Err("the C++ SSH service reported an unexpected SFTP write length".into());
    }
    Ok(())
}

pub(crate) async fn sftp_remove_file(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    path: &str,
) -> Result<(), String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct RemoveResult {
        removed: bool,
    }

    let output = run_operation(
        app,
        state,
        server_id,
        "sftp_remove_file",
        json!({"path": path}),
    )
    .await?;
    let result: RemoveResult = serde_json::from_str(&output)
        .map_err(|_| "the C++ SSH service returned an invalid SFTP removal result".to_string())?;
    if !result.removed {
        return Err("the C++ SSH service did not confirm SFTP file removal".into());
    }
    Ok(())
}

pub(crate) async fn sftp_link_count(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    path: &str,
) -> Result<Option<u64>, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LinkCount {
        link_count: Option<u64>,
    }

    let output = run_operation(
        app,
        state,
        server_id,
        "sftp_link_count",
        json!({"path": path}),
    )
    .await?;
    let result: LinkCount = serde_json::from_str(&output)
        .map_err(|_| "the C++ SSH service returned invalid SFTP link metadata".to_string())?;
    Ok(result.link_count)
}

pub(crate) async fn sftp_replace_guarded(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    path: &str,
    guard: &ReplaceGuard,
    data: &[u8],
) -> Result<ReplacedFile, ReplaceError> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Replacement {
        size: u64,
        modified: Option<u32>,
    }

    let digest = guard
        .content_digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let response = run_operation_response(
        app,
        state,
        server_id,
        "sftp_replace_guarded",
        json!({
            "path": path,
            "expected_size": guard.size,
            "expected_modified": guard.modified,
            "expected_digest": digest,
            "content_base64": base64::engine::general_purpose::STANDARD.encode(data),
        }),
    )
    .await
    .map_err(|error| match error.code.as_str() {
        "concurrent_change" => ReplaceError::ConcurrentChange(error.message),
        "replace_unsupported" => ReplaceError::Unsupported(error.message),
        "metadata_not_preserved" => ReplaceError::MetadataNotPreserved {
            message: error.message,
            missing: error.missing,
        },
        _ => ReplaceError::Transport(TransportError::new(error.message)),
    })?;
    validate_operation_response(&response, server_id, "sftp_replace_guarded")
        .map_err(|error| ReplaceError::Transport(TransportError::new(error)))?;
    let output = response
        .get("stdout")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ReplaceError::Transport(TransportError::new(
                "the C++ SSH service returned an invalid replacement result",
            ))
        })?;
    let replacement: Replacement = serde_json::from_str(output).map_err(|_| {
        ReplaceError::Transport(TransportError::new(
            "the C++ SSH service returned invalid replacement metadata",
        ))
    })?;
    if replacement.size != data.len() as u64 {
        return Err(ReplaceError::ConcurrentChange(
            "the published file size does not match the requested replacement".into(),
        ));
    }
    Ok(ReplacedFile {
        size: replacement.size,
        modified: replacement.modified,
    })
}

fn decode_sftp_read_output(output: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(output.as_bytes())
        .map_err(|_| "the C++ SSH service returned invalid file contents".to_string())?;
    if bytes.len() > max_bytes.saturating_add(1) {
        return Err("the C++ SSH service exceeded the requested SFTP read limit".into());
    }
    Ok(bytes)
}

async fn run_operation(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    operation: &str,
    parameters: Value,
) -> Result<String, String> {
    run_operation_with_status(app, state, server_id, operation, parameters, false)
        .await
        .map(|(_, stdout)| stdout)
}

async fn run_operation_with_status(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    operation: &str,
    parameters: Value,
    allow_nonzero_exit_code: bool,
) -> Result<(i32, String), String> {
    let response = run_operation_response(app, state, server_id, operation, parameters)
        .await
        .map_err(|error| error.message)?;
    validate_operation_response_with_exit_code(
        &response,
        server_id,
        operation,
        allow_nonzero_exit_code,
    )?;
    let exit_code = response
        .get("exit_code")
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok())
        .ok_or_else(|| "the C++ SSH service returned an invalid operation exit code")?;
    let stdout = response
        .get("stdout")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "the C++ SSH service returned an invalid operation result")?;
    Ok((exit_code, stdout))
}

struct SshServiceFailure {
    code: String,
    message: String,
    missing: Vec<String>,
}

impl From<String> for SshServiceFailure {
    fn from(message: String) -> Self {
        Self {
            code: "transport_error".into(),
            message,
            missing: Vec::new(),
        }
    }
}

impl From<&str> for SshServiceFailure {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

async fn run_operation_response(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
    operation: &str,
    parameters: Value,
) -> Result<Value, SshServiceFailure> {
    if !supports_cpp_target(state, server_id)? {
        return Err("the selected SSH target requires the Rust compatibility path".into());
    }
    let target_payload =
        super::sidecar::resolve_ssh_service_target(state, json!({ "serverId": server_id }))?;
    run_operation_response_with_target(app, server_id, operation, parameters, target_payload).await
}

async fn run_operation_response_with_target(
    app: &AppHandle,
    server_id: &str,
    operation: &str,
    parameters: Value,
    target_payload: Value,
) -> Result<Value, SshServiceFailure> {
    let target = target_payload
        .get("target")
        .filter(|target| target.get("target_id").and_then(Value::as_str) == Some(server_id))
        .cloned()
        .ok_or_else(|| "the desktop returned an invalid SSH target".to_string())?;
    let credentials = target_payload
        .get("credentials")
        .and_then(Value::as_object)
        .ok_or_else(|| "the desktop returned invalid SSH credentials".to_string())?;

    let cwd =
        std::env::current_dir().map_err(|_| "the desktop working directory is unavailable")?;
    let resources = app.path().resource_dir().ok();
    let binary = super::sidecar::resolve_ssh_service_binary(&cwd, resources.as_deref())?
        .ok_or_else(|| "the packaged C++ SSH service executable is unavailable".to_string())?;
    let port = unused_loopback_port()?;
    let token = api_token()?;
    let directory = tempfile::Builder::new()
        .prefix("yukinal-ssh-service-")
        .tempdir()
        .map_err(|_| "could not create the temporary SSH service directory".to_string())?;
    let config_path = directory.path().join("service.json");
    let config = json!({
        "listen_port": port,
        "api_token_env": "YUKINAL_SSH_API_TOKEN",
        "targets": [target],
    });
    let encoded = serde_json::to_vec(&config)
        .map_err(|_| "could not encode the temporary SSH service configuration".to_string())?;
    std::fs::write(&config_path, encoded)
        .map_err(|_| "could not write the temporary SSH service configuration".to_string())?;

    let mut command = Command::new(binary);
    command
        .arg("--config")
        .arg(&config_path)
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .env_clear()
        .env("YUKINAL_SSH_API_TOKEN", &token)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        command
            .env("SystemRoot", &system_root)
            .env("WINDIR", system_root);
    }
    for (name, value) in credentials {
        let agent_socket = target
            .get("auth")
            .and_then(|auth| auth.get("type"))
            .and_then(Value::as_str)
            == Some("agent")
            && name == "SSH_AUTH_SOCK";
        if !(agent_socket
            || matches!(
                name.as_str(),
                "YK_SSH_PASSWORD"
                    | "YK_SSH_PRIVATE_KEY"
                    | "YK_SSH_PASSPHRASE"
                    | "YK_SSH_PROXY_CREDENTIAL"
            ))
            || !value.is_string()
        {
            return Err("the desktop returned invalid SSH credentials".into());
        }
        command.env(name, value.as_str().expect("validated string"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }

    let child = command
        .spawn()
        .map_err(|_| "could not start the packaged C++ SSH service".to_string())?;
    drop(command);
    drop(target_payload);
    let mut child = ServiceChild(child);
    request_operation(port, &token, server_id, operation, parameters, &mut child.0).await
}

fn validate_operation_response(
    response: &Value,
    server_id: &str,
    operation: &str,
) -> Result<(), String> {
    validate_operation_response_with_exit_code(response, server_id, operation, false)
}

fn validate_operation_response_with_exit_code(
    response: &Value,
    server_id: &str,
    operation: &str,
    allow_nonzero_exit_code: bool,
) -> Result<(), String> {
    let exit_code = response.get("exit_code").and_then(Value::as_i64);
    if response.get("status").and_then(Value::as_str) != Some("succeeded")
        || response.get("target_id").and_then(Value::as_str) != Some(server_id)
        || response.get("operation").and_then(Value::as_str) != Some(operation)
        || exit_code.is_none()
        || (!allow_nonzero_exit_code && exit_code != Some(0))
        || response.get("stdout").and_then(Value::as_str).is_none()
        || response.get("stderr").and_then(Value::as_str).is_none()
        || response
            .get("duration_ms")
            .and_then(Value::as_u64)
            .is_none()
        || response.get("output_truncated").and_then(Value::as_bool) != Some(false)
        || response.get("error") != Some(&Value::Null)
    {
        return Err("the C++ SSH service returned an invalid operation result".into());
    }
    Ok(())
}

struct ServiceChild(Child);

impl Drop for ServiceChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

async fn request_operation(
    port: u16,
    token: &str,
    server_id: &str,
    operation: &str,
    parameters: Value,
    child: &mut std::process::Child,
) -> Result<Value, SshServiceFailure> {
    let base_url = format!("http://127.0.0.1:{port}");
    let timeout_seconds = match operation {
        "system_snapshot" | "docker_restart" => 30,
        "systemd_restart" => parameters
            .get("timeout_seconds")
            .and_then(Value::as_u64)
            .filter(|seconds| (1..=120).contains(seconds))
            .unwrap_or(10),
        "package_install" => parameters
            .get("timeout_seconds")
            .and_then(Value::as_u64)
            .filter(|seconds| (1..=600).contains(seconds))
            .unwrap_or(10),
        _ => 10,
    };
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(1))
        .timeout(Duration::from_secs(timeout_seconds + 2))
        .build()
        .map_err(|_| "could not create the local SSH service client".to_string())?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    loop {
        if child
            .try_wait()
            .map_err(|_| "the C++ SSH service process could not be inspected".to_string())?
            .is_some()
        {
            return Err("the C++ SSH service exited during startup".into());
        }
        if let Ok(response) = client.get(format!("{base_url}/health")).send().await {
            if response.status().is_success()
                && response
                    .content_length()
                    .is_none_or(|length| length <= 4096)
                && response
                    .bytes()
                    .await
                    .ok()
                    .filter(|body| body.len() <= 4096)
                    .and_then(|body| serde_json::from_slice::<Value>(&body).ok())
                    .and_then(|body| {
                        body.get("status")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .as_deref()
                    == Some("healthy")
            {
                break;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("the C++ SSH service did not become healthy".into());
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }

    let request_body = serde_json::to_vec(&json!({
        "target_id": server_id,
        "operation": operation,
        "parameters": parameters,
        "timeout_seconds": timeout_seconds,
    }))
    .map_err(|_| "could not encode the local SSH service request".to_string())?;
    let response = client
        .post(format!("{base_url}/v1/ssh/exec"))
        .bearer_auth(token)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(request_body)
        .send()
        .await
        .map_err(|_| "the C++ SSH service request failed".to_string())?;
    let status = response.status();
    const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err("the C++ SSH service response exceeded the size limit".into());
    }
    let response_body = response
        .bytes()
        .await
        .map_err(|_| "the C++ SSH service returned invalid JSON".to_string())?;
    if response_body.len() > MAX_RESPONSE_BYTES {
        return Err("the C++ SSH service response exceeded the size limit".into());
    }
    let body: Value = serde_json::from_slice(&response_body)
        .map_err(|_| "the C++ SSH service returned invalid JSON".to_string())?;
    if !status.is_success() {
        let error = body.get("error");
        let message = error
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("the C++ SSH service rejected the request");
        let code = error
            .and_then(|error| error.get("code"))
            .and_then(Value::as_str)
            .unwrap_or("ssh_error")
            .to_owned();
        let missing = error
            .and_then(|error| error.get("missing"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|name| matches!(*name, "mode" | "mtime" | "owner" | "group"))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        return Err(SshServiceFailure {
            code,
            message: message.to_owned(),
            missing,
        });
    }
    Ok(body)
}

fn unused_loopback_port() -> Result<u16, String> {
    TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .map_err(|_| "could not allocate a local SSH service port".to_string())
}

fn api_token() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "could not generate a local SSH service token".to_string())?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::{
        cpp_proxy_route, decode_sftp_read_output, valid_host_key_fingerprint, CppProxyRoute,
    };
    use yukinal_net::{NetworkProxy, OutboundProxy, ProxyCredential, ProxySource};

    fn proxy_route(url: &str, no_proxy: Option<&str>) -> CppProxyRoute {
        cpp_proxy_route(OutboundProxy {
            proxy: NetworkProxy::Proxy {
                url: url.to_string(),
                no_proxy: no_proxy.map(str::to_string),
                source: ProxySource::EnvironmentVariable,
            },
            credential: Some(
                ProxyCredential::new("proxy-user:proxy-password".to_string())
                    .expect("valid proxy credential"),
            ),
        })
    }

    #[test]
    fn cpp_proxy_route_accepts_plain_http_and_supported_bypass_rules() {
        assert!(matches!(
            proxy_route("http://proxy.example.test:8080", None),
            CppProxyRoute::HttpConnect { host, port: 8080, credential: Some(value), .. }
                if host == "proxy.example.test" && value == "proxy-user:proxy-password"
        ));
        assert!(matches!(
            proxy_route("http://[::1]:8080", Some("  ")),
            CppProxyRoute::HttpConnect { host, port: 8080, .. } if host == "::1"
        ));
        assert!(matches!(
            proxy_route("https://proxy.example.test:8443", None),
            CppProxyRoute::Unsupported
        ));
        assert!(matches!(
            proxy_route("http://proxy.example.test:8080", Some("localhost")),
            CppProxyRoute::HttpConnect { no_proxy, .. }
                if no_proxy.len() == 1 && no_proxy[0] == "localhost"
        ));
        assert!(matches!(
            proxy_route("http://proxy.example.test:8080", Some("10.0.0.0/8")),
            CppProxyRoute::Unsupported
        ));
        assert!(matches!(
            proxy_route("http://proxy.example.test:8080", Some("*.corp,10.*,localhost")),
            CppProxyRoute::HttpConnect { no_proxy, .. } if no_proxy.len() == 3
        ));
        assert!(matches!(
            proxy_route("http://proxy.example.test:8080", Some("192.168.999.*")),
            CppProxyRoute::Unsupported
        ));
    }

    #[test]
    fn bounded_sftp_read_decodes_binary_and_allows_one_truncation_byte() {
        assert_eq!(
            decode_sftp_read_output("YWxwaGEA", 5).expect("valid bounded response"),
            b"alpha\0"
        );
        assert!(decode_sftp_read_output("YWxwaGEAYg==", 5).is_err());
        assert!(decode_sftp_read_output("not base64", 5).is_err());
    }

    #[test]
    fn host_key_probe_requires_an_openssh_sha256_fingerprint() {
        assert!(valid_host_key_fingerprint(
            "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        ));
        assert!(!valid_host_key_fingerprint("SHA256:short"));
        assert!(!valid_host_key_fingerprint(
            "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
        ));
    }
}
