//! yukinal-core — Rust 侧的编排层与本地系统能力。
//!
//! 职责：
//! - 聚合 `yukinal-ssh` / `yukinal-terminal` / `yukinal-collector` / `yukinal-database`，
//!   向 Tauri Command 暴露稳定入口。
//! - 本地系统操作（本机 process / filesystem / OS info）。
//! - 命令风险静态规则只是 *信号生产者*，不是决策者（ADR 0005）。
//!
//! 边界：
//! - React 不允许直接触达本 crate 之外的任何原生能力，必须经 `commands`。
//! - Agent 不允许直接依赖本 crate，必须经 Tool → Permission Engine → 本 crate。
//!
//! `yukinal-credentials` 与 `yukinal-filesystem` 不在上方的依赖清单里：
//! credentials 由桌面命令层在使用点解析（本 crate 不需要它），
//! filesystem 目前仍是契约占位、尚无实现，因此这里也不依赖它。
//!
//! 实现随 SSH 层逐步落地；当前为契约占位。

pub mod collector;
pub mod health;
pub mod ipc;
pub mod mcp;

/// 一行不可信文本 → 可以安全写进日志/错误的东西。
///
/// 它是 crate 级能力而不是 `sidecar` 的私有工具：sidecar 的日志转发与 MCP 客户端的
/// stderr/诊断尾部都要用它，而**脱敏逻辑只能有一份**——第二份迟早与第一份漂移，而这里
/// 的漏报代价是一把收不回的密钥。私有（`mod`，非 `pub mod`）是因为 crate 之外无人该用它。
mod redact;
pub mod sidecar;

pub mod supervisor;
pub mod terminal;
