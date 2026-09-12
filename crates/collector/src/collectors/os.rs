//! OS 采集器：`/etc/os-release` + `uname`。

use futures::future::BoxFuture;

use crate::{run, CollectedData, Collector, CollectorContext, CollectorError, Result};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OsInfo {
    pub distribution: String,
    pub version: String,
    pub hostname: String,
    pub kernel: String,
    pub arch: String,
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_release_parses() {
        let (dist, version) =
            parse_os_release("NAME=\"Ubuntu\"\nVERSION=\"24.04 LTS (Noble Numbat)\"\nID=ubuntu\n");
        assert_eq!(dist, "Ubuntu");
        assert_eq!(version, "24.04 LTS (Noble Numbat)");
    }
}
