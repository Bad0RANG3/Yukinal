# ADR 0001：Agent Runtime 作为独立 Node.js sidecar

Status: Accepted
Date: 2026-09-09

## Context

Agent loop 需要 TypeScript 生态、Provider SDK、流式响应和可扩展工具注册表。Tauri WebView 只负责界面，不提供 Node.js 运行时；同时，窗口生命周期不应决定 Agent 是否能安全停止。

## Decision

`apps/agent` 作为独立 Node.js 进程运行，并由 Rust 宿主拥有它的生命周期。

- React 只调用白名单 Tauri 命令；`agent_spawn`、`agent_status`、`agent_kill` 和 `agent_logs` 由 Rust 实现。
- Rust 通过 stdio 与 sidecar 通信，具体帧格式由 [ADR 0006](0006-sidecar-transport-ndjson-jsonrpc.md) 定义。
- Agent 进程不直接连接 SSH、不读取系统凭据库，也不执行未经 ToolRegistry 和 Permission Engine 处理的宿主操作。
- Provider key 在 Rust 的运行时边界解析，并通过一次运行的协议参数注入 sidecar；不写入持久化配置或日志。
- 开发和发布都运行编译后的 JavaScript 入口；Rust 负责解析入口，不由 React 启动 Node。

## Consequences

- Agent 可以使用 Node.js 的 HTTP、流式和测试工具，且崩溃不会直接带走窗口。
- Stop 可以沿着 sidecar、Provider 和 Tool 的取消信号传播到在途请求。
- 需要维护额外的 IPC 契约、进程监督和跨语言集成测试。
- 当前没有自动崩溃重启策略；进程退出后 Rust 清除运行状态，桌面 UI 提供重新启动入口。发布版仍需要随安装包提供受信任的 Node runtime 和 bundle。
