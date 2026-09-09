# Yukinal 文档

这份目录记录可以影响多个模块、协议或安全边界的长期决定。短期实现细节应写在代码注释和测试中；只有会反复被讨论的设计选择才进入 ADR。

## 阅读顺序

1. [仓库 README](../README.md)：能力范围、运行方式和当前限制。
2. [ADR 0001](adr/0001-agent-runtime-as-node-sidecar.md)：Agent 进程边界。
3. [ADR 0006](adr/0006-sidecar-transport-ndjson-jsonrpc.md)：sidecar 协议。
4. [ADR 0005](adr/0005-permission-engine-sole-decision-maker.md)：工具执行授权。
5. [ADR 0009](adr/0009-agent-permission-delegation.md)：用户可把运行级批准判断委托给 Agent。
5. 其余 ADR：SSH、Provider、工具命名、仓库结构和生命周期。

## 现行架构约束

- React 只能调用 `packages/shared` 中声明的 Tauri IPC 命令和事件。
- Rust 拥有 SSH、PTY、SQLite、系统凭据库和 sidecar 进程；Node.js Agent 不直接接触这些能力。
- `packages/shared` 是跨 Rust、TypeScript 和 UI 的协议真相源；改变事件或字段时必须同步 schema、转发层、消费者和测试。
- ToolRegistry 是工具执行入口，Permission Engine 是权限决策入口；模型输出不能绕过任一层。运行可选择操作前询问，或由用户明确委托 Agent 自主批准；策略拒绝始终有效。
- 日志走 stderr，JSON-RPC 帧走 stdout；输出、审计和等待状态必须有明确边界。

## ADR 索引

| 编号 | 决定 | 状态 |
| --- | --- | --- |
| [0001](adr/0001-agent-runtime-as-node-sidecar.md) | Agent Runtime 作为独立 Node.js sidecar | Accepted |
| [0002](adr/0002-ssh-backend-russh.md) | SSH 后端使用 russh，并封装在 `crates/ssh` | Accepted |
| [0003](adr/0003-openai-compatible-only-for-mvp.md) | MVP 先实现 OpenAI-compatible Provider | Accepted |
| [0004](adr/0004-tool-name-mapping.md) | 内部使用点号工具名，Provider 边界使用双下划线 | Accepted |
| [0005](adr/0005-permission-engine-sole-decision-maker.md) | Permission Engine 是唯一执行决策者 | Accepted |
| [0006](adr/0006-sidecar-transport-ndjson-jsonrpc.md) | sidecar 使用 stdio 上的 NDJSON JSON-RPC | Accepted |
| [0007](adr/0007-monorepo-and-day-one-abstractions.md) | 使用 pnpm + Cargo monorepo 和稳定抽象边界 | Accepted |
| [0008](adr/0008-sidecar-launch-and-lifecycle.md) | Rust 负责 sidecar 启动、监督和生命周期 | Accepted |

## 如何新增 ADR

当一个决定会改变两个或更多模块、公开协议、安全模型或数据迁移策略时，新增一条编号递增的 ADR。每条记录至少包含 `Status`、`Context`、`Decision` 和 `Consequences`，并在本索引和根目录 README 中加入链接。

文档必须只引用仓库中实际存在的文件、命令和能力，不依赖未发布的设计材料。实现状态改变时，优先更新 README、相关 ADR 和能力说明，避免继续保留“待实现”或“未接线”的过时描述。
