# ADR 0001 Agent Runtime 作为独立 Node.js sidecar

Status: Accepted
Date: 2026-09-09

## Context

Agent loop 需要 TypeScript 生态、流式 HTTP 能力、Provider SDK 和可扩展的工具注册表。Tauri WebView 只承担界面职责，既不提供 Node.js 运行时，也不应决定 Agent 是否能够安全退出。若由 React 直接管理 Agent，窗口状态、进程状态和凭据边界会相互缠绕。

## Decision

将 `apps/agent` 作为独立的 Node.js 进程运行，由 Rust 宿主拥有其启动、通信和退出生命周期。

- React 只调用白名单 Tauri 命令。`agent_spawn`、`agent_status`、`agent_kill` 和 `agent_logs` 均由 Rust 实现。
- Rust 通过 stdio 与 sidecar 通信，帧格式由 [ADR 0006](0006-sidecar-transport-ndjson-jsonrpc.md) 定义。
- Agent 不直接连接 SSH、不读取系统凭据库，也不执行未经 ToolRegistry 和 Permission Engine 处理的宿主操作。
- Rust 在运行时解析 Provider key，并通过一次运行的协议参数传给 sidecar；key 不写入持久化配置或日志。
- 开发与发布环境都使用已编译的 JavaScript 入口。入口由 Rust 解析，而非由 React 启动。

## Consequences

- Agent 可以使用 Node.js 的 HTTP、流式处理和测试生态，单独崩溃也不会直接带走桌面窗口。
- 停止信号可从 sidecar 传播到 Provider 与工具的在途请求。
- 项目需要维护额外的 IPC 契约、进程监督逻辑和跨语言集成测试。
- 当前不自动重启崩溃的 sidecar。进程退出后 Rust 清理运行状态，UI 提供重新启动入口；发布版仍需要随应用分发受信任的 Node runtime 与 bundle。
