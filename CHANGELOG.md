# 变更日志

本文件按「已标记的版本」记录用户可见的变化，格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)。

版本号遵循[语义化版本](https://semver.org/lang/zh-CN/)，并且是**唯一**的：`package.json`（六个）、Cargo workspace、`tauri.conf.json` 和 IPC fixture 里的版本由 `packages/shared/src/version.test.ts` 钉在一起，改一处而不改其余会让 `pnpm check` 变红。

`1.0.0` 之前不承诺向后兼容：Tauri IPC 命令、sidecar JSON-RPC 方法和跨层类型都可能变化，破坏性变化会写在这一节里。

## [未发布]

## [0.1.0] - 2026-09-12

首个带版本号、可对照与可回退的开发快照。

### 包含

- 桌面工作区：服务器管理、连接、概览健康快照、终端、远程文件、服务与日志、活动记录、Agent 对话历史。
- Agent 运行时（Node.js sidecar）：完整的 agent loop、内置工具、三层风险事实与唯一授权入口 Permission Engine、审批往返、取消与墙钟上限。
- OpenAI-compatible Provider（Chat Completions 与 Responses 两种方言）。
- 浏览器预览模式：`pnpm desktop:dev` 可以在没有 Rust 的情况下调界面，原生能力明确不可用。
- 完整校验门禁 `pnpm check`：公开文档卫生、凭据扫描、跨语言契约、类型检查、单元测试、sidecar 冒烟与 Rust 侧的 fmt/clippy/test。

### 尚未完成

`README.md` 的[当前限制](README.md#当前限制)一节列出**所有**已知缺口，并且是该列表的唯一来源；这一版仍未完成的部分以那一节为准。

[未发布]: https://github.com/Bad0RANG3/Yukinal/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Bad0RANG3/Yukinal/releases/tag/v0.1.0
