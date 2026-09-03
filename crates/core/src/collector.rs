//! Collector → snapshot 装配：把采集器输出拼成 `snapshots` 行（camelCase 线形），
//! 健康度由 `health.rs`（与 shared 同一份阈值）计算。

use std::sync::Arc;

use yukinal_collector::{CollectedData, CollectorContext, CollectorEngine, CollectorSample};
use yukinal_database::models::{HealthState, ServerCapabilities, ServerSnapshot};
use yukinal_ssh::{RusshBackend, Session};

use crate::health::{overall, HealthClass};

/// 在 `server_id` 上跑一次完整采集并组装快照。`collected_at` 由调用方给
/// （与 activities/snapshots 行时间对齐，调用方负责）。
pub async fn collect_snapshot(
    ssh: &Arc<RusshBackend>,
    session: &Session,
    server_id: &str,
    collected_at: &str,
) -> yukinal_collector::Result<(ServerSnapshot, Vec<CollectorSample>)> {
    let engine = CollectorEngine::with_mvp();
    let context = CollectorContext::new(
        server_id,
        yukinal_collector::runners::ssh(Arc::clone(ssh), session.clone()),
    );
    engine.detect_all(&context).await?;
    let samples = engine.collect_all(&context, collected_at).await;

    let capabilities = capabilities_from(&context);
    let snapshot = assemble(server_id, collected_at, &samples, capabilities);
    Ok((
        snapshot,
        samples.iter().map(|(sample, _)| sample.clone()).collect(),
    ))
}

fn capabilities_from(context: &CollectorContext) -> ServerCapabilities {
    let mut caps = ServerCapabilities::default();
    if let Ok(entries) = context.capabilities.lock() {
        for (key, present) in entries.iter() {
            caps = caps.with(key, *present);
        }
    }
    caps
}

fn assemble(
    server_id: &str,
    collected_at: &str,
    samples: &[(CollectorSample, Option<CollectedData>)],
    capabilities: ServerCapabilities,
) -> ServerSnapshot {
    let mut os = None;
    let mut cpu = None;
    let mut memory = None;
    let mut disks = None;
    let mut uptime_seconds = None;
    let mut network = None;
    let mut docker = None;

    for (_, data) in samples {
        let Some(data) = data else { continue };
        match data {
            CollectedData::Os(value) => os = serde_json::to_value(value).ok(),
            CollectedData::Cpu(value) => cpu = serde_json::to_value(value).ok(),
            CollectedData::Memory(value) => memory = serde_json::to_value(value).ok(),
            CollectedData::Disks(value) => disks = serde_json::to_value(value).ok(),
            CollectedData::Uptime(seconds) => uptime_seconds = Some(*seconds),
            CollectedData::Network(value) => network = serde_json::to_value(value).ok(),
            CollectedData::Docker(value) => docker = serde_json::to_value(value).ok(),
        }
    }

    let health = health_of(&cpu, &memory, &disks);
    let collectors = samples
        .iter()
        .map(|(sample, _)| yukinal_database::models::CollectorSample {
            collector_id: sample.collector_id.clone(),
            collected_at: sample.collected_at.clone(),
            ok: sample.ok,
            error: sample.error.clone(),
        })
        .collect();

    ServerSnapshot {
        id: format!("snap_{server_id}_{}", now_epoch_seconds()),
        server_id: server_id.to_string(),
        collected_at: collected_at.to_string(),
        health,
        os,
        cpu,
        memory,
        disks,
        uptime_seconds,
        network,
        docker,
        capabilities,
        collectors: Some(collectors),
    }
}

fn health_of(
    cpu: &Option<serde_json::Value>,
    memory: &Option<serde_json::Value>,
    disks: &Option<serde_json::Value>,
) -> HealthState {
    let cpu_usage = cpu
        .as_ref()
        .and_then(|value| value.get("usagePercent"))
        .and_then(serde_json::Value::as_f64);
    let memory_usage = memory
        .as_ref()
        .and_then(|value| value.get("usagePercent"))
        .and_then(serde_json::Value::as_f64);
    let disk_usage = disks
        .as_ref()
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| {
            rows.iter()
                .filter_map(|row| row.get("usagePercent").and_then(serde_json::Value::as_f64))
                .max_by(|a, b| a.total_cmp(b))
        });
    match overall(cpu_usage, memory_usage, disk_usage) {
        Some(HealthClass::Healthy) => HealthState::Healthy,
        Some(HealthClass::Warning) => HealthState::Warning,
        Some(HealthClass::Critical) => HealthState::Critical,
        None => HealthState::Unknown,
    }
}

fn now_epoch_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|delta| delta.as_secs())
        .unwrap_or(0)
}
