# Architecture Decision Records

这里记录 Yukinal 的长期架构决策。ADR 用于解释为什么某个边界、协议或安全规则以当前形式存在，而不是充当实现日志或功能待办。

每份记录都应能独立阅读：它需要交代当时的问题、做出的选择，以及选择带来的代价与约束。

| ADR | 决定 | 状态 |
| --- | --- | --- |
| [0001](0001-agent-runtime-as-node-sidecar.md) | Agent Runtime 作为独立 Node.js sidecar | Accepted |
| [0002](0002-ssh-backend-russh.md) | SSH 后端采用 russh 并封装在 `crates/ssh` | Accepted |
| [0003](0003-openai-compatible-only-for-mvp.md) | MVP 只实现 OpenAI-compatible Provider | Accepted |
| [0004](0004-tool-name-mapping.md) | 内部工具名使用点号，Provider 边界使用双下划线 | Accepted |
| [0005](0005-permission-engine-sole-decision-maker.md) | Permission Engine 是唯一的执行授权决策者 | Accepted |
| [0006](0006-sidecar-transport-ndjson-jsonrpc.md) | sidecar 通过 stdio 上的 NDJSON JSON-RPC 通信 | Accepted |
| [0007](0007-monorepo-and-day-one-abstractions.md) | 使用 pnpm 与 Cargo monorepo，并维持稳定抽象边界 | Accepted |
| [0008](0008-sidecar-launch-and-lifecycle.md) | Rust 负责 sidecar 的启动、监督和回收 | Accepted |
| [0009](0009-agent-permission-delegation.md) | Agent 权限使用显式的运行级委托 | Accepted |

新增记录时使用下一个编号，并包含 `Status`、`Context`、`Decision` 和 `Consequences`。如果实现调整改变了已有决定的真实含义或状态，请更新对应 ADR，同时更新[文档入口](../README.md)和根目录 [README](../../README.md)。
