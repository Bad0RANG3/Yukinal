//! Docker 采集器：`docker ps -a --format '{{json .}}'`。
//!
//! daemon 没起 / docker 未安装不是采集错误，而是 `available: false`。

use futures::future::BoxFuture;

use crate::{run, CollectedData, Collector, CollectorContext, CollectorError, Result};

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

#[cfg(test)]
mod tests {
    use super::*;

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
