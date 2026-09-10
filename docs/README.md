# Yukinal 文档

本目录保存跨多个模块、协议或安全边界的长期说明。短期实现细节应与代码和测试放在一起；只有会反复影响设计、兼容性或安全模型的决定才需要进入这里。

## 从这里开始

1. 阅读[项目 README](../README.md)，了解产品范围、开发方式和当前限制。
2. 阅读 [ADR 0001](adr/0001-agent-runtime-as-node-sidecar.md) 与 [ADR 0008](adr/0008-sidecar-launch-and-lifecycle.md)，了解 sidecar 进程边界和生命周期。
3. 阅读 [ADR 0006](adr/0006-sidecar-transport-ndjson-jsonrpc.md)，了解 Rust 与 Agent 之间的协议。
4. 阅读 [ADR 0005](adr/0005-permission-engine-sole-decision-maker.md) 与 [ADR 0009](adr/0009-agent-permission-delegation.md)，了解授权决策和运行级委托。
5. 再按需要查阅 SSH、Provider、工具命名和 monorepo 相关的 ADR。

## 必须保持的架构边界

- React 只能使用 `packages/shared` 声明的 Tauri IPC 命令和事件。
- Rust 拥有 SSH、PTY、SQLite、系统凭据库和 sidecar 进程；Node.js Agent 不应直接访问这些资源。
- `packages/shared` 是跨 Rust、TypeScript 与 UI 的契约来源。任何事件或字段的调整都必须同步 schema、转发层、消费者和测试。
- ToolRegistry 是工具执行入口，Permission Engine 是授权决策入口。模型输出不能绕过其中任何一层；Agent 的自动委托仅适用于开发或预发布目标上的普通写入，高危、本机、未知和生产操作必须等待用户。
- sidecar 的 stdout 仅承载 JSON-RPC 帧，stderr 仅承载日志；传输、审计和等待状态都必须有明确上限。

## ADR 索引

| 编号 | 决定 | 状态 |
| --- | --- | --- |
| [0001](adr/0001-agent-runtime-as-node-sidecar.md) | Agent Runtime 作为独立 Node.js sidecar | Accepted |
| [0002](adr/0002-ssh-backend-russh.md) | SSH 后端采用 russh 并封装在 `crates/ssh` | Accepted |
| [0003](adr/0003-openai-compatible-only-for-mvp.md) | MVP 只实现 OpenAI-compatible Provider | Accepted |
| [0004](adr/0004-tool-name-mapping.md) | 内部工具名使用点号，Provider 边界使用双下划线 | Accepted |
| [0005](adr/0005-permission-engine-sole-decision-maker.md) | Permission Engine 是唯一的执行授权决策者 | Accepted |
| [0006](adr/0006-sidecar-transport-ndjson-jsonrpc.md) | sidecar 通过 stdio 上的 NDJSON JSON-RPC 通信 | Accepted |
| [0007](adr/0007-monorepo-and-day-one-abstractions.md) | 使用 pnpm 与 Cargo monorepo，并维持稳定抽象边界 | Accepted |
| [0008](adr/0008-sidecar-launch-and-lifecycle.md) | Rust 负责 sidecar 的启动、监督和回收 | Accepted |
| [0009](adr/0009-agent-permission-delegation.md) | Agent 权限使用显式的运行级委托 | Accepted |

## 更新文档

当一个决定影响两个以上模块、公开协议、安全模型或数据迁移策略时，新增一条编号递增的 ADR。每条 ADR 至少应包含 `Status`、`Context`、`Decision` 和 `Consequences`，并更新本索引与根目录 README。

写入文档的陈述必须与仓库中实际存在的代码、命令和能力相符。实现范围发生变化时，应同步更新相关 ADR、能力边界文档和 README，避免将规划描述成已可用功能。
