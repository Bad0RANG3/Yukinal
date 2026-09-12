//! MVP 的 7 个采集器：OS / CPU / Memory / Disk / Uptime / Network / Docker。
//!
//! 全部解析函数是纯函数（输入一行样例输出，输出结构），单位测试用固定 fixture；
//! 解析失败一律上抛 —— 采集器单条失败记 `ok=false`，不会静默产出坏数据。
//!
//! 一个采集器一个文件：数据结构、`Collector` 实现、解析函数与它自己的测试放在一起。
//! 这里只做转出，所以 `crate::collectors::Os` / `crate::collectors::OsInfo`
//! 这些既有路径全部不变。

mod cpu;
mod disk;
mod docker;
mod memory;
mod network;
mod os;
mod uptime;

pub use cpu::{Cpu, CpuSample};
pub use disk::{Disk, DiskUsage};
pub use docker::{ContainerInfo, Docker, DockerInfo};
pub use memory::{Memory, MemorySample};
pub use network::{Network, NetworkSample};
pub use os::{Os, OsInfo};
pub use uptime::Uptime;
