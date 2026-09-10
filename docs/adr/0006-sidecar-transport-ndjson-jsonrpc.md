# ADR 0006 sidecar 通过 stdio 上的 NDJSON JSON-RPC 通信

Status: Accepted
Date: 2026-09-09

## Context

独立的 Agent 需要支持请求、响应、流式通知、取消和错误编码。WebSocket 或本地 HTTP 会带来端口管理与额外的本地网络暴露；Tauri IPC 也不应把 Node.js Agent 直接暴露给 WebView。

## Decision

Rust 启动 sidecar 并持有进程句柄。双方经由 stdin/stdout 传输 JSON-RPC 2.0，每一行是一个 JSON frame。

- `initialize` 必须是第一条请求，用于协商协议版本并注入数据目录。
- `agent.run.start`、`agent.run.stop` 与 `agent.approval.respond` 等请求返回 JSON-RPC response。
- Agent 的思考、工具、审批、完成和失败事件以 `agent.stream` notification 上行；Rust 将其转发为同名 Tauri 事件。
- stdout 只能写协议帧，所有日志写入 stderr。
- transport 对单帧大小和畸形输入设上限。坏帧被记录并丢弃，而不会因一行无效 JSON 直接退出。
- 当前协议版本为 `1.0`；版本不匹配或 schema 校验失败时返回 `INVALID_PARAMS`。

## Consequences

- 系统不需要端口或 localhost 防火墙配置，父进程也能通过管道关闭直接发现 sidecar 退出。
- Rust、Node.js 与 UI 可以共享 `packages/shared` 中的类型和 schema。
- 浏览器开发模式不能直接复用 stdio sidecar，因此只展示 UI；实际协议通过 sidecar smoke 和跨语言集成测试验证。
- 大型日志和模型输出必须在各层截断或分页，不能以无界内容填充单个 frame。
