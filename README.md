# Yukinal

[![license: PolyForm Noncommercial 1.0.0](https://img.shields.io/badge/license-PolyForm_Noncommercial_1.0.0-blue)](LICENSE)
![status: early](https://img.shields.io/badge/status-very_early-yellow)

> **非商业许可**:本仓库使用 [PolyForm Noncommercial 1.0.0](LICENSE) —— 可自由学习、修改、
> 非商业地分发与引用(注明来源即可),**商业用途需要先取得单独授权**。详见 [NOTICE](NOTICE)。

**AI-native remote development & infrastructure workspace.**
不是"更好看的 SSH 客户端",而是一个由 agent 驱动的远程开发 / 部署 / 排障工作台。

名字取自 **yugal / 拉丁 iugum**「轭」:把人的意图和机器的执行力套进同一副轭,同向用力。
你说要什么,agent 去环境里查、去改、去验证,然后把过程讲清楚。

> ⚠️ 非常早期。目前只有桌面外壳、连接与 agent 运行时骨架,还没有可发布的版本。

## 它做什么

```text
你的意图 → Agent → 上下文 → 工具 → 权限 → 执行 → 验证 → 结果
```

一个具体的例子(我们正在把它跑通):

```text
你:「检查 staging API 为什么一直重启」
    agent: server.info → docker.ps → docker.logs → docker.inspect
    agent: 根因是 .env 里缺 DATABASE_URL
你:「帮我修」
    agent: 读文件 → 写文件（请求批准）→ docker.restart → health check
    agent: 报告改了什么、为什么、验证结果
```

设计上坚持三件事:

- **信息优先**:打开服务器看到的是健康度和结论,不是 `df -h` 的输出。
- **每一步可见**:agent 看了什么、调了什么工具、拿到什么结果,全部是 UI 里的卡片。
- **危险操作必须显式批准**:权限由「用户策略 + 工具风险 + 目标环境」决定,不由模型自己决定。

## 架构

```text
┌──────────────────────────────────────────┐
│ React (Tauri WebView)                     │
│  Server 列表 / Overview / Terminal / Agent │
└───────────────┬──────────────────────────┘
                │ Tauri IPC（唯一白名单，见 packages/shared）
┌───────────────▼──────────────────────────┐
│ Rust core                                 │
│  ssh · pty · sftp · collector · sqlite    │
│  credentials(OS keychain) · sidecar 监管  │
└──────┬───────────────────────┬───────────┘
       │ SSH                   │ stdio (NDJSON JSON-RPC)
   远端服务器            ┌──────▼───────────────────────┐
                        │ Agent runtime (Node.js)      │
                        │  loop · tool registry ·       │
                        │  permission engine · trace    │
                        └──────┬───────────┬───────────┘
                               │           │
                          Built-in      MCP / Providers
                          tools         (规划中)
```

- 密钥只存在系统凭据库(Windows Credential Manager / macOS Keychain / Linux Secret Service),
  数据库里只有 `credential_ref`。
- Agent 不直接碰 SSH、不持有凭据:`Agent → Tool → Permission → Rust → SSH`。
- 所有能力统一成带 schema 的 Tool,内部用点号命名(`docker.ps`),送给模型时才映射成
  `docker__ps`。

## 仓库结构

```text
apps/
  desktop/        Tauri 2 + React + Vite + Tailwind（UI 不含任何原生逻辑）
    src-tauri/    Rust 命令层：原生能力的唯一出入口
  agent/          Node.js sidecar：agent loop / tool registry / permission engine / trace
packages/
  shared/         跨层契约：类型 + zod schema + 事件名 + IPC 映射 + 帧协议
  provider-sdk/   LLMProvider 抽象 + provider 边界的工具名映射
  agent-sdk/      桌面 ↔ sidecar 的 typed client
crates/
  core  ssh  terminal  collector  credentials  database  filesystem
docs/adr/         架构决策记录（为什么这么选）
```

## 开发

前置:Node ≥ 24、pnpm 11、Rust stable(MSVC / Xcode CLT / Linux 上 `libwebkit2gtk-4.1-dev`)。

```bash
pnpm install
pnpm check                          # 完整门禁，见下
pnpm --filter @yukinal/desktop dev  # 浏览器里看 UI 外壳（原生能力会明确提示不可用）
pnpm --filter @yukinal/desktop tauri dev   # 桌面壳（含 Rust 编译）
pnpm --filter @yukinal/agent start         # 单独跑 sidecar，stdin 说协议、stderr 说日志
```

`pnpm check` 一条命令跑完:契约库构建 → 全量 typecheck → agent bundle → TS 单测 →
sidecar stdio 往返 smoke → `cargo fmt` / `clippy -D warnings` / `check` →
**Rust 监管真 Node sidecar 的跨语言握手测试**。CI 在 ubuntu / windows / macOS 上跑同一条命令。

改图标:`powershell -File scripts/generate-app-icon.ps1` 生成源图,再
`pnpm --filter @yukinal/desktop icon` 展开全平台图标。

## 决策记录

- [ADR 0001](docs/adr/0001-agent-runtime-as-node-sidecar.md) — agent 运行时是独立 Node 进程
- [ADR 0002](docs/adr/0002-ssh-backend-russh.md) — SSH backend 用 russh
- [ADR 0003](docs/adr/0003-openai-compatible-only-for-mvp.md) — 先只支持 OpenAI-compatible
- [ADR 0004](docs/adr/0004-tool-name-mapping.md) — 工具名内部点号、模型边界双下划线
- [ADR 0005](docs/adr/0005-permission-engine-sole-decision-maker.md) — 三层风险事实,Permission Engine 唯一决策
- [ADR 0006](docs/adr/0006-sidecar-transport-ndjson-jsonrpc.md) — sidecar 传输:stdio 上的 NDJSON JSON-RPC
- [ADR 0007](docs/adr/0007-monorepo-and-day-one-abstractions.md) — 第一天钉死的核心抽象
- [ADR 0008](docs/adr/0008-project-name-yukinal.md) — 为什么叫 Yukinal
- [ADR 0009](docs/adr/0009-sidecar-launch-and-lifecycle.md) — sidecar 的启动方式与生命周期规则

## License

**PolyForm Noncommercial License 1.0.0** —— 见 [LICENSE](LICENSE)。要点:

- ✅ 允许:学习、使用、修改、为非商业目的分发(个人研究/实验/爱好、教育与公益机构等)。
- ❌ 不允许:任何商业用途。需要商用的话,请单独联系版权人拿许可。
- 📌 再分发时必须带上本许可证全文与 `NOTICE` 里的署名行;改了也要说清楚。
- 🚫 不提供任何担保,也不额外授予商标权;专利权按许可证原文。

第三方依赖仍受各自许可证约束(`MIT` / `Apache-2.0` / `BSD` 等)——本许可证只覆盖本仓库原创的部分。

Copyright (c) 2026 Bad0RANG3
