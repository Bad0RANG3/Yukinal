# Architecture Decision Records

本目录记录 Yukinal 的长期架构决策。每条 ADR 回答一个具体问题：**当时面对的是什么约束，我们选了哪条路，以及这条路让我们付出了什么代价。**

ADR 不是实现日志，也不是功能待办。它不描述代码怎么写，只描述为什么是这个形状——因为代码本身已经说明了「是什么」，而「为什么」会在一次重构之后彻底消失。

每份记录都应能独立阅读。读者不需要读过其他 ADR，就能理解这一条在讲什么、以及它约束了哪些模块。

## 什么时候需要一条 ADR

满足下列任意一条，就应该新增一条；否则把理由写进代码注释就够了。

- **决定影响两个以上模块。** 例如工具名在内部与 Provider 边界使用不同拼写，这件事同时约束了 `packages/shared`、`packages/provider-sdk`、agent loop、registry、审计表和界面显示。
- **决定定义了一个公开协议。** 例如 Tauri IPC 命令表、sidecar 的 NDJSON JSON-RPC 方法名与版本号、审计表的字段语义。改动它们会让另一侧编译或运行失败。
- **决定属于安全模型。** 例如谁有权生成执行票据、审批的四种来源分别意味着什么、凭据允许出现在哪些边界内。这类决定无法靠单侧测试证明其正确性。
- **决定涉及数据迁移策略。** 例如为了记录 Agent 委托而重建 `tool_executions` 表：迁移一旦发布就不能再改，必须留下当时的选择与代价。
- **决定是一次有代价的取舍。** 例如「暂不支持 ssh-agent」，读者需要知道这是有意为之，而不是遗漏。

反过来，以下情形不需要 ADR：给某个工具调风险等级、调整上限常量、重命名内部函数、修改界面文案。这些应该写在代码或测试里。

## 必需的格式

```markdown
# ADR NNNN 标题（一句话说明决定了什么）

Status: Accepted
Date: YYYY-MM-DD

## Context

当时面对什么问题、哪些约束是硬的、为什么现状不可接受。

## Decision

决定了什么。要点是可验证的：写清楚谁负责什么、边界画在哪里、哪些行为被禁止。

## Consequences

这个决定带来的收益，以及它明确让我们承担的成本与限制。只写好处等于没写。

## Alternatives considered

（可选）被认真考虑过但被否决的方案，以及否决理由。
```

约定：

- 文件名是 `NNNN-短横线标题.md`，四位编号、零填充，编号只增不减，重命名文件时同步更新所有引用。
- `Status` 目前只有 `Accepted`；若某条决定被后续 ADR 取代，在该条状态里注明被哪一条取代，并保留原文。
- 路径、命令、类型名、常量名必须与仓库一致。文档里出现的每个命令都应该能在 `package.json` 的 `scripts` 中找到，或者是一次真实的二进制调用。
- 不要引用仓库之外的设计材料。读者打不开的东西等于没写：把「为什么」直接讲清楚。
- 日期表示该决定被接受的日期，不要在原文上改日期来假装决定一直如此。

## 索引

| 编号 | 决定 | 状态 |
| --- | --- | --- |
| [0001](0001-agent-runtime-as-node-sidecar.md) | Agent Runtime 作为独立 Node.js sidecar，由 Rust 拥有其生命周期 | Accepted |
| [0002](0002-ssh-backend-russh.md) | SSH 后端采用 russh，并封装在 `crates/ssh` 的 `SshBackend` 之后 | Accepted |
| [0003](0003-openai-compatible-only-for-mvp.md) | 只实现一个 OpenAI-compatible Provider，两种请求方言覆盖兼容端点 | Accepted |
| [0004](0004-tool-name-mapping.md) | 内部工具名用点号，Provider 边界用双下划线，映射集中在一处 | Accepted |
| [0005](0005-permission-engine-sole-decision-maker.md) | Permission Engine 是唯一的执行授权决策者 | Accepted |
| [0006](0006-sidecar-transport-ndjson-jsonrpc.md) | sidecar 通过 stdio 上的 NDJSON JSON-RPC 通信，协议版本 `1.0` | Accepted |
| [0007](0007-monorepo-and-day-one-abstractions.md) | pnpm 与 Cargo 双 workspace，并优先稳定变化最频繁的边界 | Accepted |
| [0008](0008-sidecar-launch-and-lifecycle.md) | Rust 负责 sidecar 的启动、握手、监督与回收 | Accepted |
| [0009](0009-agent-permission-delegation.md) | Agent 权限使用显式的运行级委托，与运行模式互相正交 | Accepted |

新增记录后，请同时更新[文档入口](../README.md)的表格与根目录 [README](../../README.md) 的文档索引。如果某个决定的实际含义已经变化，也要一并更新受影响的[能力边界说明](../../apps/agent/src/providers/README.md)。
