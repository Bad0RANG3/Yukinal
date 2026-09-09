# Architecture Decision Records

ADR 只记录会影响多个模块、公开协议、安全边界或数据生命周期，并且未来可能被重新讨论的长期决定。每条记录都应能脱离内部聊天或未发布材料独立阅读。

| ADR | 决定 | 状态 |
| --- | --- | --- |
| [0001](0001-agent-runtime-as-node-sidecar.md) | Agent Runtime 作为独立 Node.js sidecar | Accepted |
| [0002](0002-ssh-backend-russh.md) | SSH 后端使用 russh，并封装在 `crates/ssh` | Accepted |
| [0003](0003-openai-compatible-only-for-mvp.md) | MVP 先实现 OpenAI-compatible Provider | Accepted |
| [0004](0004-tool-name-mapping.md) | 内部使用点号工具名，Provider 边界使用双下划线 | Accepted |
| [0005](0005-permission-engine-sole-decision-maker.md) | Permission Engine 是唯一执行决策者 | Accepted |
| [0006](0006-sidecar-transport-ndjson-jsonrpc.md) | sidecar 使用 stdio 上的 NDJSON JSON-RPC | Accepted |
| [0007](0007-monorepo-and-day-one-abstractions.md) | 使用 pnpm + Cargo monorepo 和稳定抽象边界 | Accepted |
| [0008](0008-sidecar-launch-and-lifecycle.md) | Rust 负责 sidecar 启动、监督和生命周期 | Accepted |
| [0009](0009-agent-permission-delegation.md) | Agent 权限由用户选择询问或运行级委托 | Accepted |

新增 ADR 时使用下一个编号，至少包含 `Status`、`Context`、`Decision` 和 `Consequences`。实现状态发生变化时，同时更新相关 ADR、[文档入口](../README.md) 和根目录 [README](../../README.md)。
