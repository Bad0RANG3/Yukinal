//! 一个 MCP 服务进程的句柄：派生、三个 stdio 泵、握手、`tools/list`、`tools/call`、
//! 每次请求的超时、退出记录与有界的 stderr 尾部。

use std::collections::{HashMap, HashSet, VecDeque};
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard as StdMutexGuard, PoisonError};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

use super::config::McpStdioConfig;
use super::descriptor::{McpExitRecord, McpToolDescriptor, McpToolResult};
use super::error::McpError;
use super::wire::{self, McpInitialize};
use super::{redacted, truncated, STDERR_TAIL_LINES};

/// 诊断尾部（stdout 上的噪声、被忽略的通知与请求）保留多少行。
///
/// 比 stderr 尾部小：这些是协议噪声，一个持续输出的服务端不该把 stderr 的排障信息挤出去。
const DIAGNOSTIC_TAIL_LINES: usize = 50;

/// 观察子进程退出的间隔。
///
/// 一次工具调用可能正等着一个已经死掉的进程：等它的时间不该比人察觉到的还长。100ms 让
/// 「进程死了」在单次调用预算内就被发现，同时每个服务端只有一条这样睡着的任务。
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// 关闭时给「体面退出」的宽限期：关掉 stdin 之后等它自己走。
///
/// 一秒够一个规矩的服务端看到 EOF 并离开；再长，「停止」按钮就开始显得坏掉了 ——
/// 反正不理会 EOF 的服务端接下来也是被强杀。
const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);

/// 强杀之后再等多久才算「确认它消失了」。
///
/// `start_kill` 在本机已经是最强的动作（Unix 上是 SIGKILL，Windows 上是 TerminateProcess），
/// 所以剩下的只有操作系统回收的时间。五秒之后就如实上报 `unreaped`：本机发不出更强的信号，
/// 继续等下去只会把调用方挂住。
const SHUTDOWN_KILL_GRACE: Duration = Duration::from_secs(5);

/// 写 stdin 失败后，等退出记录出现的时间。
///
/// 写失败最常见的成因就是「子进程已经不在了」，那时候真正的错误是「它死了」而不是「管道断了」：
/// 等一下记录，能把退出原因报出来就报出来。这段等待是有界的，所以写失败最多慢 500ms，
/// 不会变成一个隐藏的挂起点。
const WRITE_FAILURE_EXIT_GRACE: Duration = Duration::from_millis(500);

/// `tools/list` 最多跟随多少页游标。
///
/// 一个一直回游标的服务端不能让我们无限翻页（每次翻页都是一次带超时的请求）。16 页远超任何
/// 真实服务器的工具表规模，这个数只是一个循环断点，不是一条策略。
const TOOLS_LIST_MAX_PAGES: usize = 16;

/// CREATE_NO_WINDOW：MCP 服务进程不许在 Windows 上闪一个控制台窗口（sidecar 用的是同一个标志）。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 有界尾部：满了就丢最旧的一行。诊断信息不能因为服务端话多就吃掉宿主的内存。
fn push_bounded(tail: &mut VecDeque<String>, line: String, cap: usize) {
    if tail.len() >= cap {
        tail.pop_front();
    }
    tail.push_back(line);
}

/// 这些锁保护的都是记账数据（待响应表、名字表、日志尾部、退出记录）。中毒只意味着某条线程
/// 在持锁时 panic 过；继续用里面的值，比把「日志尾部」变成第二个错误来源要好。
fn lock_or_recover<T>(mutex: &StdMutex<T>) -> StdMutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 一个在途请求为什么会以失败结束。
///
/// 三种原因分开保留，因为调用方要能区分「服务端说这次执行失败了」「进程没了」
/// 「我们把它关了」——它们的处置方式完全不同。
#[derive(Debug, Clone)]
enum PendingFailure {
    Remote(wire::RemoteError),
    Exited(McpExitRecord),
    Closed,
}

/// 服务进程的静态事实 + 握手结果。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerInfo {
    pub server_id: String,
    pub pid: u32,
    pub program: String,
    pub started_at: String,
    /// 握手结果。`None` 表示进程起来了但 `initialize` 还没成功 —— 这个窗口真实存在，
    /// 所以类型里必须有它，而不是用「版本是空字符串」来暗示。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handshake: Option<McpInitialize>,
}

/// 一次 [`crate::mcp::McpSupervisor::start`] 的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct McpServerStart {
    pub info: McpServerInfo,
    pub tool_count: usize,
    /// False 表示这次调用真的派生了进程。
    pub already_running: bool,
}

/// 一个 MCP 服务器在某时刻的对外状态。状态查询永远有答案：没管过的 id 也是一个答案
/// （「没在跑」），所以调用方不需要区分「没有这个服务器」和「查询失败」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStatus {
    pub server_id: String,
    pub running: bool,
    /// 只在它真的跑着时给出：把一个已经结束的进程的 pid 交给界面，就是在展示一个不存在的进程。
    pub pid: Option<u32>,
    pub program: Option<String>,
    pub started_at: Option<String>,
    pub protocol_version: Option<String>,
    pub server_name: Option<String>,
    pub server_version: Option<String>,
    pub tool_count: usize,
    /// 「怎么死的」留在状态里，直到下一次显式启动把它盖掉 —— 崩溃必须比崩溃后的安静更显眼。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_exit: Option<McpExitRecord>,
    /// 子进程 stderr 的有界尾部（[`STDERR_TAIL_LINES`] 行）。**已截断并脱敏**：见模块头
    /// 最后一条（它来自一个我们并不信任的进程，而尾部是要给用户看的）。
    pub stderr_tail: Vec<String>,
    /// 协议噪声的有界尾部：stdout 上的非 JSON 行、被忽略的通知与请求。
    pub diagnostics: Vec<String>,
}

impl McpServerStatus {
    /// 一个从未被这个 supervisor 管过的服务器：没在跑，也没有任何过程可报告。
    pub(super) fn untracked(server_id: &str) -> Self {
        Self {
            server_id: truncated(server_id),
            running: false,
            pid: None,
            program: None,
            started_at: None,
            protocol_version: None,
            server_name: None,
            server_version: None,
            tool_count: 0,
            last_exit: None,
            stderr_tail: Vec::new(),
            diagnostics: Vec::new(),
        }
    }
}

/// 关闭一个服务进程时到底发生了什么。
///
/// 三个字段各自都可能为假，所以它们值得被返回：调用方不该以为「函数返回了，就一定干净了」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShutdownReport {
    /// False 表示这个服务本来就不在跑了（崩过，或者已经被关过）。
    pub was_running: bool,
    /// True 表示「关 stdin，等它自己走」没成功，最后动用了强杀。
    pub killed: bool,
    /// True 表示强杀之后也没能在预算内确认它消失。到这一步本机已经发不出更强的信号，
    /// 所以上报事实，而不是把调用方挂在那里等一个可能永远不来的回收。
    pub unreaped: bool,
}

#[derive(Debug)]
struct Inner {
    config: McpStdioConfig,
    pid: u32,
    started_at: String,
    handshake: StdMutex<Option<wire::McpInitialize>>,
    /// `None` 表示 stdin 已经关掉（`shutdown` 的体面退出路径）。
    stdin: AsyncMutex<Option<ChildStdin>>,
    child: StdMutex<Child>,
    pending: StdMutex<HashMap<i64, oneshot::Sender<Result<Value, PendingFailure>>>>,
    tools: StdMutex<Vec<McpToolDescriptor>>,
    exit: StdMutex<Option<McpExitRecord>>,
    stderr_tail: StdMutex<VecDeque<String>>,
    diagnostics: StdMutex<VecDeque<String>>,
    next_id: AtomicI64,
    exited: AtomicBool,
}

/// 与一个服务进程对话的句柄。克隆很便宜：每个克隆都指向同一个进程。
#[derive(Debug, Clone)]
pub struct McpServerHandle {
    inner: Arc<Inner>,
}

impl McpServerHandle {
    /// 派生进程并接上三个 stdio。
    ///
    /// **不**握手：[`crate::mcp::McpSupervisor::start`] 决定握手失败时怎么收场（杀掉，而不是
    /// 留半个活着的进程），而且 `initialize` 必须是子进程看到的第一帧。
    pub async fn spawn(config: &McpStdioConfig) -> Result<Self, McpError> {
        // 配置由 `from_server_config` / `new` 校验过，但字段是公开的：真正不能被绕过的是
        // 派生进程的这一处。
        config.validate()?;

        let launch_error = |reason: String| McpError::Launch {
            server_id: truncated(&config.server_id),
            program: config.program.display().to_string(),
            reason,
        };

        let mut command = Command::new(&config.program);
        command
            .args(&config.args)
            .envs(config.env.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // 孤儿进程是不可接受的（docs/boundaries/mcp.md 的「进程生命周期仍然归 Rust」）：
            // 句柄整体消失时，子进程必须跟着走。
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(CREATE_NO_WINDOW);

        let mut child = command
            .spawn()
            .map_err(|error| launch_error(error.to_string()))?;
        let pid = child
            .id()
            .ok_or_else(|| launch_error("the child has no pid".to_string()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| launch_error("stdin was not piped".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| launch_error("stdout was not piped".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| launch_error("stderr was not piped".to_string()))?;

        let handle = Self {
            inner: Arc::new(Inner {
                config: config.clone(),
                pid,
                started_at: yukinal_time::iso8601_now(),
                handshake: StdMutex::new(None),
                stdin: AsyncMutex::new(Some(stdin)),
                child: StdMutex::new(child),
                pending: StdMutex::new(HashMap::new()),
                tools: StdMutex::new(Vec::new()),
                exit: StdMutex::new(None),
                stderr_tail: StdMutex::new(VecDeque::new()),
                diagnostics: StdMutex::new(VecDeque::new()),
                next_id: AtomicI64::new(1),
                exited: AtomicBool::new(false),
            }),
        };

        handle.start_stdout_pump(stdout);
        handle.start_stderr_pump(stderr);
        handle.start_exit_watcher();
        Ok(handle)
    }

    /// stdout：一行一帧。读坏的、太大的、不是 JSON 的行都记成诊断，只有 EOF 与读错误
    /// 才结束这条任务 —— 一个讲得不好的服务端不该让整个会话失效。
    fn start_stdout_pump(&self, stdout: tokio::process::ChildStdout) {
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(stdout);
            loop {
                match wire::read_frame(&mut reader, wire::MAX_FRAME_BYTES).await {
                    Ok(wire::FrameRead::Line(line)) => match weak.upgrade() {
                        // 强引用只在这一次同步的分发期间存在：见下面 `Weak` 的说明。
                        Some(inner) => inner.dispatch_line(&line),
                        None => break,
                    },
                    Ok(wire::FrameRead::TooLong { limit }) => match weak.upgrade() {
                        Some(inner) => inner.remember_diagnostic(format!(
                            "dropped a stdout line longer than {limit} bytes"
                        )),
                        None => break,
                    },
                    Ok(wire::FrameRead::Eof) => break,
                    Err(reason) => {
                        if let Some(inner) = weak.upgrade() {
                            inner.remember_diagnostic(reason);
                        }
                        break;
                    }
                }
            }
        });
    }

    /// stderr：排障用的有界尾部。它**不是**协议通道，所以怎么读都不会让协议错位。
    fn start_stderr_pump(&self, stderr: tokio::process::ChildStderr) {
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(stderr);
            loop {
                match wire::read_frame(&mut reader, wire::MAX_FRAME_BYTES).await {
                    Ok(wire::FrameRead::Line(line)) => match weak.upgrade() {
                        Some(inner) => inner.remember_stderr(&line),
                        None => break,
                    },
                    Ok(wire::FrameRead::TooLong { limit }) => match weak.upgrade() {
                        Some(inner) => inner.remember_stderr(&format!(
                            "[one stderr line longer than {limit} bytes was dropped]"
                        )),
                        None => break,
                    },
                    Ok(wire::FrameRead::Eof) => break,
                    Err(_) => break,
                }
            }
        });
    }

    /// 退出观察者，同时也是**唯一**的回收者。
    ///
    /// 只有一个回收者是有意的：两个任务同时对同一个 `Child` 调 `try_wait`，会有一个永远拿不到
    /// 状态。`shutdown` 因此等的是退出**记录**，不是自己去 `wait`。
    fn start_exit_watcher(&self) {
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(EXIT_POLL_INTERVAL).await;
                let Some(inner) = weak.upgrade() else { break };
                let observed = {
                    let mut child = lock_or_recover(&inner.child);
                    child.try_wait().ok().flatten()
                };
                if let Some(status) = observed {
                    inner.record_exit(status);
                    break;
                }
            }
        });
    }

    #[must_use]
    pub fn server_id(&self) -> &str {
        &self.inner.config.server_id
    }

    /// 内部名里用的段（ADR 0004），由 serverId 规范化而来。
    #[must_use]
    pub fn segment(&self) -> &str {
        &self.inner.config.segment
    }

    #[must_use]
    pub fn pid(&self) -> u32 {
        self.inner.pid
    }

    #[must_use]
    pub fn request_timeout(&self) -> Duration {
        self.inner.config.request_timeout
    }

    /// 进程是否还在跑。崩溃或已关闭之后是 false，且**不会**自己变回 true。
    #[must_use]
    pub fn is_running(&self) -> bool {
        !self.inner.exited.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn info(&self) -> McpServerInfo {
        McpServerInfo {
            server_id: self.inner.config.server_id.clone(),
            pid: self.inner.pid,
            program: self.inner.config.program.display().to_string(),
            started_at: self.inner.started_at.clone(),
            handshake: lock_or_recover(&self.inner.handshake).clone(),
        }
    }

    #[must_use]
    pub fn handshake(&self) -> Option<wire::McpInitialize> {
        lock_or_recover(&self.inner.handshake).clone()
    }

    /// 最后一次 `tools/list` 的结果（缓存）。没有刷新过就是空表。
    #[must_use]
    pub fn tools(&self) -> Vec<McpToolDescriptor> {
        lock_or_recover(&self.inner.tools).clone()
    }

    #[must_use]
    pub fn last_exit(&self) -> Option<McpExitRecord> {
        lock_or_recover(&self.inner.exit).clone()
    }

    #[must_use]
    pub fn stderr_tail(&self) -> Vec<String> {
        lock_or_recover(&self.inner.stderr_tail)
            .iter()
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn diagnostics(&self) -> Vec<String> {
        lock_or_recover(&self.inner.diagnostics)
            .iter()
            .cloned()
            .collect()
    }

    /// `initialize` 握手，然后按规范发 `notifications/initialized`。
    ///
    /// 版本协商在 [`wire::parse_initialize`] 里：服务端回一个我们不说的版本时，这里会以
    /// [`McpError::UnsupportedProtocolVersion`] 失败，而不是「大概兼容」地继续。
    pub async fn initialize(&self, timeout: Duration) -> Result<wire::McpInitialize, McpError> {
        let params = json!({
            "protocolVersion": wire::PREFERRED_PROTOCOL_VERSION,
            // 客户端能力**如实**为空：本模块一个客户端能力都没实现，就一个都不声明
            // （docs/boundaries/mcp.md 的「不要暗示已经可用」：能力报告必须是事实，不是意图）。
            "capabilities": {},
            "clientInfo": { "name": wire::CLIENT_NAME, "version": wire::CLIENT_VERSION },
        });
        let result = self
            .request(wire::METHOD_INITIALIZE, params, timeout)
            .await?;
        let handshake = wire::parse_initialize(self.server_id(), &result)?;
        // 规范要求：初始化成功后客户端必须发这条通知，之后服务端才会正常服务。
        // 它是通知（无 id），所以不注册待响应项，也不等回答。
        self.notify(wire::METHOD_INITIALIZED_NOTIFICATION, json!({}))
            .await?;
        *lock_or_recover(&self.inner.handshake) = Some(handshake.clone());
        Ok(handshake)
    }

    /// 发一个通知：有 method、没有 id，按规范不期待回答。
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        if let Some(exit) = self.last_exit() {
            return Err(self.exited_error(exit));
        }
        if !self.is_running() {
            return Err(self.not_running_error());
        }
        self.write_frame(&wire::notification_frame(method, params))
            .await
    }

    /// `tools/list`：跟随游标把整张工具表取回来，并刷新句柄里的缓存。
    ///
    /// 任何一个名字不合法 —— 或者两个远端拼写落到同一个内部段 —— 都会让**整张表**被拒绝，
    /// 而不是丢掉那一条：部分注册会让「这个服务器有哪些工具」取决于解析顺序，而 ADR 0004
    /// 要求的正是一次说得清的导入失败。
    pub async fn list_tools(&self, timeout: Duration) -> Result<Vec<McpToolDescriptor>, McpError> {
        let mut tools: Vec<McpToolDescriptor> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut cursor: Option<String> = None;
        let mut pages = 0usize;

        loop {
            if pages >= TOOLS_LIST_MAX_PAGES {
                return Err(McpError::Protocol {
                    server_id: self.server_id().to_string(),
                    reason: format!(
                        "tools/list still answered with a cursor after {TOOLS_LIST_MAX_PAGES} pages"
                    ),
                });
            }
            pages += 1;

            // 游标是远端给的、原样回给它的不透明值：它从不参与名字，长度由帧上限兜底。
            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let result = self
                .request(wire::METHOD_TOOLS_LIST, params, timeout)
                .await?;
            let (page, next) = wire::parse_tools_page(self.server_id(), &result)?;
            for tool in page {
                if !seen.insert(tool.name.clone()) {
                    // `read_file` 与 `read-file` 会落到同一个内部段：这就是 ADR 0004 要求
                    // 在导入期发现的遮蔽。
                    return Err(McpError::ToolNameCollision {
                        server_id: self.server_id().to_string(),
                        name: tool.name,
                    });
                }
                tools.push(tool);
            }

            match next {
                None => {
                    *lock_or_recover(&self.inner.tools) = tools.clone();
                    return Ok(tools);
                }
                Some(next) => cursor = Some(next),
            }
        }
    }

    /// `tools/call`。
    ///
    /// 只能调用服务端**声明过**的工具，并且按它声明的拼写去调（`remote_name`）。返回的
    /// 内容是不可信数据（docs/boundaries/mcp.md 的「描述文本一律视为不可信数据」）：本模块只搬运它。
    pub async fn call_tool(
        &self,
        tool: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Result<McpToolResult, McpError> {
        let descriptor = self
            .tools()
            .into_iter()
            .find(|descriptor| descriptor.name == tool)
            .ok_or_else(|| McpError::UnknownTool {
                server_id: self.server_id().to_string(),
                tool: truncated(tool),
            })?;
        if !arguments.is_object() {
            return Err(McpError::InvalidArguments {
                server_id: self.server_id().to_string(),
                tool: descriptor.name,
            });
        }

        let params = json!({ "name": descriptor.call_name(), "arguments": arguments });
        let result = self
            .request(wire::METHOD_TOOLS_CALL, params, timeout)
            .await?;
        wire::parse_tools_call(self.server_id(), &result)
    }

    /// 发一个请求并等它的回答。
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, McpError> {
        // 先看退出记录：对一个已经死掉的服务端，「怎么死的」比「它没在跑」有用得多。
        if let Some(exit) = self.last_exit() {
            return Err(self.exited_error(exit));
        }
        if !self.is_running() {
            return Err(self.not_running_error());
        }

        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        lock_or_recover(&self.inner.pending).insert(id, sender);

        if let Err(error) = self
            .write_frame(&wire::request_frame(id, method, params))
            .await
        {
            self.forget(id);
            return Err(error);
        }

        match tokio::time::timeout(timeout, receiver).await {
            Err(_) => {
                // 这次调用已经放弃了，但连接没坏：清掉待响应项，后面还能继续用。
                self.forget(id);
                Err(McpError::Timeout {
                    server_id: self.server_id().to_string(),
                    method: method.to_string(),
                    timeout,
                })
            }
            Ok(Err(_)) => {
                self.forget(id);
                Err(self.not_running_error())
            }
            Ok(Ok(Err(failure))) => Err(self.failure_error(failure)),
            Ok(Ok(Ok(result))) => Ok(result),
        }
    }

    /// 关闭服务进程：先请它自己走，再强杀，每一步都有预算。
    ///
    /// 语义与 sidecar 的 `stop()` 一致：先取走「还在跑」这个事实（在途请求会立刻失败，而不是
    /// 等到超时），然后才关进程。记录留在句柄里，所以第二次调用会如实说 `was_running: false`，
    /// 界面也还能看到它最后一次是怎么结束的。
    pub async fn shutdown(&self) -> ShutdownReport {
        let was_running = !self.inner.exited.swap(true, Ordering::Relaxed);
        // 关掉 stdin 就是 MCP stdio 的「体面退出」：服务端看到 EOF 自己走。
        let stdin = self.inner.stdin.lock().await.take();
        drop(stdin);
        self.inner.fail_pending(PendingFailure::Closed);

        if !was_running {
            return ShutdownReport {
                was_running: false,
                killed: false,
                unreaped: false,
            };
        }

        if self.await_exit_record(SHUTDOWN_GRACE).await.is_some() {
            return ShutdownReport {
                was_running: true,
                killed: false,
                unreaped: false,
            };
        }

        // EOF 没被理会：升级。`start_kill` 在 Unix 上是 SIGKILL、在 Windows 上是
        // TerminateProcess —— 本机能发出的最强动作，再问下去已经问不出新东西了。
        {
            let mut child = lock_or_recover(&self.inner.child);
            let _ = child.start_kill();
        }
        let reaped = self.await_exit_record(SHUTDOWN_KILL_GRACE).await.is_some();
        ShutdownReport {
            was_running: true,
            killed: true,
            unreaped: !reaped,
        }
    }

    fn not_running_error(&self) -> McpError {
        McpError::NotRunning {
            server_id: self.server_id().to_string(),
        }
    }

    fn exited_error(&self, exit: McpExitRecord) -> McpError {
        // 先取值再移动字段：`signal` 是 String，先 move 走就没法再算 reason 了。
        let reason = exit.reason();
        McpError::Exited {
            server_id: self.server_id().to_string(),
            code: exit.code,
            signal: exit.signal,
            reason,
        }
    }

    fn failure_error(&self, failure: PendingFailure) -> McpError {
        match failure {
            PendingFailure::Remote(error) => McpError::Remote {
                server_id: self.server_id().to_string(),
                code: error.code,
                message: error.message,
            },
            PendingFailure::Exited(exit) => self.exited_error(exit),
            PendingFailure::Closed => self.not_running_error(),
        }
    }

    async fn write_frame(&self, frame: &Value) -> Result<(), McpError> {
        let mut payload = serde_json::to_vec(frame).map_err(|error| McpError::Protocol {
            server_id: self.server_id().to_string(),
            reason: format!("could not encode a frame: {error}"),
        })?;
        // 一行一帧：换行就是 MCP stdio 的分帧方式，与 sidecar 的 NDJSON 长得像，
        // 但两边的帧内容没有任何关系。
        payload.push(b'\n');

        let written = {
            let mut guard = self.inner.stdin.lock().await;
            match guard.as_mut() {
                None => return Err(self.not_running_error()),
                Some(stdin) => match stdin.write_all(&payload).await {
                    Ok(()) => stdin.flush().await,
                    Err(error) => Err(error),
                },
            }
        };
        match written {
            Ok(()) => Ok(()),
            Err(error) => Err(self.write_failure(error).await),
        }
    }

    /// 写失败往往只是因为子进程已经不在了：那时候真正的错误是「它死了」，而不是
    /// 「管道断了」。等一下退出记录，能报退出原因就报退出原因。
    async fn write_failure(&self, error: std::io::Error) -> McpError {
        if let Some(exit) = self.await_exit_record(WRITE_FAILURE_EXIT_GRACE).await {
            return self.exited_error(exit);
        }
        McpError::Write {
            server_id: self.server_id().to_string(),
            reason: error.to_string(),
        }
    }

    /// 等退出记录出现（有界）。等的是记录，而不是自己去 `Child::wait`：回收者只有一个
    /// （退出观察者），两个任务同时 `try_wait` 会让其中一个永远拿不到状态。
    async fn await_exit_record(&self, grace: Duration) -> Option<McpExitRecord> {
        let deadline = tokio::time::Instant::now() + grace;
        loop {
            if let Some(record) = self.last_exit() {
                return Some(record);
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(EXIT_POLL_INTERVAL).await;
        }
    }

    fn forget(&self, id: i64) {
        lock_or_recover(&self.inner.pending).remove(&id);
    }
}

impl Inner {
    /// 一帧到达：是回答就交给等待者，是别的就记一条诊断。
    fn dispatch_line(&self, line: &str) {
        let frame: Value = match serde_json::from_str(line) {
            Ok(frame) => frame,
            Err(error) => {
                // stdout 是协议通道，但第三方服务端往上面打横幅是现实存在的。丢掉这一行并
                // 记下来，而不是让整条连接失效：行协议不会因为一行噪声而错位。
                self.remember_diagnostic(format!(
                    "dropped a stdout line that is not JSON ({error}): {}",
                    truncated(line)
                ));
                return;
            }
        };

        match wire::classify(&frame) {
            wire::Incoming::Response { id } => match wire::parse_response(&frame) {
                Ok(result) => self.resolve(id, Ok(result)),
                Err(error) => self.resolve(id, Err(PendingFailure::Remote(error))),
            },
            wire::Incoming::ServerRequest { id, method } => {
                // 我们声明了零客户端能力，合规的服务端不该发请求过来。替它编一个答案是
                // docs/boundaries/mcp.md 的「不要暗示已经可用」禁止的「假装能用」，所以只记录。
                self.remember_diagnostic(format!(
                    "ignored a request from the server (id {id}, method \"{}\")",
                    truncated(&method)
                ));
            }
            wire::Incoming::Notification { method } => {
                // 包括 `notifications/tools/list_changed`：工具表变化意味着重新注册，而注册
                // 属于适配器（docs/boundaries/mcp.md 的「外部工具必须先变成 Yukinal 的工具声明」）。
                // 这里只记录，缓存不刷新。
                self.remember_diagnostic(format!(
                    "ignored a notification from the server (\"{}\")",
                    truncated(&method)
                ));
            }
            wire::Incoming::Noise(reason) => self.remember_diagnostic(reason),
        }
    }

    /// 把退出变成三件事：一条记录、一个布尔事实、以及所有在途请求的失败原因。
    ///
    /// 先落记录再唤醒等待者：被唤醒的调用方要能立刻读到「怎么死的」。
    fn record_exit(&self, status: ExitStatus) {
        let record = McpExitRecord {
            code: status.code(),
            signal: exit_signal(&status),
            at: yukinal_time::iso8601_now(),
        };
        *lock_or_recover(&self.exit) = Some(record.clone());
        self.exited.store(true, Ordering::Relaxed);
        self.remember_diagnostic(format!("the server exited ({})", record.reason()));
        self.fail_pending(PendingFailure::Exited(record));
    }

    fn resolve(&self, id: i64, outcome: Result<Value, PendingFailure>) {
        let sender = lock_or_recover(&self.pending).remove(&id);
        if let Some(sender) = sender {
            // 接收端已经超时或放弃了是正常的；这里没有别的可做。
            let _ = sender.send(outcome);
        }
    }

    fn fail_pending(&self, failure: PendingFailure) {
        let mut pending = lock_or_recover(&self.pending);
        for (_, sender) in pending.drain() {
            let _ = sender.send(Err(failure.clone()));
        }
    }

    fn remember_stderr(&self, line: &str) {
        // 这里是**不可信文本**进入我们日志的唯一入口之一：一个陌生服务端可以往 stderr 上
        // 打任何东西，包括它自己拿到的密钥。先截断再脱敏，与 sidecar 转发日志时同序 ——
        // 反过来先脱敏会在一段被截断的文本上做匹配，边界处的密钥形式就漏过去了。
        let line = redacted(&truncated(line));
        let mut tail = lock_or_recover(&self.stderr_tail);
        push_bounded(&mut tail, line, STDERR_TAIL_LINES);
    }

    fn remember_diagnostic(&self, line: String) {
        // 诊断尾部同样会随状态发给界面，所以同样要脱敏：它里面装着 stdout 上的噪声
        // （服务端自己决定的内容）与远端错误文本。
        let mut tail = lock_or_recover(&self.diagnostics);
        push_bounded(
            &mut tail,
            redacted(&truncated(&line)),
            DIAGNOSTIC_TAIL_LINES,
        );
    }
}

#[cfg(unix)]
fn exit_signal(status: &ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;
    // 只记号码：`reason()` 负责把它说成一行，两边都拼一次会得到「signal signal 9」。
    status.signal().map(|signal| signal.to_string())
}

#[cfg(not(unix))]
fn exit_signal(_status: &ExitStatus) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tail_is_bounded_and_drops_the_oldest_line_first() {
        let mut tail = VecDeque::new();
        for index in 0..(STDERR_TAIL_LINES + 10) {
            push_bounded(&mut tail, format!("line {index}"), STDERR_TAIL_LINES);
        }
        assert_eq!(tail.len(), STDERR_TAIL_LINES);
        assert_eq!(
            tail.front().map(String::as_str),
            Some("line 10"),
            "a chatty server must not be able to grow the tail forever"
        );
        assert_eq!(
            tail.back().map(String::as_str),
            Some(&*format!("line {}", STDERR_TAIL_LINES + 9))
        );
    }
}
