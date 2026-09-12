# ADR 0001 Agent Runtime 作为独立 Node.js sidecar

Status: Accepted（「发布成安装包时必须随应用分发受信任的 Node 运行时」一处已由 [ADR 0013](0013-installer-distribution.md) 取代：安装包随包分发 agent bundle，但使用用户系统上的 Node。其余部分仍然有效）
Date: 2026-09-09

## Context

Agent 的核心工作是一个持续与外部服务对话的循环：把上下文发给模型，流式接收文本与工具调用增量，决定是否执行工具，再把结果回灌模型进入下一轮。这个循环需要三样东西：成熟的流式 HTTP 客户端、可以快速迭代的工具与 schema 生态，以及一个能在运行中途被取消的执行环境。

Tauri 的 WebView 三样都给不了：它没有 Node.js 运行时，没有可用的流式 HTTP 栈，也不是一个适合承载长任务的进程——窗口一关，任务的定义就消失了。如果把 Agent 放进 React 侧，窗口生命周期、进程生命周期和凭据边界会互相缠绕：界面刷新不应该杀掉一次正在等待用户批准的运行，而界面也不应该成为一个能读凭据、能连 SSH 的进程。

同时，Agent 也不能拥有远程访问能力。一旦 sidecar 自己能连 SSH，权限决策就退化成了「模型说要连，那就连」，而这正是本项目要避免的事情。

## Decision

把 Agent 运行时做成独立的 Node.js 进程（`apps/agent`），由 Rust 宿主拥有它的启动、通信和退出，而不是由 React 或用户操作拥有。

- **React 只调用白名单里的 Tauri 命令。** 与 Agent 相关的有 `agent_spawn`、`agent_status`、`agent_kill`、`agent_logs`、`agent_run_start`、`agent_run_stop`、`agent_approval_respond`，全部由 Rust 实现。React 不知道 Node 路径、PID 或 bundle 位置。
- **两侧通过 stdio 通信。** 帧格式、方法名与版本协商由 [ADR 0006](0006-sidecar-transport-ndjson-jsonrpc.md) 定义；进程本身的启动与回收由 [ADR 0008](0008-sidecar-launch-and-lifecycle.md) 定义。
- **Agent 不直接触碰远程资源。** sidecar 既不连接 SSH，也不访问 SQLite 或操作系统凭据库。它需要远端数据时，通过同一个协议流发出 `host.tool.execute` 或 `host.context.fetch` 请求，由 Rust 宿主执行并把结果返回（`apps/agent/src/transport/host-client.ts`）。这些请求在发出前已经过 ToolRegistry 与 Permission Engine；宿主收到后还会再校验一次目标。宿主是唯一持有原生能力的进程。
- **凭据在宿主的侧解析，只以一次性参数跨进程。** Rust 从 SQLite 读取 Provider 配置、从操作系统凭据库取出密钥，把它们放进 `agent.run.start` 的 `providerConfig` 里。这个负载不落盘、不进日志、不进审计。sidecar 自己的进程配置只有 `YUKINAL_DATA_DIR`、`YUKINAL_LOG_LEVEL` 和 `YUKINAL_MAX_RUN_MS`（`apps/agent/src/config.ts`），其中没有任何机密。
- **入口是编译产物，由 Rust 解析。** Agent 以 `tsc` 编译后的 JavaScript（`apps/agent/dist/index.js`）运行，启动方式与开发、测试环境一致；解析逻辑在 Rust 侧（`SidecarConfig::from_env_with_cwd`），React 无权指定要执行什么。

```text
React  ──Tauri IPC──►  Rust 宿主  ──stdio JSON-RPC──►  Node.js sidecar  ──HTTPS──►  模型端点
                          │                                  │
                          │◄──── host.tool.execute ───────────┘
                          ▼
                    SSH / SFTP / PTY / SQLite / 凭据库
```

## Consequences

**收益**

- Agent 可以使用 Node.js 的流式 HTTP、取消语义和测试生态；`node --test` 风格的单测可以直接覆盖 loop、权限与 Provider。
- 崩溃被隔离：sidecar 挂掉不会带走窗口，窗口关闭也不会让一个正在等待批准的运行无声消失。
- 停止信号可以真正传播。`agent.run.stop` 会让 loop 中止在途的 HTTP 流、取消的工具执行和等待中的审批，并向宿主发出 `host.tool.cancel` 让已经开始的宿主操作尽快收敛。
- 权限与凭据边界变得可以陈述：唯一能执行远程操作的进程是 Rust 宿主，唯一能读密钥的进程也是 Rust 宿主。

**成本**

- 需要维护一整套额外契约：JSON-RPC 方法、双向请求、schema 校验、跨语言集成测试和进程监督逻辑。这些都不是业务功能，但不维护就会静默出错。
- 发布成安装包时必须随应用分发受信任的 Node 运行时与 bundle，并给出绝对路径；当前实现依赖 PATH 上的 `node`，这在开发机上没问题，在成品里不成立。
- 当前**不自动重启**崩溃的 sidecar。进程退出后 Rust 记录退出码与信号、保留最近 200 行 stderr，界面提供重新启动的入口；用户会看到一次中断，而不是一个自动恢复的假象。
- 帧是有界的，日志与模型输出必须在各层截断或分页，不能把无界内容塞进一个 frame。

## Alternatives considered

- **把 Agent 放进 Tauri WebView。** 没有 Node.js 运行时，也没有等价的流式 HTTP 与工具生态；同时会让「窗口是否打开」变成「Agent 能否工作」的前提。否决。
- **用 Rust 实现整个 Agent loop。** 能省掉跨语言协议，但会失去 TypeScript 侧的工具与 Provider 迭代速度，并把模型协议差异带进原生核心。否决。
- **由 React 直接派生并管理子进程。** 权限最小化地看它多给界面一项系统能力（派生子进程），而进程状态与界面状态耦合会让「刷新界面」变成危险操作。否决。
- **让 sidecar 自己持有 SSH 与凭据。** 会让授权决策失去唯一入口，也使凭据出现在第二个进程里。否决。
