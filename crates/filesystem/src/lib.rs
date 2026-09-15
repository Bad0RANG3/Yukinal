//! yukinal-filesystem — 远端文件能力的唯一实现处：路径规则、字节/字符上限，以及有界的
//! `list` / `read` / `write` / `edit`。
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
//! - [`limits`]：路径长度上限、Agent 读写上限、编辑上限、UI 浏览器 1 MiB 上限，以及有界读取的
//!   解码（[`decode_bounded`]）。
//! - [`revision`]：一次读取的内容 revision（SHA-256，小写十六进制）。`read` 返回它，`edit`
//!   在写回之前重算并比较 —— 两侧同源，比较才有意义。
//! - [`service`]：传输 trait [`RemoteFileTransport`] 与套在它外面的 [`RemoteFileService`]。
//!   策略与上限在服务里生效，失败是类型化的 [`Error`]；`invalid_input` /
//!   `denied_by_policy` / `transport` 这些**码**由上层映射（见下）。
//!
//! # 两个写工具为什么都在
//! - [`RemoteFileService::agent_write`] 是**覆盖写**：整份内容由调用方给出，写下去就是全部。
//!   它存在的理由是「创建文件」和「我确实要整份换掉」。它不带守卫，也不会自己先读一遍再合并
//!   —— 那种「隐式读改写」恰恰是它今天会覆盖掉并发修改的原因，把它改成补丁语义不会解决这个
//!   问题，只会让「整份替换」这件事没有工具可用。
//! - [`RemoteFileService::agent_edit`] 是**有守卫的精确替换**（先读后改）：它要求
//!   `expectedRevision` 与文件当前内容一致，且 `oldString` 恰好出现一次。
//!
//! # 编辑的保证边界（诚实版本）
//! `edit` 的前置条件是「文件的内容还是我读过的那一份」，它拦得住的是：读完之后别人改过、
//! 模型拿着过期内容写回、`oldString` 指不到唯一一处。普通文件的替换阶段会先写同目录临时
//! 文件再 rename，因此读者不会看到半写状态；但整条守卫仍是 check-then-replace，不是
//! compare-and-swap，检查与替换之间的并发写入仍可能被覆盖。两份内容大于读上限的文件根本不允许编辑（
//! [`MAX_AGENT_EDIT_BYTES`]），因为截断读取的 revision 只描述前缀，而写回前缀就是截断用户的
//! 文件：那条规则是数据丢失的防线，不是可调参数。
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
//!   一起）。`edit` 的两种新拒绝（revision 过期、文件超大）因此也落在已有的 `invalid_input`
//!   上，没有引入新的码。
//! - **不给 UI 浏览器套凭据黑名单。** 浏览器的操作者是人：用户在界面上主动打开
//!   `~/.ssh/config` 是正当需求。黑名单针对的是「Agent 把文件工具当成读凭据的入口」，
//!   所以 [`RemoteFileService::list`] / [`RemoteFileService::browse_read`] 只受上限约束，
//!   不查黑名单 —— 这不是漏掉的一步。

pub mod limits;
pub mod policy;
pub mod revision;
pub mod service;

pub use limits::{
    decode_bounded, BoundedRead, BROWSER_READ_BYTES, DEFAULT_AGENT_READ_BYTES,
    MAX_AGENT_EDIT_BYTES, MAX_AGENT_READ_BYTES, MAX_AGENT_WRITE_BYTES, MAX_REMOTE_PATH_CHARS,
};
pub use policy::{is_agent_blocked_path, validate_remote_path, AGENT_PATH_POLICY_MESSAGE};
pub use revision::{content_revision, is_content_revision};
pub use service::{
    AgentEditRequest, AgentReadRequest, AgentWriteRequest, Error, ListedEntry, RemoteEdit,
    RemoteEntry, RemoteEntryKind, RemoteFileService, RemoteFileTransport, RemoteListing,
    RemoteRead, RemoteStat, RemoteWrite, ReplaceError, ReplaceGuard, ReplacedFile, Result,
    TransportError, TransportResult,
};
