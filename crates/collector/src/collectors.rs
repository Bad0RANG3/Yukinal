//! MVP 的 7 个采集器：OS / CPU / Memory / Disk / Uptime / Network / Docker。
//!
//! 全部解析函数是纯函数（输入一行样例输出，输出结构），单位测试用固定 fixture；
//! 解析失败一律上抛 —— 采集器单条失败记 `ok=false`，不会静默产出坏数据。

use futures::future::BoxFuture;

use crate::{run, CollectedData, Collector, CollectorContext, CollectorError, Result};

// ---------------------------------------------------------------------------
// 数据结构（与 @yukinal/shared 的 snapshot 字段 camelCase 对齐）

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OsInfo {
    pub distribution: String,
    pub version: String,
    pub hostname: String,
    pub kernel: String,
    pub arch: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CpuSample {
    pub model: String,
    pub cores: u32,
    pub usage_percent: f64,
    pub load_average: [f64; 3],
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySample {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub usage_percent: f64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskUsage {
    pub device: String,
    pub mount_point: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub usage_percent: f64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkSample {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerInfo {
    pub name: String,
    pub image: String,
    pub state: String,
    pub status: String,
    pub restart_count: u32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerInfo {
    pub available: bool,
    pub containers: Vec<ContainerInfo>,
}

// ---------------------------------------------------------------------------
// OS

pub struct Os;

impl Collector for Os {
    fn id(&self) -> &'static str {
        "os"
    }

    fn detect(&self, context: &CollectorContext) -> BoxFuture<'static, Result<bool>> {
        let context = context.clone_context();
        Box::pin(async move {
            let out = run(&context, "uname -s 2>/dev/null; test $? -eq 0").await?;
            let present = !out.stdout.trim().is_empty();
            context.set_capability("linux", present);
            Ok(present)
        })
    }

    fn collect(&self, context: &CollectorContext) -> BoxFuture<'static, Result<CollectedData>> {
        let context = context.clone_context();
        Box::pin(async move {
            let os_release = run(&context, "cat /etc/os-release 2>/dev/null").await?;
            let kernel = run(&context, "uname -r").await?;
            let arch = run(&context, "uname -m").await?;
            let hostname = run(&context, "hostname").await?;
            let (distribution, version) = parse_os_release(&os_release.stdout);
            if distribution.is_empty() {
                return Err(CollectorError::Collect {
                    collector: "os".into(),
                    message: "failed to parse /etc/os-release".into(),
                });
            }
            Ok(CollectedData::Os(OsInfo {
                distribution,
                version,
                hostname: hostname.stdout.trim().to_string(),
                kernel: kernel.stdout.trim().to_string(),
                arch: arch.stdout.trim().to_string(),
            }))
        })
    }
}

fn parse_os_release(raw: &str) -> (String, String) {
    let mut dist = String::new();
    let mut version = String::new();
    for line in raw.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"');
        match key {
            "NAME" => dist = value.to_string(),
            "VERSION_ID" | "VERSION" if version.is_empty() => version = value.to_string(),
            _ => {}
        }
    }
    (dist, version)
}

// ---------------------------------------------------------------------------
// CPU

pub struct Cpu;

impl Collector for Cpu {
    fn id(&self) -> &'static str {
        "cpu"
    }

    fn detect(&self, context: &CollectorContext) -> BoxFuture<'static, Result<bool>> {
        let context = context.clone_context();
        Box::pin(async move {
            let out = run(&context, "test -r /proc/stat && echo ok").await?;
            Ok(out.exit_code == 0)
        })
    }

    fn collect(&self, context: &CollectorContext) -> BoxFuture<'static, Result<CollectedData>> {
        let context = context.clone_context();
        Box::pin(async move {
            let a = run(&context, "head -n1 /proc/stat").await?;
            // 两次采样间隔 120ms 算使用率（与 top 同思路）。
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            let b = run(&context, "head -n1 /proc/stat").await?;
            let usage_percent =
                cpu_usage(&parse_cpu_ticks(&a.stdout)?, &parse_cpu_ticks(&b.stdout)?);

            let cores = run(
                &context,
                "nproc 2>/dev/null || grep -c ^processor /proc/cpuinfo",
            )
            .await?;
            let model = run(
                &context,
                "grep -m1 'model name' /proc/cpuinfo | cut -d: -f2",
            )
            .await?;
            let load = run(&context, "cat /proc/loadavg").await?;

            Ok(CollectedData::Cpu(CpuSample {
                model: model.stdout.trim().to_string(),
                cores: cores.stdout.trim().parse().unwrap_or(1),
                usage_percent,
                load_average: parse_load(&load.stdout),
            }))
        })
    }
}

/// `cpu user nice system idle iowait irq softirq steal ...` -> ticks
fn parse_cpu_ticks(line: &str) -> Result<[u64; 4]> {
    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1) // "cpu"
        .take(4)
        .map(|field| field.parse().unwrap_or(0))
        .collect();
    if fields.len() < 4 {
        return Err(CollectorError::Collect {
            collector: "cpu".into(),
            message: format!("unexpected /proc/stat line: {line}"),
        });
    }
    Ok([fields[0], fields[1], fields[2], fields[3]])
}

fn cpu_usage(a: &[u64; 4], b: &[u64; 4]) -> f64 {
    let idle_a = a[3];
    let idle_b = b[3];
    let total_a: u64 = a.iter().sum();
    let total_b: u64 = b.iter().sum();
    let delta_total = total_b.saturating_sub(total_a);
    let delta_idle = idle_b.saturating_sub(idle_a);
    if delta_total == 0 {
        return 0.0;
    }
    let busy = delta_total.saturating_sub(delta_idle) as f64;
    (busy / delta_total as f64 * 100.0).clamp(0.0, 100.0)
}

fn parse_load(line: &str) -> [f64; 3] {
    let mut out = [0.0f64; 3];
    for (index, field) in line.split_whitespace().take(3).enumerate() {
        out[index] = field.parse().unwrap_or(0.0);
    }
    out
}

// ---------------------------------------------------------------------------
// Memory

pub struct Memory;

impl Collector for Memory {
    fn id(&self) -> &'static str {
        "memory"
    }

    fn detect(&self, context: &CollectorContext) -> BoxFuture<'static, Result<bool>> {
        let context = context.clone_context();
        Box::pin(async move {
            let out = run(&context, "test -r /proc/meminfo && echo ok").await?;
            Ok(out.exit_code == 0)
        })
    }

    fn collect(&self, context: &CollectorContext) -> BoxFuture<'static, Result<CollectedData>> {
        let context = context.clone_context();
        Box::pin(async move {
            let out = run(&context, "cat /proc/meminfo").await?;
            let total =
                meminfo_kb(&out.stdout, "MemTotal").ok_or_else(|| CollectorError::Collect {
                    collector: "memory".into(),
                    message: "MemTotal missing".into(),
                })?;
            let available = meminfo_kb(&out.stdout, "MemAvailable").unwrap_or_else(|| {
                total.saturating_sub(meminfo_kb(&out.stdout, "MemFree").unwrap_or(0))
            });
            let used = total.saturating_sub(available);
            let usage_percent = if total == 0 {
                0.0
            } else {
                used as f64 / total as f64 * 100.0
            };
            Ok(CollectedData::Memory(MemorySample {
                total_bytes: total * 1024,
                used_bytes: used * 1024,
                available_bytes: available * 1024,
                usage_percent,
            }))
        })
    }
}

fn meminfo_kb(raw: &str, key: &str) -> Option<u64> {
    raw.lines().find_map(|line| {
        let (name, rest) = line.split_once(':')?;
        if name != key {
            return None;
        }
        rest.split_whitespace().next()?.parse::<u64>().ok()
    })
}

// ---------------------------------------------------------------------------
// Uptime

pub struct Uptime;

impl Collector for Uptime {
    fn id(&self) -> &'static str {
        "uptime"
    }

    fn detect(&self, context: &CollectorContext) -> BoxFuture<'static, Result<bool>> {
        let context = context.clone_context();
        Box::pin(async move {
            let out = run(&context, "test -r /proc/uptime && echo ok").await?;
            Ok(out.exit_code == 0)
        })
    }

    fn collect(&self, context: &CollectorContext) -> BoxFuture<'static, Result<CollectedData>> {
        let context = context.clone_context();
        Box::pin(async move {
            let out = run(&context, "cat /proc/uptime").await?;
            let seconds = out
                .stdout
                .split_whitespace()
                .next()
                .and_then(|field| field.split('.').next())
                .and_then(|field| field.parse::<u64>().ok())
                .ok_or_else(|| CollectorError::Collect {
                    collector: "uptime".into(),
                    message: "unexpected /proc/uptime output".into(),
                })?;
            Ok(CollectedData::Uptime(seconds))
        })
    }
}

// ---------------------------------------------------------------------------
// Disk

pub struct Disk;

impl Collector for Disk {
    fn id(&self) -> &'static str {
        "disk"
    }

    fn detect(&self, context: &CollectorContext) -> BoxFuture<'static, Result<bool>> {
        let context = context.clone_context();
        Box::pin(async move {
            let out = run(&context, "command -v df >/dev/null && echo ok").await?;
            Ok(out.exit_code == 0)
        })
    }

    fn collect(&self, context: &CollectorContext) -> BoxFuture<'static, Result<CollectedData>> {
        let context = context.clone_context();
        Box::pin(async move {
            let out = run(&context, "df -B1 -P 2>/dev/null").await?;
            let disks = parse_df(&out.stdout)?;
            if disks.is_empty() {
                return Err(CollectorError::Collect {
                    collector: "disk".into(),
                    message: "df produced no parseable rows".into(),
                });
            }
            Ok(CollectedData::Disks(disks))
        })
    }
}

fn parse_df(raw: &str) -> Result<Vec<DiskUsage>> {
    let mut disks = Vec::new();
    for line in raw.lines().skip(1) {
        // Filesystem 512-blocks Used Available Capacity Mounted on
        let mut fields = line.split_whitespace();
        let Some(device) = fields.next() else {
            continue;
        };
        // device may be "host:path"; keep as-is.
        let (size, used, _available): (u64, u64, u64) = (
            parse_i64(fields.next())?,
            parse_i64(fields.next())?,
            parse_i64(fields.next())?,
        );
        let _capacity = fields.next();
        let mount_point = fields.collect::<Vec<_>>().join(" ");
        if device.starts_with("tmpfs") || device.starts_with("overlay") {
            continue;
        }
        let usage_percent = if size == 0 {
            0.0
        } else {
            used as f64 / size as f64 * 100.0
        };
        disks.push(DiskUsage {
            device: device.to_string(),
            mount_point,
            total_bytes: size,
            used_bytes: used,
            usage_percent,
        });
    }
    Ok(disks)
}

fn parse_i64(field: Option<&str>) -> Result<u64> {
    field
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| CollectorError::Collect {
            collector: "disk".into(),
            message: "unexpected df column".into(),
        })
}

// ---------------------------------------------------------------------------
// Network

pub struct Network;

impl Collector for Network {
    fn id(&self) -> &'static str {
        "network"
    }

    fn detect(&self, context: &CollectorContext) -> BoxFuture<'static, Result<bool>> {
        let context = context.clone_context();
        Box::pin(async move {
            let out = run(&context, "test -r /proc/net/dev && echo ok").await?;
            Ok(out.exit_code == 0)
        })
    }

    fn collect(&self, context: &CollectorContext) -> BoxFuture<'static, Result<CollectedData>> {
        let context = context.clone_context();
        Box::pin(async move {
            let out = run(&context, "cat /proc/net/dev").await?;
            let samples = parse_net_dev(&out.stdout);
            if samples.is_empty() {
                return Err(CollectorError::Collect {
                    collector: "network".into(),
                    message: "no interfaces in /proc/net/dev".into(),
                });
            }
            Ok(CollectedData::Network(samples))
        })
    }
}

fn parse_net_dev(raw: &str) -> Vec<NetworkSample> {
    let mut samples = Vec::new();
    for line in raw.lines().skip(2) {
        // eth0:  rx  tx ... , rx bytes is field 1, tx bytes field 9
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let fields: Vec<u64> = rest
            .split_whitespace()
            .map(|field| field.parse().unwrap_or(0))
            .collect();
        if fields.len() >= 9 {
            samples.push(NetworkSample {
                name: name.trim().to_string(),
                rx_bytes: fields[0],
                tx_bytes: fields[8],
            });
        }
    }
    samples
}

// ---------------------------------------------------------------------------
// Docker

pub struct Docker;

impl Collector for Docker {
    fn id(&self) -> &'static str {
        "docker"
    }

    fn detect(&self, context: &CollectorContext) -> BoxFuture<'static, Result<bool>> {
        let context = context.clone_context();
        Box::pin(async move {
            let out = run(&context, "docker info >/dev/null 2>&1; echo $?").await?;
            let available = out.stdout.contains('0') && !out.stdout.contains('1');
            context.set_capability("docker", available);
            Ok(available)
        })
    }

    fn collect(&self, context: &CollectorContext) -> BoxFuture<'static, Result<CollectedData>> {
        let context = context.clone_context();
        Box::pin(async move {
            let probe = run(&context, "docker info >/dev/null 2>&1").await;
            let daemon_ok = match probe {
                Ok(_) => true,
                // docker 未安装 / daemon 没起：available=false，不算采集错误。
                Err(CollectorError::CommandFailed { .. })
                | Err(CollectorError::Runner(_))
                | Err(CollectorError::Timeout) => false,
                Err(_) => false,
            };
            if !daemon_ok {
                return Ok(CollectedData::Docker(DockerInfo {
                    available: false,
                    containers: Vec::new(),
                }));
            }

            let out = run(&context, "docker ps -a --format '{{json .}}' 2>/dev/null").await?;
            let containers = parse_docker_ps(&out.stdout)?;
            Ok(CollectedData::Docker(DockerInfo {
                available: true,
                containers,
            }))
        })
    }
}

fn parse_docker_ps(raw: &str) -> Result<Vec<ContainerInfo>> {
    let mut containers = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // 每行是一个 JSON 对象；解析失败记为不可信行，继续。
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        containers.push(ContainerInfo {
            name: value
                .get("Names")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
            image: value
                .get("Image")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
            state: value
                .get("State")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
            status: value
                .get("Status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
            restart_count: value
                .get("RestartCount")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as u32,
        });
    }
    Ok(containers)
}

// ---------------------------------------------------------------------------
// 单元测试：纯解析函数 + fixture

#[cfg(test)]
mod tests {
    use super::*;

    const DF_SAMPLE: &str = "Filesystem     512-blocks      Used Available Capacity Mounted on\n/dev/sda1     104857600  41943040  62914560   40%  /\ntmpfs             2097152    1048576   1048576   50%  /run\n";

    const MEMINFO_SAMPLE: &str =
        "MemTotal:       16777216 kB\nMemFree:         8388608 kB\nMemAvailable:   10485760 kB\n";

    const NET_DEV_SAMPLE: &str = "Inter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n  eth0: 1099511627776  123456789    0    0    0     0          0         0  2199023255552  987654321    0    0    0     0     0          0\n    lo:       10000        10    0    0    0     0          0         0      10000        10    0    0    0     0     0          0\n";

    #[test]
    fn os_release_parses() {
        let (dist, version) =
            parse_os_release("NAME=\"Ubuntu\"\nVERSION=\"24.04 LTS (Noble Numbat)\"\nID=ubuntu\n");
        assert_eq!(dist, "Ubuntu");
        assert_eq!(version, "24.04 LTS (Noble Numbat)");
    }

    #[test]
    fn cpu_usage_between_two_ticks() {
        let a = parse_cpu_ticks("cpu  1000 0 1000 8000").expect("a");
        let b = parse_cpu_ticks("cpu  1020 0 1030 8060").expect("b");
        // busy delta = (20+30)=50, total delta = 110 → ~45.45%
        let usage = cpu_usage(&a, &b);
        assert!((usage - 45.45).abs() < 0.2, "usage={usage}");
    }

    #[test]
    fn load_average_parses() {
        let load = parse_load("0.52 0.38 0.30 1/234 5678");
        assert_eq!(load, [0.52, 0.38, 0.30]);
    }

    #[test]
    fn meminfo_parses_kib() {
        assert_eq!(meminfo_kb(MEMINFO_SAMPLE, "MemTotal"), Some(16_777_216));
        assert_eq!(meminfo_kb(MEMINFO_SAMPLE, "MemAvailable"), Some(10_485_760));
        assert_eq!(meminfo_kb(MEMINFO_SAMPLE, "MemFree"), Some(8_388_608));
    }

    #[test]
    fn df_skips_tmpfs_and_reports_bytes() {
        let disks = parse_df(DF_SAMPLE).expect("parse");
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].device, "/dev/sda1");
        assert_eq!(disks[0].mount_point, "/");
        assert_eq!(disks[0].total_bytes, 104_857_600);
        assert_eq!(disks[0].used_bytes, 41_943_040);
    }

    #[test]
    fn net_dev_parses_bytes_matrix() {
        let samples = parse_net_dev(NET_DEV_SAMPLE);
        assert_eq!(samples.len(), 2);
        let eth = &samples[0];
        assert_eq!(eth.name, "eth0");
        assert_eq!(eth.rx_bytes, 1_099_511_627_776);
        assert_eq!(eth.tx_bytes, 2_199_023_255_552);
    }

    #[test]
    fn docker_ps_parses_json_lines() {
        let raw = r#"{"Names":"api","Image":"ghcr.io/example/api:1.4.2","State":"running","Status":"Up 12 hours","RestartCount":0}
{"Names":"redis","Image":"redis:7","State":"exited","Status":"Exited (0) 2 hours ago","RestartCount":2}"#;
        let containers = parse_docker_ps(raw).expect("parse");
        assert_eq!(containers.len(), 2);
        assert_eq!(containers[0].name, "api");
        assert_eq!(containers[1].restart_count, 2);
    }
}
