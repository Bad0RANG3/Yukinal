# ADR 0008 Rust 负责 sidecar 的启动和生命周期

Status: Accepted
Date: 2026-09-09

## Context

React 不应持有进程句柄，也不能在 WebView 中依赖 Node.js。sidecar 需要在 handshake 失败、父进程退出或用户停止运行时被可靠回收；启动路径还必须覆盖开发 bundle、测试入口和未来安装包资源。

## Decision

`yukinal-core` 的 `sidecar` 负责进程与 JSON-RPC transport，`supervisor` 负责状态、日志、单实例和退出记录。Tauri 命令只做参数编组：`agent_spawn`、`agent_status`、`agent_kill` 和 `agent_logs`。

入口按下列顺序解析，第一个有效配置获胜：

1. `YUKINAL_AGENT_COMMAND` 与 `YUKINAL_AGENT_ARGS`，用于测试或明确的定制启动。
2. `YUKINAL_AGENT_ENTRY`，可配合 `YUKINAL_NODE` 指向指定 Node 与 bundle。
3. 从当前目录向上查找 `apps/agent/dist/index.js`，用于开发与本地构建。

启动顺序固定为 `spawn → subscribe → initialize/handshake → publish runtime`。handshake 失败时必须关闭 child，不能发布半初始化状态。Supervisor 的 start 和 stop 共用同一把锁，以避免竞争留下孤儿进程。

生命周期规则如下：

- 每个 Supervisor 只保留一个运行中的 sidecar；重复启动返回 `alreadyRunning`。
- sidecar stderr 保存最近 200 行，状态保留上一次退出记录，供 UI 显示故障原因。
- sidecar 监听父 stdin，父进程消失时主动结束。Rust 正常关闭时请求 sidecar shutdown，进程句柄仍以 kill-on-drop 作为兜底。
- sidecar 崩溃不能伪造“运行完成”事件；Supervisor 清理运行时，桌面 UI 通过状态轮询显示退出并提供重启入口。

## Consequences

- 开发、测试和桌面端共用实际的启动链路，跨语言 handshake 可在 CI 中验证。
- React 不需要知道 Node 路径、PID 或重启细节，只消费状态和事件。
- 发布安装包必须提供受信任的 Node 可执行文件与 bundle 绝对路径；生产环境不应依赖可被替换的 PATH 中的 `node`。
- 当前仓库尚未启用 Tauri installer bundle，发布资源分发与 Node runtime 打包仍需后续决定。
