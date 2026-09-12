//! 远端文件服务：传输 trait + 套在它外面的策略与上限。
//!
//! 分两层是刻意的：
//! - [`RemoteFileTransport`] 是**能力对传输的最小需求**（一次目录列表、一次有上限读取、
//!   一次覆盖写）。桌面侧用 `TerminalService`（SFTP）实现它，测试用内存假传输实现它 ——
//!   策略与上限因此可以完全离线测试，不需要真机，也不需要 Tauri。
//! - [`RemoteFileService`] 是**唯一**允许上层调用的入口。它把路径策略、字节上限与有界解码
//!   摆在传输前面，失败是类型化的 [`Error`]。
//!
//! 失败码（`invalid_input` / `denied_by_policy` / `transport`）**不在**这里决定：那是 sidecar
//! 的 host 协议词汇，映射留在 `apps/desktop/src-tauri/src/commands/host.rs`。本模块只保证
//! 「哪一类失败」是确定的，以及文案与规则同源。
//!
//! 实现拆成四个子模块，公开路径由下面的 `pub use` 原样转出：
//! `error`（`Error` / `TransportError` / 两个 `Result` 别名）、
//! `types`（`RemoteFileTransport` 与 `Listing` / `Read` / `Edit` / `Write` 结果）、
//! `request`（三个 `Agent*Request`）、`remote_file_service`（`RemoteFileService` 自身）、
//! `helpers`（`byte_match_offsets` / `count_lines` / `read_result` / `join_remote_path`）。

mod error;
mod helpers;
mod remote_file_service;
mod request;
mod types;

pub use error::{Error, Result, TransportError, TransportResult};
pub use remote_file_service::RemoteFileService;
pub use request::{AgentEditRequest, AgentReadRequest, AgentWriteRequest};
pub use types::{
    ListedEntry, RemoteEdit, RemoteEntry, RemoteFileTransport, RemoteListing, RemoteRead,
    RemoteWrite,
};
