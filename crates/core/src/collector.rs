//! Collector → snapshot 装配：把采集器输出拼成 `snapshots` 行（camelCase 线形），
//! 健康度由 `health.rs`（与 shared 同一份阈值）计算。

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;
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
    cancel: &CancellationToken,
) -> yukinal_collector::Result<(ServerSnapshot, Vec<CollectorSample>)> {
    collect_snapshot_with_runner(
        server_id,
        collected_at,
        cancel,
        yukinal_collector::runners::ssh(Arc::clone(ssh), session.clone()),
    )
    .await
}

/// Collect and assemble one snapshot using an injected transport runner.
/// The desktop uses this seam to feed C++ SSH results through the same parsers and UI model.
pub async fn collect_snapshot_with_runner(
    server_id: &str,
    collected_at: &str,
    cancel: &CancellationToken,
    runner: yukinal_collector::Runner,
) -> yukinal_collector::Result<(ServerSnapshot, Vec<CollectorSample>)> {
    let engine = CollectorEngine::with_mvp();
    let context = CollectorContext::new(server_id, runner).with_cancel(cancel.clone());
    engine.detect_all(&context).await?;
    let samples = engine.collect_all(&context, collected_at).await;

    let capabilities = capabilities_from(&context);
    let snapshot = assemble(server_id, collected_at, &samples, capabilities);
    Ok((
        snapshot,
        samples.iter().map(|(sample, _)| sample.clone()).collect(),
    ))
}

/// Parse a bounded C++ snapshot batch through the same collector implementations used by Rust SSH.
pub async fn collect_snapshot_with_command_results(
    server_id: &str,
    collected_at: &str,
    cancel: &CancellationToken,
    outputs: HashMap<String, (i32, String)>,
) -> yukinal_collector::Result<(ServerSnapshot, Vec<CollectorSample>)> {
    const COMMANDS: &[(&str, &str)] = &[
        ("os_detect", "uname -s 2>/dev/null; test $? -eq 0"),
        ("os_release", "cat /etc/os-release 2>/dev/null"),
        ("os_kernel", "uname -r"),
        ("os_arch", "uname -m"),
        ("os_hostname", "hostname"),
        ("cpu_detect", "test -r /proc/stat && echo ok"),
        ("cpu_ticks_a", "head -n1 /proc/stat"),
        ("cpu_ticks_b", "head -n1 /proc/stat"),
        (
            "cpu_cores",
            "nproc 2>/dev/null || grep -c ^processor /proc/cpuinfo",
        ),
        (
            "cpu_model",
            "grep -m1 'model name' /proc/cpuinfo | cut -d: -f2",
        ),
        ("cpu_load", "cat /proc/loadavg"),
        ("memory_detect", "test -r /proc/meminfo && echo ok"),
        ("memory_info", "cat /proc/meminfo"),
        ("uptime_detect", "test -r /proc/uptime && echo ok"),
        ("uptime_info", "cat /proc/uptime"),
        ("disk_detect", "command -v df >/dev/null && echo ok"),
        ("disk_usage", "df -B1 -P 2>/dev/null"),
        ("network_detect", "test -r /proc/net/dev && echo ok"),
        ("network_info", "cat /proc/net/dev"),
        ("docker_detect", "docker info >/dev/null 2>&1; echo $?"),
        ("docker_info", "docker info >/dev/null 2>&1"),
        (
            "docker_ps",
            "docker ps -a --format '{{json .}}' 2>/dev/null",
        ),
    ];
    let mut queued = HashMap::<String, VecDeque<yukinal_collector::CommandOutput>>::new();
    for (id, command) in COMMANDS {
        let (exit_code, stdout) = outputs.get(*id).ok_or_else(|| {
            yukinal_collector::CollectorError::Runner(
                "the C++ SSH service omitted a system snapshot result".into(),
            )
        })?;
        queued.entry((*command).to_string()).or_default().push_back(
            yukinal_collector::CommandOutput {
                exit_code: *exit_code,
                stdout: stdout.clone(),
                stderr: String::new(),
            },
        );
    }
    let queued = Arc::new(Mutex::new(queued));
    let runner: yukinal_collector::Runner = Arc::new(move |command, _timeout, _cancel| {
        let output = queued
            .lock()
            .ok()
            .and_then(|mut queued| queued.get_mut(command).and_then(VecDeque::pop_front));
        Box::pin(async move {
            output.ok_or_else(|| {
                yukinal_collector::CollectorError::Runner(
                    "collector requested an unsupported snapshot command".into(),
                )
            })
        })
    });
    collect_snapshot_with_runner(server_id, collected_at, cancel, runner).await
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
    // 实现已移到 `yukinal-time`；本文件原来那份与 terminal / sidecar / live 测试
    // 里的三份逐字相同。
    yukinal_time::now_epoch_seconds()
}

#[cfg(test)]
mod tests {
    use super::collect_snapshot_with_command_results;
    use std::collections::HashMap;
    use tokio_util::sync::CancellationToken;
    use yukinal_database::models::HealthState;

    #[tokio::test]
    async fn cpp_snapshot_batch_uses_the_existing_collector_parsers() {
        let outputs = HashMap::from([
            ("os_detect".into(), (0, "Linux\n".into())),
            (
                "os_release".into(),
                (0, "NAME=\"Test Linux\"\nVERSION_ID=\"1\"\n".into()),
            ),
            ("os_kernel".into(), (0, "6.1.0\n".into())),
            ("os_arch".into(), (0, "x86_64\n".into())),
            ("os_hostname".into(), (0, "mock-host\n".into())),
            ("cpu_detect".into(), (0, "ok\n".into())),
            ("cpu_ticks_a".into(), (0, "cpu 100 0 100 800\n".into())),
            ("cpu_ticks_b".into(), (0, "cpu 120 0 110 830\n".into())),
            ("cpu_cores".into(), (0, "4\n".into())),
            ("cpu_model".into(), (0, " Mock CPU\n".into())),
            ("cpu_load".into(), (0, "0.10 0.20 0.30 1/20 99\n".into())),
            ("memory_detect".into(), (0, "ok\n".into())),
            (
                "memory_info".into(),
                (
                    0,
                    "MemTotal: 1024 kB\nMemAvailable: 512 kB\n".into(),
                ),
            ),
            ("uptime_detect".into(), (0, "ok\n".into())),
            ("uptime_info".into(), (0, "120.0 100.0\n".into())),
            ("disk_detect".into(), (0, "ok\n".into())),
            (
                "disk_usage".into(),
                (
                    0,
                    "Filesystem 1B-blocks Used Available Use% Mounted on\n/dev/vda1 1000 250 750 25% /\n".into(),
                ),
            ),
            ("network_detect".into(), (0, "ok\n".into())),
            (
                "network_info".into(),
                (
                    0,
                    "Inter-| Receive | Transmit\n face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n eth0: 100 1 0 0 0 0 0 0 200 2 0 0 0 0 0 0\n".into(),
                ),
            ),
            ("docker_detect".into(), (0, "0\n".into())),
            ("docker_info".into(), (0, String::new())),
            (
                "docker_ps".into(),
                (
                    0,
                    "{\"Names\":\"api\",\"Image\":\"example/api\",\"State\":\"running\",\"Status\":\"Up\",\"RestartCount\":0}\n".into(),
                ),
            ),
        ]);

        let (snapshot, samples) = collect_snapshot_with_command_results(
            "srv_test",
            "2026-10-15T00:00:00Z",
            &CancellationToken::new(),
            outputs,
        )
        .await
        .expect("the fixed C++ snapshot result set should parse");

        assert_eq!(snapshot.health, HealthState::Healthy);
        assert_eq!(
            snapshot
                .os
                .as_ref()
                .and_then(|value| value["hostname"].as_str()),
            Some("mock-host")
        );
        assert_eq!(
            snapshot
                .cpu
                .as_ref()
                .and_then(|value| value["cores"].as_u64()),
            Some(4)
        );
        assert_eq!(
            snapshot
                .cpu
                .as_ref()
                .and_then(|value| value["usagePercent"].as_f64()),
            Some(50.0)
        );
        assert_eq!(
            snapshot
                .memory
                .as_ref()
                .and_then(|value| value["totalBytes"].as_u64()),
            Some(1_048_576)
        );
        assert_eq!(snapshot.uptime_seconds, Some(120));
        assert_eq!(
            snapshot
                .network
                .as_ref()
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            snapshot
                .docker
                .as_ref()
                .and_then(|value| value["containers"].as_array())
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(samples.len(), 7);
        assert!(samples.iter().all(|sample| sample.ok));
    }
}
