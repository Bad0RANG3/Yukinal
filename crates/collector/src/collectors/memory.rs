//! Memory 采集器：`/proc/meminfo`。

use futures::future::BoxFuture;

use crate::{run, CollectedData, Collector, CollectorContext, CollectorError, Result};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySample {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub usage_percent: f64,
}

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

#[cfg(test)]
mod tests {
    use super::*;

    const MEMINFO_SAMPLE: &str =
        "MemTotal:       16777216 kB\nMemFree:         8388608 kB\nMemAvailable:   10485760 kB\n";

    #[test]
    fn meminfo_parses_kib() {
        assert_eq!(meminfo_kb(MEMINFO_SAMPLE, "MemTotal"), Some(16_777_216));
        assert_eq!(meminfo_kb(MEMINFO_SAMPLE, "MemAvailable"), Some(10_485_760));
        assert_eq!(meminfo_kb(MEMINFO_SAMPLE, "MemFree"), Some(8_388_608));
    }
}
