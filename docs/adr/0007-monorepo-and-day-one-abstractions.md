# ADR 0007：使用 pnpm + Cargo monorepo 和稳定抽象边界

Status: Accepted
Date: 2026-09-09

## Context

Yukinal 同时包含 React、Node.js 和 Rust。Provider、工具执行、权限、SSH、采集、数据库和跨层事件都会持续变化；如果这些能力直接互相依赖，功能增长会把实现细节扩散到整个仓库。

## Decision

使用 pnpm workspace 管理 TypeScript 包，使用 Cargo workspace 管理 Rust crate，并把变化最大的边界固定为可编译、可测试的模块：

| 边界 | 位置 | 当前状态 |
| --- | --- | --- |
| Tool / ToolRegistry | `apps/agent/src/tools` | 已实现：schema、超时、取消、重试和 ticket 校验 |
| Permission Engine | `apps/agent/src/permissions` + shared risk types | 已实现：策略和风险 facts |
| `LLMProvider` | `packages/provider-sdk` | 接口已定，OpenAI-compatible 实现已接入 |
| Provider 配置 | `packages/shared/src/types/provider.ts` + SQLite repository | 已实现：配置、密钥引用和导入模型 |
| `SshBackend` | `crates/ssh` | russh 实现已接入，trait 作为上层出口 |
| Collector | `crates/collector` | 采集器和解析器已实现并可独立测试 |
| 跨层契约与事件 | `packages/shared` | 类型、Zod schema、IPC、事件和 JSON-RPC 已实现 |

每个边界的实现细节只能向内依赖；上层通过接口或 schema 使用能力。Rust 命令层是 React 访问原生能力的唯一出口。

## Consequences

- 前端、Agent 和 Rust 可以并行演进，契约变更能够在构建和测试阶段暴露。
- 第一次建立目录和抽象需要更多文件，但避免把 Provider、SSH 或安全判断复制到 UI。
- workspace 检查会同时覆盖 TypeScript 和 Rust；需要保持两套工具链和跨语言测试可运行。
- 新能力应先明确它属于哪个边界，再决定是否需要新增公共抽象，避免为了未来可能性提前扩展接口。
