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
pub mod sidecar;

pub mod supervisor;
pub mod terminal;
