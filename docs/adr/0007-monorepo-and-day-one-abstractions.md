# 0007 — 第一天建立 monorepo，并钉死 7 个抽象

Status: accepted (2026-09)

## Context
可以简单，但 `LLMProvider / Tool / ToolRegistry / PermissionPolicy / SshBackend / Collector / Provider`
必须从第一天就抽象，因为它们是变化最大的地方。

## Decision
第一步就建立目标目录结构（`apps/ packages/ crates/` + pnpm workspace + Cargo workspace），
并把 7 个抽象写成**已存在、可编译、可测试**的接口或骨架：

| 抽象 | 位置 | 当前状态 |
| --- | --- | --- |
| `Tool` / `ToolRegistry` | `apps/agent/src/tools/*` | 已实现（含超时/取消/重试/票据校验） |
| `PermissionPolicy` / Engine | `packages/shared/src/types/risk.ts` + `apps/agent/src/permissions/*` | 已实现（策略表 =） |
| `LLMProvider` | `packages/provider-sdk/src/types.ts` | 接口已定，实现待 provider 步骤 |
| `Provider`(infra) | `packages/shared/src/types/provider.ts` | 配置模型已定，属于后续能力 |
| `SshBackend` | `crates/ssh/src/lib.rs` | trait + 数据类型已定，实现待 SSH 步骤 |
| `Collector` | `crates/collector/src/lib.rs` | trait + 数据结构已定，实现待采集步骤 |
| 契约/事件 | `packages/shared` | 全覆盖 |

## Consequences
- (+) 之后的工作是"填实现"，不再是"发明形状"，跨层改动成本被压在最小。
- (−) 初始化的 diff 很大；靠「每阶段只做该阶段的事」这条纪律约束，不再顺手扩面。
- (+) `crates/*` 起步保持零依赖，因此 `cargo check` / `clippy -D warnings` 第一时间就是全绿的；
  russh / sqlite / keyring 带来的真实编译与跨平台风险留到各自落地时再承担。
