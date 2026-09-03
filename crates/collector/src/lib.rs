//! yukinal-collector — Server Collector Engine。
//!
//! 插件化：每个能力一个 `Collector`，先 `detect`（写 capabilities），再 `collect`
//! （产出结构化数据）。UI 只消费结构化数据 + health 结论，不消费原始命令输出。
//!
//! MVP 只实现：OS / CPU / Memory / Disk / Uptime / Network / Docker。
//! 不要在第一版实现 systemd / nginx / postgres / redis / kubernetes。
//!
//! 对应的 TS 镜像类型在 `@yukinal/shared`，改动必须双侧同步。

#![allow(dead_code)] // day-0 contract skeleton

use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    /// 采集命令执行失败（含 SSH 层错误）。
    Command(String),
    /// 原始输出无法解析 —— 必须上报，不允许静默丢数据。
    Parse(String),
    Timeout,
    Unsupported,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Command(message) => write!(f, "collector command failed: {message}"),
            Error::Parse(message) => write!(f, "collector output unparsable: {message}"),
            Error::Timeout => write!(f, "collector timed out"),
            Error::Unsupported => write!(f, "collector unsupported on this server"),
        }
    }
}

impl std::error::Error for Error {}

/// 能力模型由 detect 自动探测，UI 据此动态展示功能。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub linux: bool,
    pub docker: bool,
    pub systemd: bool,
    pub nginx: bool,
    pub postgres: bool,
    pub redis: bool,
    pub kubernetes: bool,
}

#[derive(Debug, Clone)]
pub struct CollectorContext {
    pub server_id: String,
    /// 已建立的 SSH session。
    pub session_id: String,
    pub capabilities: Capabilities,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct OsInfo {
    pub distribution: String,
    pub version: String,
    pub hostname: String,
    pub kernel: String,
    pub arch: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CpuInfo {
    pub model: String,
    pub cores: u32,
    /// 0..=100
    pub usage_percent: f32,
    pub load_average: [f32; 3],
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemoryInfo {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub usage_percent: f32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiskUsage {
    pub device: String,
    pub mount_point: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub usage_percent: f32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NetworkInfo {
    pub interfaces: Vec<NetworkInterface>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NetworkInterface {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContainerInfo {
    pub name: String,
    pub image: String,
    /// running / restarting / exited / paused / ...
    pub state: String,
    pub status: String,
    pub restart_count: u32,
}

/// `collect()` 的结构化产物。新增变体时同步 `@yukinal/shared` 与 snapshots 表。
#[derive(Debug, Clone, PartialEq)]
pub enum CollectedData {
    Os(OsInfo),
    Cpu(CpuInfo),
    Memory(MemoryInfo),
    Disk(Vec<DiskUsage>),
    Uptime { seconds: u64 },
    Network(NetworkInfo),
    Docker { containers: Vec<ContainerInfo> },
}

/// 。
pub trait Collector {
    /// 稳定 id，例如 `"docker"`。同时用于 capabilities 解析与 UI 开关。
    fn id(&self) -> &str;

    /// 只读探测，成本必须低。
    fn detect(&self, context: &CollectorContext) -> Result<bool>;

    fn collect(
        &self,
        context: &CollectorContext,
    ) -> impl std::future::Future<Output = Result<CollectedData>> + Send;
}
