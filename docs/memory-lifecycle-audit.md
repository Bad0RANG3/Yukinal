# 内存与生命周期审计

审计日期：2026-09-20。范围覆盖 Windows 工作区、WSL Arch Linux 中真实运行的 OpenSSH/Node
sidecar、回环 SSH/Collector 路径，以及 Xvfb 下打包的 Tauri 窗口。没有把 `127.0.0.1`
说成远程主机，也没有把一次窗口冒烟说成长时间资源曲线。

## 结论

Rust 在安全代码里已经排除 use-after-free、double free 和数据竞争这类内存安全问题。本项目并没有一个需要靠“重构掉 Rust”来解决的天然内存缺陷。真正需要治理的是更高层的所有权与生命周期：`Arc` 可能因为循环或后台任务而一直不释放，`tokio::spawn` 的任务可能跨过所有者销毁继续运行，子进程、PTY、SSE 连接和文件句柄也不会因为语言安全保证而自动收口。

本轮按这个边界做了静态审计、源码修复和可重复的本地回归。修复的原则是：

- 长等待的后台任务不跨 `await` 持有所有者 `Arc`，改用 `Weak` 或独立状态快照。
- 每个需要停止的后台任务都有明确的取消信号；`Drop` 是兜底，不是唯一关闭路径。
- 子进程、SSE 流、PTY 转发器和 keepalive 任务在所有者退出、显式关闭或最后句柄释放时，都要有确定的收口动作。
- Rust 的安全保证负责防止内存错误，不负责替业务决定何时释放、取消或回收逻辑资源。

## 根因分类

### 强引用保活

把 `Arc<Owner>` 克隆进后台任务后，任务本身会成为所有者的强引用根。即使界面、会话或管理器已经丢弃最后一个可见句柄，任务仍可能保活整张状态表。这个问题不是悬垂指针，也不是未定义行为，而是对象的生命周期超过了业务生命周期。

`Weak` 只在任务真正需要进入同步临界区时短暂升级；升级失败就退出。这样既避免长期强引用，又保留“所有者仍存在时才能操作其状态”的检查。

### 缺少取消协议

只让所有者从外部丢弃 `Arc` 不足以停止一个正在等待网络、通道或定时器的任务。任务需要可组合的 `CancellationToken`、关闭通知或显式的所有者关闭入口。取消信号必须覆盖启动阶段和长时间等待阶段，否则“取消”只会在下一次事件到达后才生效。

### 异步 `Drop` 与外部句柄

Rust 的 `Drop` 不能直接执行异步关闭协议。正常路径仍应调用显式的 `shutdown`/`close`，并把 `Drop` 当作同步兜底：发出取消信号、终止可 abort 的任务、或关闭已经持有的同步句柄。子进程退出需要回收并观察退出记录；SSE/PTY 需要断开接收端；keepalive 需要停止重连循环。

### 关闭与资源语义分离

释放 Rust 内存不等于远端状态已经回滚，也不等于操作系统句柄已经立即回收。文档和实现都必须区分“本地任务已停止”“子进程已退出”“远端调用已取消”和“远端副作用已撤销”。

## 已修复问题

| 模块 | 原风险 | 当前修复 | 回归证据 |
| --- | --- | --- | --- |
| MCP Streamable HTTP（`crates/core/src/mcp/http.rs`） | 可选 GET SSE 任务跨握手和长期等待持有 `HttpInner`，最后一个 HTTP 句柄释放后可能继续保活会话 | `HttpInner::drop` 取消关闭令牌并 abort GET 任务；GET 启动只持配置快照和 `Weak<HttpInner>`；认证发送抽成快照函数，避免为等待而克隆整个所有者 | `crates/core/tests/mcp_http.rs::dropping_the_last_http_handle_stops_the_optional_get_stream` |
| MCP supervisor（`crates/core/src/mcp/supervisor.rs`） | 退出观察和自动重启在退避等待期间持有 supervisor 强引用；启动锁也属于容易误保活的所有者 | 退出观察与重启循环只持 `Weak<SupervisorInner>`；`start_lock` 使用独立 `Arc`，启动/握手等待不依赖整个 supervisor 存活 | `crates/core/src/mcp/supervisor.rs::restart_backoff_does_not_keep_the_supervisor_alive` |
| sidecar supervisor（`crates/core/src/supervisor/mod.rs`、`crates/core/src/sidecar/mod.rs`） | stdout/stderr pump、退出轮询和重启循环可能延长 supervisor 生命周期；显式停止后退出事件与状态记录之间存在异步调度窗口 | pump 与 restart loop 改用 `Weak`；显式关闭 supervisor 会回收 sidecar，并在 `stop()` 返回前用真实退出状态写入 `lastExit`；非预期崩溃仍由 watcher 记录 | `crates/core/tests/sidecar_agent.rs::dropping_the_supervisor_kills_its_sidecar`、`the_supervisor_tracks_its_own_child_including_the_exit_record` |
| terminal manager（`crates/terminal/src/manager.rs`） | 每个终端会话的 forwarder 持有 session map 的 `Arc`，管理器释放后任务仍可能保活会话 | forwarder 持 `Weak` 和 `CancellationToken`；收到输出事件时短暂升级；`TerminalManager::drop` 取消转发器 | `crates/terminal/src/manager.rs::dropping_manager_releases_sessions_and_stops_the_forwarder` |
| SSH session（`crates/ssh/src/conn.rs`） | `SessionHandle` 的 keepalive 任务在普通析构路径没有显式停止信号 | `SessionHandle::drop` 发送关闭信号并 abort keepalive 任务；正常连接关闭仍走显式 `close` 路径 | WSL 中真实 OpenSSH 回环测试 4 项通过、4 项因未配置私钥/证书/agent 凭证明确跳过；长时间远程主机 soak 仍留作外部验证 |
| Collector context（`crates/collector/src/lib.rs`） | `detect()` 写进克隆的 capability 容器，检测结果随临时克隆丢弃，后续采集看不到能力 | capability 改为 `Arc<Mutex<_>>`，`clone_context()` 共享同一所有权；采集器检测与实际采集使用同一能力快照 | `cloned_context_shares_capabilities`，以及 WSL 真实 OpenSSH 上的 `full_mvp_chain_on_a_real_linux_host` |
| 既有 MCP 句柄分发（`crates/core/src/mcp/handle.rs`） | 需要确认退出订阅和分发不会把所有者长期钉住 | 审计确认使用 `Weak` 退出订阅，强引用只在同步分发期间短暂存在；本轮未改写该文件 | 源码静态审计；没有把它包装成新功能 |

## 所有权与关闭约定

后续修改 Rust 后台任务时，按以下顺序判断：

1. 任务是否需要所有者本身，还是只需要 endpoint、配置、token source、事件发送端等可克隆快照。若只需快照，不要克隆 `Arc<Owner>`。
2. 任务是否会跨 `await`、网络请求、退避或无限循环。若是，优先持 `Weak`，只在同步临界区短暂升级；升级失败应作为所有者已退出处理。
3. 任务是否有明确的取消条件。使用 `CancellationToken`、关闭通知或独立停止标志；不要假设所有者析构一定先于任务下一次调度。
4. 所有者是否有显式 `shutdown`/`close`。子进程、PTY、SSE、keepalive 和 MCP 会话都应有显式收口路径；`Drop` 只负责兜底，不能替代异步关闭协议。
5. 关闭后是否仍有外部事实。本地句柄释放不表示远端副作用已经取消或回滚，错误和状态报告必须保持这个区别。

这条约定不要求把所有 `Arc` 改成 `Weak`。共享只读配置、短生命周期的任务依赖和显式拥有的子对象仍可以正常使用 `Arc`；关键是任务不能在所有者需要被释放时成为隐形的强引用根。

## 验证证据

本轮本地验证包括：

- `cargo fmt --all`。
- `cargo test -p yukinal-core --test mcp_http`，覆盖 HTTP 生命周期释放、SSE、认证、代理与取消路径。
- 使用 `YUKINAL_TEST_NODE=/usr/bin/node` 与打包后的 `target/debug/agent/index.js` 运行
  `cargo test -p yukinal-core --test sidecar_agent`，8 项真实 Node sidecar 测试通过，覆盖显式停止、
  退出记录、崩溃重启和 supervisor 释放时回收 sidecar。
- `cargo test -p yukinal-terminal`，覆盖终端管理器与转发器释放。
- WSL `127.0.0.1:22222` 上的真实 OpenSSH 回环：`yukinal-ssh --test live` 的密码认证、keepalive、
  host key 变化阻断、probe/trust 流程 4 项通过；私钥、加密私钥、证书和 agent 认证 4 项因没有对应
  凭证明确跳过。Collector 的 `full_mvp_chain_on_a_real_linux_host` 与
  `command_timeout_is_surfaced_not_hung` 也通过。
- Xvfb 下启动 Windows 构建产物：sidecar 真实握手为 `protocol=1.0 tools=30`；关闭 Tauri 窗口后，
  应用进程与 sidecar 子进程均退出，没有留下脱离父进程的 `index.js`。
- `cargo clippy --workspace --all-targets -- -D warnings`。
- `pnpm.cmd check` 与文档链接检查。

其中 HTTP 生命周期测试使用多线程 Tokio 运行时，因为 fixture 的同步客户端和异步 transport 需要在同一测试内并行推进。这里没有“曾经发现协议层连接泄漏”的结论；最初的单线程测试失败来自测试自身阻塞了运行时，修正为多线程后验证的是真正的最后句柄释放行为。

静态扫描没有在当前 Rust/TypeScript 源码中发现新的 `Arc::new_cyclic`、`mem::forget` 或 `Box::leak` 用法。这个结果只说明本轮审计范围内没有发现这些显式逃逸模式，不能替代长期运行的行为验证。

## 验收边界与残余风险

本次审计只覆盖本机进程、WSL 回环、临时 SQLite、stdio、fixture 和一次 Tauri 窗口关闭冒烟。
`127.0.0.1:22222` 证明真实 SSH 协议、Linux 命令与进程生命周期，不证明远程网络条件。以下内容仍未验收：

- 真实 SSH 主机上的断线、keepalive、keyboard-interactive、重连和长时间会话资源回收。
- 真实 Provider 在取消、超时、SSE 断流、HTTP/2 连接复用和重试时的远端资源行为。
- 第三方 MCP 服务器收到 `notifications/cancelled` 后是否真正停止工作，以及非标准服务器的 GET/DELETE 行为。
- Windows 原生进程终止、系统休眠/唤醒、安装包启动路径和反复打开/关闭窗口时的句柄回收。
- 长时间反复打开/关闭 MCP、PTY、SSH 和 sidecar 后的内存曲线、线程数、文件描述符和子进程数量。

因此，本地测试通过只能证明这些代码路径在本机模型中不再由后台任务保活，并能在当前可观察边界内停止；不能把它写成真实服务器、远程部署或生产环境下“没有泄漏”的证明。
