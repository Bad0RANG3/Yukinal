# ADR 0008：Rust 负责 sidecar 启动、监督和生命周期

Status: Accepted
Date: 2026-09-09

## Context

React 不应拥有进程句柄，也不能在 WebView 中依赖 Node.js。sidecar 还需要在 handshake 失败、父进程退出或用户点击停止时被可靠回收，并且启动路径必须能区分开发 bundle、测试入口和未来的安装包资源。

## Decision

`yukinal-core` 中的 `sidecar` 负责进程和 JSON-RPC 传输，`supervisor` 负责状态、日志、单实例和退出记录。Tauri 命令只做参数编组：`agent_spawn`、`agent_status`、`agent_kill` 和 `agent_logs`。

入口解析按以下顺序执行，第一个有效配置胜出：

1. `YUKINAL_AGENT_COMMAND` 与 `YUKINAL_AGENT_ARGS`，用于测试和明确的定制启动。
2. `YUKINAL_AGENT_ENTRY`，可配合 `YUKINAL_NODE` 指向指定 Node 和 bundle。
3. 从当前目录向上查找 `apps/agent/dist/index.js`，服务开发和本地构建。

启动流程固定为 `spawn → subscribe → initialize/handshake → publish runtime`。handshake 失败会关闭 child，不发布半初始化状态。Supervisor 的 start/stop 共用同一把锁，避免 stop 与 spawn/handshake 竞争时留下孤儿进程。

生命周期规则：

- 同一 Supervisor 只保留一个运行中的 sidecar；重复启动返回 `alreadyRunning`。
- sidecar stderr 保留最近 200 行，并在状态中保留上一次退出记录，便于 UI 展示失败原因。
- sidecar 监听父 stdin；父进程消失时主动结束。Rust 正常关闭时调用 sidecar shutdown，进程句柄仍提供 kill-on-drop 兜底。
- sidecar 崩溃不会伪造运行完成事件；Supervisor 清除运行时，桌面 UI 通过状态轮询显示退出并提供重新启动入口。

## Consequences

- 开发、测试和桌面壳共用一条实际启动链路，跨语言握手可以在 CI 中验证。
- React 不需要知道 Node 的路径、PID 或重启细节，只消费状态和事件。
- 发布安装包必须提供受信任的 Node 可执行文件和 bundle 绝对路径；生产环境不应依赖可被替换的 PATH 上的 `node`。
- 当前仓库尚未启用 Tauri installer bundle，因此发布资源分发和 Node runtime 打包仍需后续决策。
