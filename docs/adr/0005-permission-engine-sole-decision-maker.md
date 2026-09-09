# ADR 0005：Permission Engine 是唯一执行决策者

Status: Accepted
Date: 2026-09-09

## Context

工具声明、输入内容和目标环境都可能改变一次操作的风险。如果每个工具、Provider 或模型都能独立决定“允许执行”，授权结果就无法解释，也无法保证远程目标之间的隔离。

## Decision

风险事实由多个来源提供，但只有 `PermissionEngine.evaluate()` 合成最终决策：

| 来源 | 事实 |
| --- | --- |
| Tool declaration | 工具静态风险 |
| `analyzeCommand()` | 输入中的命令风险 |
| `ENVIRONMENT_RISK_FLOOR` | 目标环境最低风险 |

Engine 取这些事实中的最高风险，并根据策略映射为 `auto`、`ask` 或 `deny`。以下约束不可被配置覆盖：

- `critical` 风险不能由环境策略静默自动放行；只有用户明确选择本次运行的 `auto` 委托时，才允许以 `agent` 来源执行，否则必须有用户审批。
- 会话授权只在当前运行内扩大范围，并按工具、服务器和环境绑定。
- ToolRegistry 只接受 `policy_auto`、带有明确委托来源的 `agent_auto`、当前运行会话授权的 `session_auto`，或与待处理审批完全匹配的 `user_approved` ticket；工具名、目标、来源和审批 ID 任一不匹配都拒绝执行。

## Consequences

- UI 可以展示构成决策的 facts，用户能够理解为什么需要审批或为什么被拒绝。
- 模型输出不能伪造权限来源；用户选择的运行级 Agent 委托会显式标记为 `agent`，规则升级不会改变授权链的单一入口。
- 增加了风险事实和 ticket 契约，但把安全审计集中在一个可测试的边界内。
- 未来的团队策略或 RBAC 只能扩展策略来源，不能在其他模块增加第二个授权点。
