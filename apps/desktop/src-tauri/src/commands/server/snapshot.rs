use serde::Deserialize;
use serde_json::Value;
use yukinal_database::models::{HealthState, ServerCapabilities, ServerSnapshot};

const MAX_NORMALIZED_SERVER_SNAPSHOT_BYTES: usize = 512 * 1024;
const COLLECTOR_IDS: &[&str] = &["os", "cpu", "memory", "uptime", "disk", "network", "docker"];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NormalizedSnapshotData {
    health: HealthState,
    os: Option<SnapshotOs>,
    cpu: Option<SnapshotCpu>,
    memory: Option<SnapshotMemory>,
    disks: Option<Vec<SnapshotDisk>>,
    uptime_seconds: Option<u64>,
    network: Option<Vec<SnapshotNetwork>>,
    docker: Option<SnapshotDocker>,
    capabilities: SnapshotCapabilities,
    collectors: Vec<SnapshotCollector>,
}

#[derive(Debug, serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotOs {
    distribution: String,
    version: String,
    hostname: String,
    kernel: String,
    arch: String,
}

#[derive(Debug, serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotCpu {
    model: String,
    cores: u32,
    usage_percent: f64,
    load_average: [f64; 3],
}

#[derive(Debug, serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotMemory {
    total_bytes: u64,
    used_bytes: u64,
    available_bytes: u64,
    usage_percent: f64,
}

#[derive(Debug, serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotDisk {
    device: String,
    mount_point: String,
    total_bytes: u64,
    used_bytes: u64,
    usage_percent: f64,
}

#[derive(Debug, serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotNetwork {
    name: String,
    rx_bytes: u64,
    tx_bytes: u64,
}

#[derive(Debug, serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotDocker {
    available: bool,
    containers: Vec<SnapshotContainer>,
}

#[derive(Debug, serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotContainer {
    name: String,
    image: String,
    state: String,
    status: String,
    restart_count: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotCapabilities {
    linux: bool,
    docker: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotCollector {
    collector_id: String,
    ok: bool,
    error: Option<String>,
}

pub(crate) fn server_snapshot_from_normalized_data(
    server_id: &str,
    collected_at: &str,
    value: &Value,
) -> Result<ServerSnapshot, String> {
    if serde_json::to_vec(value)
        .map(|bytes| bytes.len() > MAX_NORMALIZED_SERVER_SNAPSHOT_BYTES)
        .unwrap_or(true)
    {
        return Err("server.info snapshot data exceeds its bounds".into());
    }
    let data = serde_json::from_value::<NormalizedSnapshotData>(value.clone())
        .map_err(|_| "server.info snapshot data is invalid".to_string())?;
    if data.collectors.len() != COLLECTOR_IDS.len()
        || data
            .collectors
            .iter()
            .zip(COLLECTOR_IDS)
            .any(|(sample, expected)| {
                sample.collector_id != *expected
                    || sample.ok == sample.error.is_some()
                    || sample
                        .error
                        .as_ref()
                        .is_some_and(|error| error.len() > 64 * 1024)
            })
    {
        return Err("server.info snapshot collector set is invalid".into());
    }

    let capabilities = ServerCapabilities::default()
        .with("linux", data.capabilities.linux)
        .with("docker", data.capabilities.docker);
    let collectors = data
        .collectors
        .into_iter()
        .map(|sample| yukinal_database::models::CollectorSample {
            collector_id: sample.collector_id,
            collected_at: collected_at.to_string(),
            ok: sample.ok,
            error: sample.error,
        })
        .collect();
    Ok(ServerSnapshot {
        id: format!("snap_{server_id}_{}", yukinal_time::now_epoch_seconds()),
        server_id: server_id.to_string(),
        collected_at: collected_at.to_string(),
        health: data.health,
        os: section_value(data.os)?,
        cpu: section_value(data.cpu)?,
        memory: section_value(data.memory)?,
        disks: section_value(data.disks)?,
        uptime_seconds: data.uptime_seconds,
        network: section_value(data.network)?,
        docker: section_value(data.docker)?,
        capabilities,
        collectors: Some(collectors),
    })
}

fn section_value<T: serde::Serialize>(value: Option<T>) -> Result<Option<Value>, String> {
    value
        .map(serde_json::to_value)
        .transpose()
        .map_err(|error| error.to_string())
}
