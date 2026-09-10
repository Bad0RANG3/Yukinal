# ADR 0005 Permission Engine 是唯一的执行授权决策者

Status: Accepted
Date: 2026-09-09

## Context

工具声明、输入内容和目标环境都会影响一次操作的风险。若每个工具、Provider 或模型都能自行决定是否允许执行，授权结果将无法解释，也无法保证不同远程目标之间的隔离。

## Decision

多个来源提供风险事实，但只有 `PermissionEngine.evaluate()` 可以合成最终决策：

| 来源 | 风险事实 |
| --- | --- |
| Tool declaration | 工具的静态风险 |
| `analyzeCommand()` | 输入命令的风险 |
| `ENVIRONMENT_RISK_FLOOR` | 目标环境的最低风险 |

Engine 取这些事实中的最高风险，再按策略得出 `auto`、`ask` 或 `deny`。以下约束不可由普通配置绕过：

- `critical` 风险不会被环境策略或 Agent 委托自动放行，必须得到用户对该次操作的明确审批。
- `auto` 委托只允许 Agent 在 `development` 或 `staging` 目标上自动批准普通写入 tier。高危操作、本机、未知目标和生产目标都必须等待用户。
- 会话授权只在当前运行中生效，并绑定具体工具、服务器和环境。
- ToolRegistry 只接受 `policy_auto`、明确标记委托来源的 `agent_auto`、当前运行会话授权的 `session_auto`，或与待处理审批完全匹配的 `user_approved` ticket。工具名、目标、来源或审批 ID 任一不匹配都必须拒绝。

## Consequences

- UI 可以展示构成决策的风险事实，用户能理解为什么操作被批准、等待或拒绝。
- 模型输出不能伪造授权来源。受限的运行级委托会明确记录为 `agent`，而规则升级不会产生第二个授权入口。
- 系统需要维护风险事实与 ticket 契约，但安全审计被集中到可测试的单一边界。
- 未来的团队策略或 RBAC 只能扩展策略来源，不能在其他模块再建立授权决策点。
