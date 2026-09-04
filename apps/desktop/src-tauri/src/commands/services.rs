//! Read-only remote service discovery over the already authenticated SSH session.

use std::time::Duration;

use serde::Serialize;
use tauri::State;
use tokio_util::sync::CancellationToken;
use yukinal_ssh::SshBackend;

use crate::commands::terminal::ensure_session;
use crate::state::AppState;

const SERVICE_DISCOVERY_COMMAND: &str = r#"if command -v systemctl >/dev/null 2>&1; then systemctl list-units --type=service --all --no-legend --no-pager --plain 2>/dev/null; if [ $? -eq 0 ]; then printf '\n__YUKINAL_SOURCE__=systemd\n'; exit 0; fi; fi; if command -v docker >/dev/null 2>&1; then docker ps -a --format '{{.Names}}\t{{.State}}\t{{.Status}}\t{{.Image}}' 2>/dev/null; if [ $? -eq 0 ]; then printf '\n__YUKINAL_SOURCE__=docker\n'; exit 0; fi; fi; printf '__YUKINAL_SOURCE__=unavailable\n'"#;
const SOURCE_PREFIX: &str = "__YUKINAL_SOURCE__=";
const MAX_SERVICES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceState {
    Running,
    Stopped,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceSource {
    Systemd,
    Docker,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerService {
    pub name: String,
    pub state: ServiceState,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerServicesResponse {
    pub source: ServiceSource,
    pub services: Vec<ServerService>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `server_services`: connect if necessary, then run one fixed read-only probe.
#[tauri::command]
pub async fn server_services(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<ServerServicesResponse, String> {
    ensure_session(&state, &server_id).await?;
    let session = state
        .terminals
        .cached_session(&server_id)
        .map_err(|error| error.to_string())?;
    let output = state
        .ssh
        .execute(
            &session,
            SERVICE_DISCOVERY_COMMAND,
            Some(Duration::from_secs(10)),
            &CancellationToken::new(),
        )
        .await
        .map_err(|error| error.to_string())?;

    parse_services_output(&output.stdout_lossy())
        .map_err(|error| format!("service discovery returned an invalid response: {error}"))
}

fn parse_services_output(raw: &str) -> Result<ServerServicesResponse, String> {
    let source = raw
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(SOURCE_PREFIX))
        .map(parse_source)
        .transpose()?
        .ok_or_else(|| "missing service source marker".to_string())?;

    let services = match source {
        ServiceSource::Systemd => parse_systemd_services(raw),
        ServiceSource::Docker => parse_docker_services(raw),
        ServiceSource::Unavailable => Vec::new(),
    };
    let message = (source == ServiceSource::Unavailable)
        .then(|| "未检测到可读取的 systemd 或 Docker 服务管理器".to_string());
    Ok(ServerServicesResponse {
        source,
        services,
        message,
    })
}

fn parse_source(source: &str) -> Result<ServiceSource, String> {
    match source {
        "systemd" => Ok(ServiceSource::Systemd),
        "docker" => Ok(ServiceSource::Docker),
        "unavailable" => Ok(ServiceSource::Unavailable),
        other => Err(format!("unknown service source `{other}`")),
    }
}

fn parse_systemd_services(raw: &str) -> Vec<ServerService> {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with(SOURCE_PREFIX))
        .filter_map(parse_systemd_line)
        .take(MAX_SERVICES)
        .collect()
}

fn parse_systemd_line(line: &str) -> Option<ServerService> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    let offset = usize::from(fields.first() == Some(&"●"));
    if fields.len() < offset + 4 {
        return None;
    }
    let name = fields[offset].to_string();
    let active = fields[offset + 2];
    let sub = fields[offset + 3];
    let description = (fields.len() > offset + 4).then(|| fields[offset + 4..].join(" "));
    Some(ServerService {
        name,
        state: state_from_systemd(active, sub),
        status: format!("{active}/{sub}"),
        description,
    })
}

fn state_from_systemd(active: &str, sub: &str) -> ServiceState {
    if active == "failed" || sub == "failed" {
        ServiceState::Failed
    } else if active == "active" && sub == "running" {
        ServiceState::Running
    } else if matches!(active, "inactive" | "deactivating") || matches!(sub, "dead" | "exited") {
        ServiceState::Stopped
    } else {
        ServiceState::Unknown
    }
}

fn parse_docker_services(raw: &str) -> Vec<ServerService> {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with(SOURCE_PREFIX))
        .filter_map(parse_docker_line)
        .take(MAX_SERVICES)
        .collect()
}

fn parse_docker_line(line: &str) -> Option<ServerService> {
    let mut fields = line.splitn(4, '\t');
    let name = fields.next()?.trim();
    let state = fields.next()?.trim();
    let status = fields.next()?.trim();
    let description = fields.next()?.trim();
    if name.is_empty() || state.is_empty() || status.is_empty() {
        return None;
    }
    Some(ServerService {
        name: name.to_string(),
        state: state_from_docker(state),
        status: status.to_string(),
        description: (!description.is_empty()).then(|| description.to_string()),
    })
}

fn state_from_docker(state: &str) -> ServiceState {
    match state {
        "running" | "restarting" => ServiceState::Running,
        "dead" => ServiceState::Failed,
        "created" | "exited" | "paused" | "removing" => ServiceState::Stopped,
        _ => ServiceState::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        parse_services_output, ServerService, ServerServicesResponse, ServiceSource, ServiceState,
    };

    const FIXTURE: &str =
        include_str!("../../../../../packages/shared/fixtures/ipc/server_services.json");

    #[test]
    fn parses_systemd_rows_and_maps_states() {
        let response = parse_services_output(concat!(
            "sshd.service loaded active running OpenSSH server daemon\n",
            "nginx.service loaded active running A high performance web server\n",
            "broken.service loaded failed failed Broken service\n",
            "stopped.service loaded inactive dead Stopped service\n",
            "__YUKINAL_SOURCE__=systemd\n",
        ))
        .expect("systemd output");

        assert_eq!(response.source, ServiceSource::Systemd);
        assert_eq!(response.services.len(), 4);
        assert_eq!(response.services[0].name, "sshd.service");
        assert_eq!(response.services[0].state, ServiceState::Running);
        assert_eq!(response.services[0].status, "active/running");
        assert_eq!(
            response.services[0].description.as_deref(),
            Some("OpenSSH server daemon")
        );
        assert_eq!(response.services[2].state, ServiceState::Failed);
        assert_eq!(response.services[3].state, ServiceState::Stopped);
    }

    #[test]
    fn parses_docker_rows_without_splitting_status_spaces() {
        let response = parse_services_output(concat!(
            "web\trunning\tUp 3 hours\tnginx:1.27\n",
            "db\texited\tExited (0) 2 days ago\tpostgres:16\n",
            "__YUKINAL_SOURCE__=docker\n",
        ))
        .expect("docker output");

        assert_eq!(response.source, ServiceSource::Docker);
        assert_eq!(response.services.len(), 2);
        assert_eq!(response.services[0].state, ServiceState::Running);
        assert_eq!(response.services[0].status, "Up 3 hours");
        assert_eq!(
            response.services[0].description.as_deref(),
            Some("nginx:1.27")
        );
        assert_eq!(response.services[1].state, ServiceState::Stopped);
    }

    #[test]
    fn serializes_to_the_shared_contract_fixture() {
        let actual = serde_json::to_value(ServerServicesResponse {
            source: ServiceSource::Systemd,
            services: vec![
                ServerService {
                    name: "sshd.service".into(),
                    state: ServiceState::Running,
                    status: "active/running".into(),
                    description: Some("OpenSSH server daemon".into()),
                },
                ServerService {
                    name: "nginx.service".into(),
                    state: ServiceState::Failed,
                    status: "failed/failed".into(),
                    description: Some("A high performance web server".into()),
                },
            ],
            message: None,
        })
        .expect("serialize");
        let expected: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture");
        assert_eq!(actual, expected);
    }
}
