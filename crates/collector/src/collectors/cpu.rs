//! CPU 采集器：两次 `/proc/stat` 采样算使用率，外加 cores / model / loadavg。

use futures::future::BoxFuture;

use crate::{run, CollectedData, Collector, CollectorContext, CollectorError, Result};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CpuSample {
    pub model: String,
    pub cores: u32,
    pub usage_percent: f64,
    pub load_average: [f64; 3],
}

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
