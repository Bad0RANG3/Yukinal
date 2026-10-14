//! Short-lived desktop bridge to the native C++ SSH service.
//!
//! Each desktop request gets a one-target allowlist and credentials passed only in the child
//! environment. The temporary JSON contains target metadata and environment-variable names.

use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use base64::Engine as _;
use ring::rand::SecureRandom;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child as TokioChild, ChildStdin, Command as TokioCommand};
use tokio::sync::{mpsc, Mutex as AsyncMutex};
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

pub(crate) async fn probe_host_key(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
) -> Result<Option<String>, String> {
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
    if !matches!(
        identity.method.as_str(),
        "password" | "privateKey" | "agent"
    ) {
        return Ok(false);
    }
    let proxy =
        super::network::resolve_outbound_proxy(&state.database, state.credentials.as_ref())?;
    if proxy.proxy != yukinal_net::NetworkProxy::Direct {
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
                "YK_SSH_PASSWORD" | "YK_SSH_PRIVATE_KEY" | "YK_SSH_PASSPHRASE"
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
    let response = run_operation_response(app, state, server_id, operation, parameters)
        .await
        .map_err(|error| error.message)?;
    validate_operation_response(&response, server_id, operation)?;
    response
        .get("stdout")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "the C++ SSH service returned an invalid operation result".into())
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
                "YK_SSH_PASSWORD" | "YK_SSH_PRIVATE_KEY" | "YK_SSH_PASSPHRASE"
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
    if response.get("status").and_then(Value::as_str) != Some("succeeded")
        || response.get("target_id").and_then(Value::as_str) != Some(server_id)
        || response.get("operation").and_then(Value::as_str) != Some(operation)
        || response.get("exit_code").and_then(Value::as_i64) != Some(0)
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
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(1))
        .timeout(Duration::from_secs(12))
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
        "timeout_seconds": 10,
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
    use super::{decode_sftp_read_output, valid_host_key_fingerprint};

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
