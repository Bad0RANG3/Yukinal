# 0001 — Agent Runtime 是独立 Node.js 进程（sidecar）

Status: accepted (2026-09)

## Context
允许 Agent Runtime 用 TypeScript，并要求它"独立于 UI"。宿主是 Tauri 2（Rust + 系统 WebView），
WebView 内没有 Node 运行时，因此"独立"必须具体化。

## Decision
`apps/agent` 是一个独立 Node.js 进程：

- 由 **Rust** 负责 spawn / kill / 崩溃重启（`agent_spawn` / `agent_kill`，见：UI 不管理进程）。
- 与 UI 之间只有 JSON-RPC（ADR 0006），没有共享内存、没有直接 require。
- Agent 进程 **不持有凭据**：需要 key 时通过 Tool → Rust 解析 `credentialRef`。
- 打包：生产用 bundle 后的单文件 + Node runtime（体积必须实测；热点允许下沉 Rust）。

## Consequences
- (+) MCP SDK / provider SDK / tool calling 全部可直接用，开发速度符合 的初衷。
- (+) Agent 崩溃不会带走窗口；Stop 可以真正 kill 掉执行体。
- (−) 多一套 IPC 契约与生命周期测试；多一个 runtime 的体积成本（必须量化，而不是假设）。
- (−) 任何"性能敏感或安全敏感"路径将来要下沉 Rust，接口必须先稳定 —— 所以 ADR 0007 第一天就钉抽象。
