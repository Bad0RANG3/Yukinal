//! Short-lived desktop bridge to the native C++ SSH service.
//!
//! Each desktop request gets a one-target allowlist and credentials passed only in the child
//! environment. The temporary JSON contains target metadata and environment-variable names.

use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use base64::Engine as _;
use ring::rand::SecureRandom;
use serde_json::{Value, json};
use tauri::{AppHandle, Manager};

use crate::state::AppState;

pub(crate) async fn service_inventory(
    app: &AppHandle,
    state: &AppState,
    server_id: &str,
) -> Result<String, String> {
    let target_payload =
        super::sidecar::resolve_ssh_service_target(state, json!({ "serverId": server_id }))?;
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
        if !matches!(
            name.as_str(),
            "YK_SSH_PASSWORD" | "YK_SSH_PRIVATE_KEY" | "YK_SSH_PASSPHRASE"
        ) || !value.is_string()
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
    request_service_inventory(port, &token, server_id, &mut child.0).await
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

async fn request_service_inventory(
    port: u16,
    token: &str,
    server_id: &str,
    child: &mut std::process::Child,
) -> Result<String, String> {
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
        "operation": "service_inventory",
        "parameters": {},
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
    if response
        .content_length()
        .is_some_and(|length| length > 256 * 1024)
    {
        return Err("the C++ SSH service response exceeded the size limit".into());
    }
    let response_body = response
        .bytes()
        .await
        .map_err(|_| "the C++ SSH service returned invalid JSON".to_string())?;
    if response_body.len() > 256 * 1024 {
        return Err("the C++ SSH service response exceeded the size limit".into());
    }
    let body: Value = serde_json::from_slice(&response_body)
        .map_err(|_| "the C++ SSH service returned invalid JSON".to_string())?;
    if !status.is_success() {
        let message = body
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("the C++ SSH service rejected the request");
        return Err(message.to_string());
    }
    if body.get("status").and_then(Value::as_str) != Some("succeeded")
        || body.get("target_id").and_then(Value::as_str) != Some(server_id)
        || body.get("operation").and_then(Value::as_str) != Some("service_inventory")
        || body.get("exit_code").and_then(Value::as_i64) != Some(0)
        || body.get("stdout").and_then(Value::as_str).is_none()
        || body.get("stderr").and_then(Value::as_str).is_none()
        || body.get("duration_ms").and_then(Value::as_u64).is_none()
        || body.get("output_truncated").and_then(Value::as_bool) != Some(false)
        || body.get("error") != Some(&Value::Null)
    {
        return Err("the C++ SSH service returned an invalid inventory result".into());
    }
    body.get("stdout")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "the C++ SSH service returned an invalid inventory result".into())
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
