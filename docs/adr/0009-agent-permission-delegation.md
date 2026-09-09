# ADR 0009：Agent 权限采用显式运行级委托

Status: Accepted
Date: 2026-09-09

## Context

Yukinal 原先只按目标环境策略执行：生产写入和危险操作会等待用户，开发环境的部分写入则直接自动执行。这个模型能保护边界，但无法提供常见 AI Agent 的两种工作方式：用户可以让 Agent 连续完成一项任务，也可以要求每次改变状态前都询问。

同时，放开自动执行不能意味着模型的一段文本就变成授权。否则无法区分策略批准、用户批准和 Agent 被委托后的自主判断，也无法在活动记录中说明结果由谁承担。

## Decision

每次 Agent run 接收一个可选的 `permissionMode`：

| 模式 | 行为 |
| --- | --- |
| `ask` | 只读操作保持自动；写入、部署、重启和危险操作在执行前等待用户批准。 |
| `auto` | 用户明确把本次运行的批准判断委托给 Agent；策略允许的操作由 Agent 自主批准执行，策略拒绝仍然拒绝。 |

省略该字段的 SDK 调用继续使用环境策略，以保持协议兼容。桌面 UI 默认使用 `ask`，并把选择随每次运行发送到 sidecar。

Permission Engine 仍是唯一生成 `ExecutionTicket` 的模块。`auto` 不是模型在消息中声明的权限，而是用户在运行级选择的委托。自动执行的审计来源分别记录为 `policy`、`agent` 或 `user`；Agent 自主批准不会伪装成用户批准。

严重风险在没有显式 `auto` 委托时仍需要用户批准；在 `auto` 模式下，策略允许的严重风险也会执行，但必须写入 `agent` 审计来源。任何 `deny` 决策都不可被模式覆盖。

## Consequences

- 用户可以在 Agent 输入区或系统设置中切换“操作前询问”和“委托 Agent 自动批准”，更接近现有 AI Agent 的使用体验。
- Agent 能连续完成被委托的任务，但每个自动结果都标记为 `Agent 自主批准`，并在执行审计中保留风险、目标和结果。
- ToolRegistry 增加 Agent 委托 ticket，仍校验工具名、解析目标、决策来源和风险，不接受模型伪造的 ticket。
- SQLite `tool_executions.approved_by` 增加 `agent` 值，并通过版本迁移兼容已有数据库。
