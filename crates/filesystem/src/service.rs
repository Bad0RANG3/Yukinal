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

use crate::limits::{
    decode_bounded, BROWSER_READ_BYTES, DEFAULT_AGENT_READ_BYTES, MAX_AGENT_READ_BYTES,
    MAX_AGENT_WRITE_BYTES,
};
use crate::policy::{self, AGENT_PATH_POLICY_MESSAGE};

/// 服务层结果。
pub type Result<T> = std::result::Result<T, Error>;

/// 传输层结果。
pub type TransportResult<T> = std::result::Result<T, TransportError>;

/// 传输层失败：连接 / SFTP channel / 取消等等。
///
/// 文案由实现方给出（桌面侧就是 `yukinal-core` 的错误字符串），本 crate 不解释、不包装它 ——
/// 宿主侧的 `transport_or_cancel` 依赖这段文字原样到达用户面前。
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct TransportError(String);

impl TransportError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// 文件能力的失败分类。
///
/// 分类而不是码：`Error::InvalidInput` 对应宿主侧的 `invalid_input`（可重试的入参问题），
/// `Error::DeniedByPolicy` 对应 `denied_by_policy`（策略拒绝，不可重试），
/// `Error::Transport` 交给 `transport_or_cancel`。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 路径形状或上限不合法。
    #[error("{0}")]
    InvalidInput(String),
    /// 路径命中凭据/进程密钥黑名单。
    #[error("{0}")]
    DeniedByPolicy(String),
    /// 传输失败；文案原样来自传输实现。
    #[error("{0}")]
    Transport(#[from] TransportError),
}

/// 传输层交出的原始目录项：名字、类型（`file` / `dir` / …）、字节数。
///
/// 路径拼接等归一化不在传输上做：不同传输给出的名字形式不一样，而「父路径 + 名字」的规则
/// 只有一条，应该只有一份（见 [`RemoteFileService::list`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedEntry {
    pub name: String,
    pub file_type: String,
    pub size: u64,
}

/// 文件能力需要的最小传输面。
///
/// 实现方负责「能用」：桌面侧的 `TerminalFileTransport` 在每次操作前确认 SSH 连接已建立
/// （缓存命中时只是一次查找），因此调用方不必先自己连接。
///
/// 参数名是 `server_id` 而不是 `target`：本能力按契约**只有远端**一种目标（`srv_` id，
/// 见 crate 文档）。将来若要有本地实现，那意味着先有一套本机策略，而不是在这里加一个分支。
pub trait RemoteFileTransport: Send + Sync {
    /// 列出目录项（未归一化，返回传输拿到的原始形状）。
    fn list(
        &self,
        server_id: &str,
        path: &str,
    ) -> impl std::future::Future<Output = TransportResult<Vec<ListedEntry>>> + Send;

    /// 读取至多 `max_bytes` 字节。实现方可以多返回**一**个字节来示意「还有更多」，
    /// 解码由 [`decode_bounded`] 统一处理。
    fn read_bounded(
        &self,
        server_id: &str,
        path: &str,
        max_bytes: usize,
    ) -> impl std::future::Future<Output = TransportResult<Vec<u8>>> + Send;

    /// 覆盖写入（没有补丁/追加语义）。
    fn write(
        &self,
        server_id: &str,
        path: &str,
        data: &[u8],
    ) -> impl std::future::Future<Output = TransportResult<()>> + Send;
}

/// Agent `filesystem.read` 的请求：不可伪造地携带「策略与上限已通过」。
///
/// 字段私有、唯一构造入口是 [`AgentReadRequest::check`]，所以 `RemoteFileService::agent_read`
/// 不可能收到一个没查过黑名单的路径。
///
/// 为什么把纯校验从 async 的读取里拆出来：宿主侧的顺序是「校验 → 取消/传输」，而**非法输入
/// 即使已经按下停止也必须报 `invalid_input`**（今天的顺序：形状 → 黑名单 → 上限都排在
/// `ensure_session` 与 `select!` 之前）。校验是同步的、可先行的，把它留在 `select!` 里面就
/// 会把这条顺序交给分支竞争的运气；反过来在命令层再抄一份规则又会重新长出第二份黑名单。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentReadRequest {
    path: String,
    max_bytes: usize,
}

impl AgentReadRequest {
    /// 校验路径与 `maxBytes`，并把缺省的读取上限解析成具体字节数。
    pub fn check(path: &str, max_bytes: Option<usize>) -> Result<Self> {
        policy::validate_remote_path(path).map_err(Error::InvalidInput)?;
        if policy::is_agent_blocked_path(path) {
            return Err(Error::DeniedByPolicy(AGENT_PATH_POLICY_MESSAGE.to_string()));
        }
        let max_bytes = max_bytes.unwrap_or(DEFAULT_AGENT_READ_BYTES);
        if !(1..=MAX_AGENT_READ_BYTES).contains(&max_bytes) {
            return Err(Error::InvalidInput(format!(
                "maxBytes must be between 1 and {MAX_AGENT_READ_BYTES}"
            )));
        }
        Ok(Self {
            path: path.to_string(),
            max_bytes,
        })
    }
}

/// Agent `filesystem.write` 的请求，同 [`AgentReadRequest`]：只能经 [`AgentWriteRequest::check`]
/// 构造。`content` 按值收下，避免为最大 512 KiB 的正文多复制一次。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentWriteRequest {
    path: String,
    content: String,
}

impl AgentWriteRequest {
    pub fn check(path: &str, content: String) -> Result<Self> {
        policy::validate_remote_path(path).map_err(Error::InvalidInput)?;
        if policy::is_agent_blocked_path(path) {
            return Err(Error::DeniedByPolicy(AGENT_PATH_POLICY_MESSAGE.to_string()));
        }
        if content.len() > MAX_AGENT_WRITE_BYTES {
            return Err(Error::InvalidInput(format!(
                "content must be at most {MAX_AGENT_WRITE_BYTES} bytes"
            )));
        }
        Ok(Self {
            path: path.to_string(),
            content,
        })
    }
}

/// 目录列表结果：请求的路径加上已归一化为绝对路径的条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteListing {
    pub path: String,
    pub entries: Vec<RemoteEntry>,
}

/// 一个目录项。`path` 是可直接再喂给 `read` 的绝对路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntry {
    pub name: String,
    pub path: String,
    pub file_type: String,
    pub size: u64,
}

/// 一次读取的结果（正文 + 是否被上限截断）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRead {
    pub path: String,
    pub content: String,
    pub truncated: bool,
}

/// 一次覆盖写的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteWrite {
    pub path: String,
    /// 写入的**字节**数（正文按 UTF-8 编码后的长度）。
    pub bytes_written: usize,
}

/// 远端文件服务：策略 + 上限 + 有界解码，套在一个 [`RemoteFileTransport`] 外面。
pub struct RemoteFileService<T> {
    transport: T,
}

impl<T: RemoteFileTransport> RemoteFileService<T> {
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    /// 目录列表（UI 浏览器用；Agent 目前没有列表工具）。
    ///
    /// 刻意不套用凭据黑名单，也不做路径形状校验：浏览器的操作者是人，而 Agent 的策略不是
    /// 给人用的（见 crate 文档）。这里唯一生效的规则是「名字归一化成绝对路径」。
    pub async fn list(&self, server_id: &str, path: &str) -> Result<RemoteListing> {
        let entries = self.transport.list(server_id, path).await?;
        Ok(RemoteListing {
            path: path.to_string(),
            entries: entries
                .into_iter()
                .map(|entry| RemoteEntry {
                    path: join_remote_path(path, &entry.name),
                    name: entry.name,
                    file_type: entry.file_type,
                    size: entry.size,
                })
                .collect(),
        })
    }

    /// UI 浏览器的读取：上限固定为 [`BROWSER_READ_BYTES`]，调用方无法要求更多，也不查黑名单。
    pub async fn browse_read(&self, server_id: &str, path: &str) -> Result<RemoteRead> {
        let bytes = self
            .transport
            .read_bounded(server_id, path, BROWSER_READ_BYTES)
            .await?;
        Ok(read_result(path, &bytes, BROWSER_READ_BYTES))
    }

    /// Agent `filesystem.read`：请求已经带过策略与上限，这里只做有界读取。
    pub async fn agent_read(
        &self,
        server_id: &str,
        request: &AgentReadRequest,
    ) -> Result<RemoteRead> {
        let bytes = self
            .transport
            .read_bounded(server_id, &request.path, request.max_bytes)
            .await?;
        Ok(read_result(&request.path, &bytes, request.max_bytes))
    }

    /// Agent `filesystem.write`（覆盖写）。
    pub async fn agent_write(
        &self,
        server_id: &str,
        request: &AgentWriteRequest,
    ) -> Result<RemoteWrite> {
        self.transport
            .write(server_id, &request.path, request.content.as_bytes())
            .await?;
        Ok(RemoteWrite {
            path: request.path.clone(),
            bytes_written: request.content.len(),
        })
    }
}

fn read_result(path: &str, bytes: &[u8], max_bytes: usize) -> RemoteRead {
    let decoded = decode_bounded(bytes, max_bytes);
    RemoteRead {
        path: path.to_string(),
        content: decoded.content,
        truncated: decoded.truncated,
    }
}

/// 父路径 + 条目名 → 绝对路径。
///
/// 三个分支对应三种真实输入：根目录（`/` 已经带斜杠）、带尾斜杠的目录、普通目录。直接
/// `format!("{parent}/{name}")` 会在前两种情况下产出 `//hosts` 与 `/etc//hosts`，
/// 而 SFTP 对这些形式的解释依赖服务端实现。
fn join_remote_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else if parent.ends_with('/') {
        format!("{parent}{name}")
    } else {
        format!("{parent}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{
        join_remote_path, AgentReadRequest, AgentWriteRequest, Error, ListedEntry,
        RemoteFileService, RemoteFileTransport, TransportError, TransportResult,
    };
    use crate::limits::{
        BROWSER_READ_BYTES, DEFAULT_AGENT_READ_BYTES, MAX_AGENT_READ_BYTES, MAX_AGENT_WRITE_BYTES,
    };
    use crate::policy::AGENT_PATH_POLICY_MESSAGE;

    /// 传输调用记录。用 `Arc` 是因为传输移交给服务之后，测试还要能读到它 ——
    /// 「被拦下的路径没有到达传输」只能靠这份记录证明，光看错误类型看不出来。
    #[derive(Clone, Default)]
    struct CallLog {
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl CallLog {
        fn record(&self, call: String) {
            self.calls.lock().expect("call log lock").push(call);
        }

        fn snapshot(&self) -> Vec<String> {
            self.calls.lock().expect("call log lock").clone()
        }
    }

    /// 内存假传输：返回预设数据、记录每次调用，并可选地失败。
    #[derive(Default)]
    struct FakeTransport {
        log: CallLog,
        entries: Vec<ListedEntry>,
        content: Vec<u8>,
        failure: Option<&'static str>,
    }

    impl FakeTransport {
        fn with_content(content: &[u8]) -> Self {
            Self {
                content: content.to_vec(),
                ..Self::default()
            }
        }

        fn log(&self) -> CallLog {
            self.log.clone()
        }
    }

    impl RemoteFileTransport for FakeTransport {
        async fn list(&self, server_id: &str, path: &str) -> TransportResult<Vec<ListedEntry>> {
            self.log.record(format!("list {server_id} {path}"));
            if let Some(message) = self.failure {
                return Err(TransportError::new(message));
            }
            Ok(self.entries.clone())
        }

        async fn read_bounded(
            &self,
            server_id: &str,
            path: &str,
            max_bytes: usize,
        ) -> TransportResult<Vec<u8>> {
            self.log
                .record(format!("read {server_id} {path} @{max_bytes}"));
            if let Some(message) = self.failure {
                return Err(TransportError::new(message));
            }
            Ok(self.content.clone())
        }

        async fn write(&self, server_id: &str, path: &str, data: &[u8]) -> TransportResult<()> {
            self.log
                .record(format!("write {server_id} {path} {}b", data.len()));
            if let Some(message) = self.failure {
                return Err(TransportError::new(message));
            }
            Ok(())
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().expect("runtime")
    }

    #[test]
    fn joins_posix_paths_without_double_slashes() {
        assert_eq!(join_remote_path("/etc", "hosts"), "/etc/hosts");
        assert_eq!(join_remote_path("/", "hosts"), "/hosts");
        assert_eq!(join_remote_path("/etc/", "hosts"), "/etc/hosts");
    }

    #[test]
    fn listing_normalises_entry_paths_against_the_directory_it_listed() {
        let transport = FakeTransport {
            entries: vec![
                ListedEntry {
                    name: "hosts".to_string(),
                    file_type: "file".to_string(),
                    size: 42,
                },
                ListedEntry {
                    name: "nginx".to_string(),
                    file_type: "dir".to_string(),
                    size: 4_096,
                },
            ],
            ..FakeTransport::default()
        };
        let log = transport.log();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let listing = service.list("srv_1", "/etc").await.expect("list /etc");
            assert_eq!(listing.path, "/etc");
            assert_eq!(listing.entries[0].path, "/etc/hosts");
            assert_eq!(listing.entries[0].file_type, "file");
            assert_eq!(listing.entries[1].path, "/etc/nginx");
            assert_eq!(listing.entries[1].size, 4_096);

            let root = service.list("srv_1", "/").await.expect("list /");
            assert_eq!(root.entries[0].path, "/hosts");

            let trailing = service.list("srv_1", "/etc/").await.expect("list /etc/");
            assert_eq!(trailing.entries[0].path, "/etc/hosts");
        });

        // 归一化发生在服务里，传输只看到调用方给的原文。
        assert_eq!(
            log.snapshot(),
            vec!["list srv_1 /etc", "list srv_1 /", "list srv_1 /etc/"]
        );
    }

    #[test]
    fn an_over_cap_read_request_is_refused_before_the_transport_is_touched() {
        let transport = FakeTransport::with_content(b"PORT=8080\n");
        let log = transport.log();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            for max_bytes in [0, MAX_AGENT_READ_BYTES + 1, usize::MAX] {
                match AgentReadRequest::check("/etc/app.env", Some(max_bytes)) {
                    // 这一支在一次通过的运行里不会成立；真成立就会在这里碰到传输，
                    // 下面那次日志断言会立刻失败。
                    Ok(request) => {
                        let _ = service.agent_read("srv_1", &request).await;
                        panic!("maxBytes = {max_bytes} must not yield a request");
                    }
                    Err(error) => assert_eq!(
                        error.to_string(),
                        format!("maxBytes must be between 1 and {MAX_AGENT_READ_BYTES}"),
                        "maxBytes = {max_bytes}"
                    ),
                }
            }
        });

        assert!(
            log.snapshot().is_empty(),
            "a rejected request reached the transport: {:?}",
            log.snapshot()
        );
    }

    #[test]
    fn agent_read_uses_the_documented_default_cap_and_truncates_at_it() {
        let transport = FakeTransport::with_content(b"0123456789abcdefX");
        let log = transport.log();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let request = AgentReadRequest::check("/etc/app.env", None).expect("request");
            let read = service.agent_read("srv_1", &request).await.expect("read");
            assert_eq!(read.path, "/etc/app.env");
            assert_eq!(read.content, "0123456789abcdefX");
            assert!(!read.truncated);

            let request = AgentReadRequest::check("/etc/app.env", Some(16)).expect("request");
            let read = service.agent_read("srv_1", &request).await.expect("read");
            assert_eq!(read.content, "0123456789abcdef");
            assert!(read.truncated);
        });

        // 默认上限是**具体字节数**发给传输的，缺省不在传输里做。
        assert_eq!(
            log.snapshot(),
            vec![
                format!("read srv_1 /etc/app.env @{DEFAULT_AGENT_READ_BYTES}"),
                "read srv_1 /etc/app.env @16".to_string(),
            ]
        );
    }

    #[test]
    fn agent_write_enforces_the_write_cap_and_counts_bytes_not_characters() {
        let transport = FakeTransport::default();
        let log = transport.log();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let at_cap = "x".repeat(MAX_AGENT_WRITE_BYTES);
            let request =
                AgentWriteRequest::check("/var/app/out.txt", at_cap).expect("at-cap request");
            let write = service.agent_write("srv_1", &request).await.expect("write");
            assert_eq!(write.path, "/var/app/out.txt");
            assert_eq!(write.bytes_written, MAX_AGENT_WRITE_BYTES);

            // 上限按字节：两个字符的 "文a" 是 4 字节。
            let request = AgentWriteRequest::check("/var/app/uni.txt", "文a".to_string())
                .expect("unicode request");
            let write = service.agent_write("srv_1", &request).await.expect("write");
            assert_eq!(write.bytes_written, 4);

            let over_cap = "x".repeat(MAX_AGENT_WRITE_BYTES + 1);
            let error = AgentWriteRequest::check("/var/app/out.txt", over_cap)
                .expect_err("over-cap content");
            assert_eq!(
                error.to_string(),
                format!("content must be at most {MAX_AGENT_WRITE_BYTES} bytes")
            );
        });

        assert_eq!(
            log.snapshot(),
            vec![
                format!("write srv_1 /var/app/out.txt {MAX_AGENT_WRITE_BYTES}b"),
                "write srv_1 /var/app/uni.txt 4b".to_string(),
            ]
        );
    }

    #[test]
    fn a_blocked_path_never_reaches_the_transport() {
        let blocked = [
            "/home/deploy/.ssh/id_ed25519",
            "/home/deploy/.aws/credentials",
            "/srv/app/.env.production",
            "/run/secrets/provider-token",
            "/proc/1/environ",
            "/etc/ssl/private/service.key",
            "/etc/shadow",
        ];
        let transport = FakeTransport::with_content(b"secret");
        let log = transport.log();
        let service = RemoteFileService::new(transport);

        // 到达传输的唯一途径是一个校验过的请求。这些路径都构造不出请求，所以下面两个
        // `Ok` 分支在一次通过的运行里是死代码 —— 一旦有谁把策略放松了，它会立刻在这里
        // 碰到传输并把路径写进日志，收尾的断言随即失败。
        runtime().block_on(async {
            for path in blocked {
                match AgentReadRequest::check(path, None) {
                    Ok(request) => {
                        let _ = service.agent_read("srv_1", &request).await;
                        panic!("{path} must not yield a read request");
                    }
                    Err(error) => match error {
                        Error::DeniedByPolicy(message) => {
                            assert_eq!(message, AGENT_PATH_POLICY_MESSAGE, "{path}")
                        }
                        other => panic!("{path}: expected a policy denial, got {other:?}"),
                    },
                }
                match AgentWriteRequest::check(path, "x".to_string()) {
                    Ok(request) => {
                        let _ = service.agent_write("srv_1", &request).await;
                        panic!("{path} must not yield a write request");
                    }
                    Err(error) => match error {
                        Error::DeniedByPolicy(message) => {
                            assert_eq!(message, AGENT_PATH_POLICY_MESSAGE, "{path}")
                        }
                        other => panic!("{path}: expected a policy denial, got {other:?}"),
                    },
                }
            }
        });

        assert!(
            log.snapshot().is_empty(),
            "a blocked path reached the transport: {:?}",
            log.snapshot()
        );
    }

    #[test]
    fn the_credential_policy_is_decided_before_the_shape_and_limit_checks() {
        // 顺序是对外行为的一部分，宿主侧的失败码依赖它：今天「形状 → 黑名单 → 上限」，
        // 所以带黑名单的路径即使 maxBytes 也非法，报的仍是 denied_by_policy。
        match AgentReadRequest::check("/home/deploy/.ssh/id_rsa", Some(0))
            .expect_err("blocked path with a bad cap")
        {
            Error::DeniedByPolicy(message) => assert_eq!(message, AGENT_PATH_POLICY_MESSAGE),
            other => panic!("expected a policy denial, got {other:?}"),
        }

        match AgentWriteRequest::check("/srv/app/.env", "x".repeat(MAX_AGENT_WRITE_BYTES + 1))
            .expect_err("blocked path with oversized content")
        {
            Error::DeniedByPolicy(message) => assert_eq!(message, AGENT_PATH_POLICY_MESSAGE),
            other => panic!("expected a policy denial, got {other:?}"),
        }

        // 形状校验排在黑名单之前：相对路径先报 invalid_input。
        match AgentReadRequest::check("relative/.env", None).expect_err("relative path") {
            Error::InvalidInput(message) => assert_eq!(message, "remote path must be absolute"),
            other => panic!("expected an invalid-input error, got {other:?}"),
        }
    }

    #[test]
    fn the_ui_browser_read_uses_its_own_cap_and_is_not_policy_blocked() {
        // 浏览器不是 Agent：`~/.ssh/id_rsa` 这类路径由人来打开是正当的。哪一天这里开始拒绝，
        // 说明有人把 Agent 的策略套到了 UI 上 —— 那是行为变更，不是加固。
        let transport = FakeTransport::with_content(b"Host web\n");
        let log = transport.log();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let read = service
                .browse_read("srv_1", "/home/deploy/.ssh/id_rsa")
                .await
                .expect("browser read");
            assert_eq!(read.path, "/home/deploy/.ssh/id_rsa");
            assert_eq!(read.content, "Host web\n");
            assert!(!read.truncated);

            let listing = service
                .list("srv_1", "/home/deploy/.ssh")
                .await
                .expect("browser list");
            assert_eq!(listing.path, "/home/deploy/.ssh");
        });

        assert_eq!(
            log.snapshot(),
            vec![
                format!("read srv_1 /home/deploy/.ssh/id_rsa @{BROWSER_READ_BYTES}"),
                "list srv_1 /home/deploy/.ssh".to_string(),
            ]
        );
    }

    #[test]
    fn transport_failures_keep_their_own_text() {
        const MESSAGE: &str = "no session cached for server `srv_1`; connect first";
        let transport = FakeTransport {
            failure: Some(MESSAGE),
            ..FakeTransport::default()
        };
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let request = AgentReadRequest::check("/etc/app.env", None).expect("request");
            match service.agent_read("srv_1", &request).await {
                Err(Error::Transport(error)) => assert_eq!(error.to_string(), MESSAGE),
                other => panic!("expected a transport error, got {other:?}"),
            }

            let request =
                AgentWriteRequest::check("/etc/app.env", "x".to_string()).expect("request");
            match service.agent_write("srv_1", &request).await {
                Err(Error::Transport(error)) => assert_eq!(error.to_string(), MESSAGE),
                other => panic!("expected a transport error, got {other:?}"),
            }

            match service.browse_read("srv_1", "/etc/app.env").await {
                Err(Error::Transport(error)) => assert_eq!(error.to_string(), MESSAGE),
                other => panic!("expected a transport error, got {other:?}"),
            }

            match service.list("srv_1", "/etc").await {
                Err(Error::Transport(error)) => assert_eq!(error.to_string(), MESSAGE),
                other => panic!("expected a transport error, got {other:?}"),
            }
        });
    }
}
