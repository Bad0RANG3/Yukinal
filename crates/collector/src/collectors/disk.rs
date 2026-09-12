//! Disk 采集器：`df -B1 -P`，跳过 tmpfs / overlay，按字节上报。

use futures::future::BoxFuture;

use crate::{run, CollectedData, Collector, CollectorContext, CollectorError, Result};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskUsage {
    pub device: String,
    pub mount_point: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub usage_percent: f64,
}

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

#[cfg(test)]
mod tests {
    use super::*;

    const DF_SAMPLE: &str = "Filesystem     512-blocks      Used Available Capacity Mounted on\n/dev/sda1     104857600  41943040  62914560   40%  /\ntmpfs             2097152    1048576   1048576   50%  /run\n";

    #[test]
    fn df_skips_tmpfs_and_reports_bytes() {
        let disks = parse_df(DF_SAMPLE).expect("parse");
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].device, "/dev/sda1");
        assert_eq!(disks[0].mount_point, "/");
        assert_eq!(disks[0].total_bytes, 104_857_600);
        assert_eq!(disks[0].used_bytes, 41_943_040);
    }
}
