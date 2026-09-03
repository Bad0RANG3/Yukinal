# 0005 — 三层风险事实，Permission Engine 是唯一决策者

Status: accepted (2026-09)

## Context
（工具静态风险）、（危险命令检测）、/（环境差异）各自都能提高风险，
但 又规定"权限不能由 LLM 自己决定"。如果三处各自 `return allow`，就没有单一真相。

## Decision
风险计算是 **事实（fact）生产者**，不是决策者：

| 层 | 生产者 | 产物 |
| --- | --- | --- |
| 1 静态 | Tool 声明（`declaration.risk`） | `ToolRiskFact` |
| 2 动态 | `analyzeCommand()` 规则集 | `CommandRiskFact` |
| 3 环境 | `ENVIRONMENT_RISK_FLOOR[environment]` | `EnvironmentRiskFact` |

`PermissionEngine.evaluate()` 独占合成：
`finalRisk = max(facts)` → `tier = tierOf(finalRisk)` → `mode = policy.tiers[tier]`，
并施加两条不可配置的下限：
1. `critical` 永远不能 `auto`（ 禁止隐藏危险操作）。
2. 会话级授权只覆盖 read/write tier，且按 `tool + serverId + environment` 隔离。

`ToolRegistry.execute()` 需要 **ExecutionTicket**：要么 `policy_auto`（决策为 auto），
要么 `user_approved`（携带匹配 `approvalId`）；决策绑定的 tool 与 target 必须与调用完全一致，否则拒绝。

## Consequences
- (+) 可以回答"为什么允许/为什么拒绝"：`decision.facts` 就是解释，直接渲染进 Approval UI。
- (+) 规则升级不会改变授权语义；策略改动不会绕过命令检测。
- (−) 多一层数据结构和一处必须始终同步的映射表。
- (−) 未来 team policy / RBAC只能扩展 `PermissionPolicy` 的来源，不能新增决策点。
