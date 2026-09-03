//! yukinal-collector — 服务器采集引擎（MVP 只做 7 个采集器）。
//!
//! 采集器走插件化 trait：`id` / `detect`（探测能力并写 capabilities）/ `collect`
//! （产出结构化数据）。命令通过注入的 [`Runner`] 执行（远端 ssh 或本机进程），
//! 采集器本身不关心传输 —— 解析用固定 fixture 测试，真机走 env 门控集成测试。
//!
//! 规则：解析失败不是静默 —— 单采集器以 `ok=false` + 错误上抛，拖垮卡片不拖垮整页。

#![allow(dead_code)] // 服务化采集器（nginx/postgres/…）在 MVP 之后补

use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;

pub mod collectors;
pub mod runners;

pub type Result<T> = std::result::Result<T, CollectorError>;

#[derive(Debug, thiserror::Error)]
pub enum CollectorError {
    #[error("command failed (exit {exit_code}): {stderr}")]
    CommandFailed { exit_code: i32, stderr: String },
    #[error("command timed out")]
    Timeout,
    #[error("collector {collector} failed: {message}")]
    Collect { collector: String, message: String },
    #[error("runner error: {0}")]
    Runner(String),
}

/// 一次命令的结果。采集器解析它，绝不直接把它当指令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// 命令执行入口（远端 ssh / 本机进程的公共视图）。
pub type Runner =
    Arc<dyn Fn(&str, Duration) -> BoxFuture<'static, Result<CommandOutput>> + Send + Sync>;

/// 目标 + 可变 capabilities。capabilities 由 `detect()` 写入，与数据库行对齐。
pub struct CollectorContext {
    pub server_id: String,
    pub capabilities: std::sync::Mutex<Vec<(String, bool)>>,
    pub runner: Runner,
}

impl CollectorContext {
    #[must_use]
    pub fn new(server_id: &str, runner: Runner) -> Self {
        Self {
            server_id: server_id.to_string(),
            capabilities: std::sync::Mutex::new(Vec::new()),
            runner,
        }
    }

    /// 把上下文克隆进采集器的 async future（runner 是 Arc 闭包，拷贝很便宜）。
    #[must_use]
    pub fn clone_context(&self) -> CollectorContext {
        CollectorContext {
            server_id: self.server_id.clone(),
            capabilities: std::sync::Mutex::new(
                self.capabilities
                    .lock()
                    .map(|caps| caps.clone())
                    .unwrap_or_default(),
            ),
            runner: Arc::clone(&self.runner),
        }
    }

    pub fn set_capability(&self, key: &str, present: bool) {
        if let Ok(mut caps) = self.capabilities.lock() {
            caps.retain(|(existing, _)| existing != key);
            caps.push((key.to_string(), present));
        }
    }

    /// 命令超时上限（单条采集命令）。
    pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
}

/// 采集产物（MVP 的 7 种）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CollectedData {
    Os(collectors::OsInfo),
    Cpu(collectors::CpuSample),
    Memory(collectors::MemorySample),
    Disks(Vec<collectors::DiskUsage>),
    Uptime(u64),
    Network(Vec<collectors::NetworkSample>),
    Docker(collectors::DockerInfo),
}

/// 单采集器跑完后的健康行（写进 snapshots.collectors）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectorSample {
    pub collector_id: String,
    pub collected_at: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 插件形状：`detect` 探测能力，`collect` 产出。
pub trait Collector: Send + Sync {
    fn id(&self) -> &'static str;
    /// 探测能力并写 capabilities。探测要跑一条命令，所以与 collect 同为 async。
    fn detect(&self, context: &CollectorContext) -> BoxFuture<'static, Result<bool>>;
    fn collect(&self, context: &CollectorContext) -> BoxFuture<'static, Result<CollectedData>>;
}

/// 采集引擎：跑全部分析器，逐条上抛 ok/error，产出快照素材。
pub struct CollectorEngine {
    collectors: Vec<Arc<dyn Collector>>,
}

impl CollectorEngine {
    #[must_use]
    pub fn with_mvp() -> Self {
        Self {
            collectors: vec![
                Arc::new(collectors::Os),
                Arc::new(collectors::Cpu),
                Arc::new(collectors::Memory),
                Arc::new(collectors::Uptime),
                Arc::new(collectors::Disk),
                Arc::new(collectors::Network),
                Arc::new(collectors::Docker),
            ],
        }
    }

    #[must_use]
    pub fn with(collectors: Vec<Arc<dyn Collector>>) -> Self {
        Self { collectors }
    }

    pub fn ids(&self) -> Vec<&'static str> {
        self.collectors
            .iter()
            .map(|collector| collector.id())
            .collect()
    }

    /// 探测所有能力（写进 context.capabilities）。
    pub async fn detect_all(&self, context: &CollectorContext) -> Result<()> {
        for collector in &self.collectors {
            let present = collector.detect(context).await?;
            context.set_capability(collector.id(), present);
        }
        Ok(())
    }

    /// 采集全部：每条失败都上抛，单条错误不吞掉其它结果。
    pub async fn collect_all(
        &self,
        context: &CollectorContext,
        collected_at: &str,
    ) -> Vec<(CollectorSample, Option<CollectedData>)> {
        let mut out = Vec::with_capacity(self.collectors.len());
        for collector in &self.collectors {
            match collector.collect(context).await {
                Ok(data) => out.push((
                    CollectorSample {
                        collector_id: collector.id().to_string(),
                        collected_at: collected_at.to_string(),
                        ok: true,
                        error: None,
                    },
                    Some(data),
                )),
                Err(error) => out.push((
                    CollectorSample {
                        collector_id: collector.id().to_string(),
                        collected_at: collected_at.to_string(),
                        ok: false,
                        error: Some(error.to_string()),
                    },
                    None,
                )),
            }
        }
        out
    }
}

/// 便捷：context 的 runner 执行一条命令；非零退出按失败处理（exit 127 的
/// `docker: command not found` 会被 Docker 采集器特判，不吞）。
pub(crate) async fn run(context: &CollectorContext, command: &str) -> Result<CommandOutput> {
    (context.runner)(command, CollectorContext::COMMAND_TIMEOUT).await
}
