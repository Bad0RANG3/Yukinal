# ADR 0007 使用 monorepo 和稳定抽象边界

Status: Accepted
Date: 2026-09-09

## Context

Yukinal 同时包含 React、Node.js 和 Rust。Provider、工具执行、权限、SSH、采集、数据库和跨层事件都会持续演进；如果这些能力彼此直接依赖，实现细节会迅速扩散到整个仓库。

## Decision

使用 pnpm workspace 管理 TypeScript 包，使用 Cargo workspace 管理 Rust crate。优先稳定变化最频繁的边界，并以可编译、可测试的模块表达它们：

| 边界 | 位置 | 当前职责 |
| --- | --- | --- |
| Tool 和 ToolRegistry | `apps/agent/src/tools` | schema、超时、取消、重试和 ticket 校验 |
| Permission Engine | `apps/agent/src/permissions` 与 shared risk types | 策略与风险事实 |
| `LLMProvider` | `packages/provider-sdk` | Provider 抽象与统一事件 |
| Provider 配置 | `packages/shared/src/types/provider.ts` 与 SQLite repository | 配置、密钥引用和导入模型 |
| `SshBackend` | `crates/ssh` | russh 实现与上层出口 |
| Collector | `crates/collector` | 服务器信息采集与解析 |
| 跨层契约与事件 | `packages/shared` | 类型、Zod schema、IPC、事件与 JSON-RPC |

实现细节只能向模块内部依赖；上层通过接口或 schema 使用能力。Rust 命令层是 React 访问原生资源的唯一出口。

## Consequences

- 前端、Agent 和 Rust 可以并行演进，契约变化会在构建与测试中暴露。
- 初期需要更多目录与抽象，但避免将 Provider、SSH 或安全判断复制到 UI。
- workspace 检查必须覆盖两套工具链和跨语言测试，因此这些流程需要持续保持可运行。
- 新能力应先确认所属边界，再判断是否需要公共抽象，避免为假设中的未来过早扩大接口。
