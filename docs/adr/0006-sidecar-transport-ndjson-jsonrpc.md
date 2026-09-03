# 0006 — sidecar 传输：stdio 上的 NDJSON JSON-RPC

Status: accepted (2026-09)

## Context
ADR 0001 定了独立 Node 进程，需要选定 UI/Rust ↔ Agent 的通信方式。候选：WebSocket、本地 HTTP、
Tauri IPC 转发、stdio。

## Decision
**JSON-RPC 2.0 over newline-delimited JSON on stdin/stdout**，由 Rust 拥有进程句柄。

- 请求/响应 = 动作（`agent.run.start`、`tools.list`、`initialize`）。
- 通知 = 流（`agent.stream` → 映射到共享契约里的 `agent.*` 事件）。
- `stdout` 只承载协议帧；**日志一律 stderr**（否则一个 `console.log` 会污染协议）。
- 帧大小上限（8 MiB）与畸形行的容错在 `NdjsonDecoder`：坏帧丢弃并记录，进程不退出。
- 版本协商：`initialize.protocolVersion`，不匹配直接 `INVALID_PARAMS`。

## Consequences
- (+) 无端口冲突、无 localhost 防火墙弹窗、无 token 化的本地 HTTP 面。
- (+) 崩溃即断管，Rust 能立刻知道并重启（ Stop 语义更可靠）。
- (−) 不能像 WebSocket 那样在浏览器 dev 模式直连 sidecar（开发时用 `agent:dev` + 测试客户端替代）。
- (−) 大输出（长日志）需要分页/截断约定，否则一帧太重；截断必须可见。
