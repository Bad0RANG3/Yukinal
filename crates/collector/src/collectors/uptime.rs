//! Uptime 采集器：`/proc/uptime`。无解析 fixture —— 它只取第一个字段的整数部分。

use futures::future::BoxFuture;

use crate::{run, CollectedData, Collector, CollectorContext, CollectorError, Result};

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
