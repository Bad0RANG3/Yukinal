# ADR 0006 sidecar 通过 stdio 上的 NDJSON JSON-RPC 通信

Status: Accepted
Date: 2026-09-09

## Context

独立的 Agent 进程（[ADR 0001](0001-agent-runtime-as-node-sidecar.md)）需要一套进程间协议，同时满足几个不常见的要求：要支持请求与响应、要从 Agent 单向推送流式通知、要允许 Agent 反向请求宿主执行操作、要能取消一个正在进行的运行，还要有明确的错误编码。

可选项都有明显代价：本地 HTTP 或 WebSocket 需要端口管理，会把能力暴露给本机上的其他进程，也要处理「端口被占用」这类与业务无关的失败；把 Agent 直接暴露给 Tauri WebView 会绕过命令白名单；自定义的二进制协议会失去可读性，排障时必须先写一个解析器。

## Decision

Rust 启动 sidecar 并持有进程句柄，双方通过 stdin/stdout 传输 **NDJSON 编码的 JSON-RPC 2.0**，每一行是一个完整帧。协议版本常量为 `1.0`，两侧各有一份定义（`packages/shared/src/protocol/jsonrpc.ts` 的 `YUKINAL_RPC_VERSION` 与 `crates/core/src/sidecar.rs` 的 `PROTOCOL_VERSION`），必须一致。

**握手**

- `initialize` 必须是第一条请求，参数为 `{protocolVersion, clientVersion, dataDir}`，其中 `protocolVersion` 在 schema 里被定义为字面量，任何其他取值都会以 `INVALID_PARAMS` 被拒。重复 `initialize` 返回 `INVALID_REQUEST`。
- 结果里包含 `agentVersion` 与 `capabilities`：`streaming`、`toolCalling`、`cancellation` 为 `true`，`mcp` 为 `false`（MCP 尚未实现，这里如实报告而不是留空）。
- 握手成功后 Rust 还会调用 `system.describe`，取回工具数量、可用策略 ID、已实现的方法表与工具名冲突列表。协议版本不匹配、或报告存在工具名冲突时，Rust 拒绝发布这个 sidecar。

**方法（Agent 侧实现）**

| 方法 | 用途 |
| --- | --- |
| `initialize` | 协商版本、注入数据目录 |
| `system.ping` | 存活探测，返回 `pong` 与 `agentPid` |
| `system.describe` | 能力与方法可用性自述 |
| `tools.list` | 返回已注册工具声明 |
| `agent.run.start` | 受理一次运行，立即返回 `runId` |
| `agent.run.stop` | 按 `runId` 停止运行 |
| `agent.approval.respond` | 回应一次挂起的审批 |
| `provider.models` | 用一次性 Provider 配置读取模型目录 |

**通知（Agent 上行）**：`agent.stream` 承载 `AgentStreamEvent`。Rust 收到后按事件的 `type` 映射成同名 Tauri 事件（`agent.thinking`、`agent.tool_call`、`agent.tool_result`、`agent.waiting_approval`、`agent.approval_expired`、`agent.completed`、`agent.failed`）。`agent.log` 在协议常量里保留了名字，但当前实现不用它传日志——日志走 stderr。

**反向请求**：Agent 也会发出请求，由 Rust 处理：`host.tool.execute`（执行宿主工具）、`host.context.fetch`（读取服务器/快照/工作区）、`host.tool.cancel`（请求取消一个已经在执行的宿主操作）。请求与响应复用同一条流，靠「有 `method` 的是请求、有 `id` 且带 `result`/`error` 的是响应」来区分。

**帧与流的规则**

- **stdout 只承载协议帧。** 所有日志写入 stderr；一条走错位置的 `console.log` 就会破坏协议。
- **单帧上限 8 MiB。** 超过上限的帧被丢弃并记录，而不是让缓冲区无限增长。
- **畸形帧不会杀死进程。** 解码失败的行被报告并跳过，流继续可用（`scripts/smoke-sidecar.mjs` 会显式发送一行坏 JSON，并断言后续 `system.ping` 仍然正常）。
- **非请求帧被忽略并记录**，不当作错误处理。
- **运行是流式的。** `agent.run.start` 的响应帧必须先于该次运行的任何 `agent.*` 通知发出，否则界面会先看到事件、后拿到 `runId`。
- **受理凭证保证重试幂等。** 带 `messageId` 的 `run.start` 会被记入一张最多 256 条的受理表；同一条消息重发且内容一致时返回同一个 `runId`（`duplicate: true`），内容不一致则报错；已有的受理记录不会被淘汰，避免一次传输重试开出第二个运行。
- **同一 `runId` 不能并发运行**，重复启动返回 `INVALID_PARAMS`。
- **宿主侧的转发有第二道闸门。** Rust 只转发白名单里的事件类型，要求 `runId` 非空且不超过 256 字符，并把单个负载限制在 1 MB（比传输层上限更紧），`agent.tool_result` 还会先落审计再转发。
- **契约违规是 `INVALID_PARAMS`，不是 `INTERNAL_ERROR`。** 参数 schema 校验在分发层完成，调用方发了双方约定不接受的结构，就应该得到这个错误码。

错误码沿用 JSON-RPC 的保留区间，并补充了业务码：`NOT_IMPLEMENTED`、`CANCELLED`、`TIMEOUT`、`DENIED_BY_POLICY`、`APPROVAL_REJECTED`、`UNKNOWN_TOOL`。其中「被拒绝」被建模成一等结果，而不是异常。

## Consequences

**收益**

- 不需要端口，也不需要本机防火墙配置；父进程关闭管道就能直接发现子进程退出，sidecar 也会在检测到 stdin 关闭时主动结束。
- 协议帧是可读的 JSON，排障时可以直接对着日志看；`scripts/smoke-sidecar.mjs` 用真实传输跑完整握手，不需要窗口。
- Rust、Node.js 与 TypeScript 类型可以从 `packages/shared` 共享同一份定义，避免两侧各自猜测字段名。
- 双向复用一条流，让「Agent 请求宿主」不需要第二套连接或第二套超时模型。

**成本**

- 浏览器开发模式无法复用 stdio sidecar，因此预览模式只能看界面；真实协议由 sidecar 冒烟测试与 Rust↔Node 跨语言测试保障。
- 两侧各有一份版本常量，必须手工保持一致。不一致的后果是启动失败，这是有意为之——半懂协议的组合比启动失败更危险。
- 大块内容必须在各层截断：帧有 8 MiB 上限，转发再降到 1 MB，模型文本与工具输出摘要各自还有更小的上限。任何「把整份日志塞进一个事件」的做法都会撞上这些闸门。
- NDJSON 没有 schema 版本协商能力，字段变更只能靠「加可选字段」或提升协议版本。

## Alternatives considered

- **本地 HTTP 或 WebSocket。** 需要端口分配与冲突处理，并把一个能执行工具的接口暴露给本机其他进程。否决。
- **让 WebView 直接连 sidecar。** 会让界面绕过 Tauri 命令白名单，也会把凭据与权限边界搬到前端。否决。
- **用 Tauri 自带的 sidecar 通道而不是自己实现传输。** 无法满足同一条流上的双向请求与自定义取消语义，也难以在纯 Node 环境下测试。否决。
- **用 gRPC 或消息包编码。** 需要代码生成与额外的运行时依赖，换来的只是体积更小，而本项目的瓶颈不在这里。否决。
