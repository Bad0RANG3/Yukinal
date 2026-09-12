//! yukinal-filesystem — 远端文件能力的唯一实现处：路径规则、字节/字符上限，以及有界的
//! `list` / `read` / `write`。
//!
//! 这个 crate 以前是一份 7 行的契约占位（「本地与远端文件能力」），而真正的规则住在别处：
//! 凭据路径黑名单、远端路径校验与读写上限全在
//! `apps/desktop/src-tauri/src/commands/host.rs`，UI 的远端浏览器
//! （`apps/desktop/src-tauri/src/commands/files.rs`）则另有一份「读到 1 MiB 就截断」的副本 ——
//! 同一段 `String::from_utf8_lossy(&bytes[..min(len, max)])` 加上 `truncated` 的算法被抄了
//! 两遍，两边各有一个 1 MiB 常量。收拢到这里之后判据只有一条：**任何「远端文件能不能碰」
//! 的规则都写在本 crate**，上层只负责把它接到传输与 sidecar 的失败码上。
//!
//! # 它是什么
//! - [`policy`]：远端路径的形状校验，以及 Agent 的凭据/进程密钥路径黑名单（大小写不敏感）。
//! - [`limits`]：路径长度上限、Agent 读写上限、UI 浏览器 1 MiB 上限，以及有界读取的解码
//!   （[`decode_bounded`]）。
//! - [`service`]：传输 trait [`RemoteFileTransport`] 与套在它外面的 [`RemoteFileService`]。
//!   策略与上限在服务里生效，失败是类型化的 [`Error`]；`invalid_input` /
//!   `denied_by_policy` / `transport` 这些**码**由上层映射（见下）。
//!
//! # 它刻意不做什么
//! - **不碰本地文件系统。** Agent 的宿主工具是「只有远端」的契约：`host.tool.execute` 在
//!   `host != "remote"` 或 `serverId` 不带 `srv_` 前缀时直接以 `denied_by_policy` 拒绝，
//!   所以本 crate 的本地实现不会有任何可达的调用方 —— 它只会变成一条没人测、也没人用的
//!   绕过路径。真要为本地文件开口子，前提是先有一套属于本机的策略（这里的黑名单讲的是
//!   远端服务器上的凭据），那是一次独立的设计，不是顺手补一个 backend 就能算数的。
//! - **不管连接与凭据。** 连接（SQLite 取 server → keychain 取 secret → russh 握手 → 缓存）
//!   属于桌面层的 `ensure_session`，本 crate 只看得见 [`RemoteFileTransport`]。这也正是
//!   它能在内存假传输上把策略与上限测完的原因。
//! - **不产失败码。** `invalid_input` / `denied_by_policy` / `transport` 是 sidecar 的 host
//!   协议词汇，映射留在命令层；本 crate 只给出类型与文案（文案是对外契约，所以和规则放在
//!   一起）。
//! - **不给 UI 浏览器套凭据黑名单。** 浏览器的操作者是人：用户在界面上主动打开
//!   `~/.ssh/config` 是正当需求。黑名单针对的是「Agent 把文件工具当成读凭据的入口」，
//!   所以 [`RemoteFileService::list`] / [`RemoteFileService::browse_read`] 只受上限约束，
//!   不查黑名单 —— 这不是漏掉的一步。
//! - **没有补丁或追加语义**：`write` 是覆盖写，与 README 的说明一致。

pub mod limits;
pub mod policy;
pub mod service;

pub use limits::{
    decode_bounded, BoundedRead, BROWSER_READ_BYTES, DEFAULT_AGENT_READ_BYTES,
    MAX_AGENT_READ_BYTES, MAX_AGENT_WRITE_BYTES, MAX_REMOTE_PATH_CHARS,
};
pub use policy::{is_agent_blocked_path, validate_remote_path, AGENT_PATH_POLICY_MESSAGE};
pub use service::{
    AgentReadRequest, AgentWriteRequest, Error, ListedEntry, RemoteEntry, RemoteFileService,
    RemoteFileTransport, RemoteListing, RemoteRead, RemoteWrite, Result, TransportError,
    TransportResult,
};
