# ADR 0006：sidecar 使用 stdio 上的 NDJSON JSON-RPC

Status: Accepted
Date: 2026-09-09

## Context

独立 Agent 需要请求、响应、流式通知、取消和错误编码。WebSocket 或本地 HTTP 会引入端口管理和额外的本地网络面；Tauri IPC 也不应把 Node.js Agent 直接暴露给 WebView。

## Decision

Rust 启动 sidecar 并持有进程句柄；双方通过 stdin/stdout 传输 JSON-RPC 2.0，每行一个 JSON frame。

- `initialize` 必须是第一条请求，用于协商协议版本和注入数据目录。
- `agent.run.start`、`agent.run.stop`、`agent.approval.respond` 等请求返回 JSON-RPC response。
- Agent 的思考、工具、审批、完成和失败事件通过 `agent.stream` notification 上行；Rust 将其映射为同名 Tauri 事件。
- stdout 只写协议帧，日志全部写 stderr。
- transport 对单帧大小和畸形输入设上限；坏帧记录后丢弃，不因为一行无效 JSON 直接退出进程。
- 当前协议版本为 `1.0`；版本不匹配或 schema 不通过时返回 `INVALID_PARAMS`。

## Consequences

- 没有端口冲突或 localhost 防火墙配置，父进程退出也能直接发现管道断开。
- Rust、Node.js 和 UI 可以共享 `packages/shared` 中的类型和 schema。
- 浏览器开发模式无法直接复用 stdio sidecar，因此预览模式只展示 UI；真实协议通过 sidecar smoke 和跨语言集成测试验证。
- 大型日志和模型输出必须在各层截断或分页，不能把无界内容塞入单帧。
