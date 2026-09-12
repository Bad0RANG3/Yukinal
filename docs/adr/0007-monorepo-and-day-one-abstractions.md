# ADR 0007 使用 monorepo 和早期就稳定的抽象边界

Status: Accepted（Decision 一节的 `members` 列表写于 `crates/time` 出现之前，漏了它：根 `Cargo.toml` 现在还有 `crates/time`（`yukinal-time`，时间戳的唯一实现，被 `crates/core` 与 `crates/terminal` 依赖）。它正好是成本一节那句话的现成例子 —— 新增成员要按同一套标准被审视，而不是只往列表里补一行。双 workspace、依赖集中声明、契约库先构建与 fixture 双向校验这些结论不变）
Date: 2026-09-09

## Context

Yukinal 同时包含三种语言与四类关注点：React 界面、Node.js Agent、Rust 原生核心，以及它们之间共享的契约。Provider、工具执行、权限、SSH、采集、数据库和跨层事件都会持续演进。

如果让这些能力彼此直接依赖，实现细节会迅速扩散到整个仓库：界面开始知道 SSH 会话长什么样，Agent 开始关心 SQLite 表结构，某个 Provider 的字段名出现在审计表里。一旦发生这种情况，任何单点改动都需要跨语言同步，而「某处还在用旧字段」这类错误只有在运行时才会暴露。

## Decision

使用 pnpm workspace 管理 TypeScript 包，使用 Cargo workspace 管理 Rust crate，并优先把**变化最频繁、被引用最多**的边界固定成可编译、可测试的接口。

```yaml
# pnpm-workspace.yaml（节选）
packages:
  - "apps/*"
  - "packages/*"
```

```toml
# Cargo.toml（节选）
[workspace]
members = [
  "apps/desktop/src-tauri",
  "crates/collector", "crates/core", "crates/credentials",
  "crates/database", "crates/filesystem", "crates/ssh", "crates/terminal",
]
```

依赖版本集中声明在根 `Cargo.toml` 的 `[workspace.dependencies]`，内部 crate 以路径引用（`yukinal-core = { path = "crates/core" }`），因此「谁依赖什么」在一个文件里就能读完。几个关键选择是有理由的，也一并固定在这里：`rusqlite` 用 `bundled` 以省掉系统 SQLite 差异；`keyring` 启用三个平台的原生后端；`russh` 用 `ring` 而不是 `aws-lc-rs`（理由见 [ADR 0002](0002-ssh-backend-russh.md)）。

被固定下来的边界：

| 边界 | 位置 | 当前职责 |
| --- | --- | --- |
| 工具与 ToolRegistry | `apps/agent/src/tools` | 声明校验、输入 schema、超时、取消、重试、票据复核 |
| Permission Engine | `apps/agent/src/permissions` + `packages/shared/src/types/risk.ts` | 策略表与风险事实的合成 |
| `LLMProvider` | `packages/provider-sdk` | Provider 抽象、统一的 `StreamEvent`、Provider 侧工具名映射 |
| Provider 配置 | `packages/shared/src/types/provider.ts` + SQLite repository | 持久化配置、凭据引用、每运行注入的运行时配置 |
| `SshBackend` | `crates/ssh` | russh 实现与其唯一出口 |
| 采集 | `crates/collector` | 采集器、解析、本地与 SSH 两种 runner |
| 凭据 | `crates/credentials` | `keychain://` 引用与三个平台的原生后端 |
| 跨层契约 | `packages/shared` | 类型、Zod schema、IPC 命令表、事件名、JSON-RPC 协议、命名规则、契约 fixture |

配套的三条工程规则：

- **实现细节只能向内依赖。** 上层通过接口或 schema 使用能力；`crates/*` 的公开类型不包含后端库的类型（例如没有任何 russh 类型出现在 `SshBackend` 之上）。
- **契约库先构建。** 消费方导入的是 `packages/*/dist/*.d.ts`，所以本地门禁的第一步就是构建 `@yukinal/shared`、`@yukinal/provider-sdk`、`@yukinal/agent-sdk`，之后才做类型检查。顺序不能颠倒。
- **契约在运行时也被校验。** 每个 Tauri 命令在界面侧用共享的 Zod schema 解析参数与返回值（`IPC_SCHEMAS`），Rust 侧用 camelCase 对齐的 serde 结构与 Tauri 命令参数承载同一份形状（宿主请求结构体还会用 `deny_unknown_fields` 拒绝多余字段）；`packages/shared/fixtures/ipc/` 下的 JSON 被 Rust 的 `include_str!` 与 TypeScript 测试同时解析，任一侧的序列化漂移都会让构建失败。类型只保证编译期一致，fixture 保证运行时一致。

## Consequences

**收益**

- 前端、Agent 与原生核心可以并行演进：契约变化会先表现为构建或测试失败，而不是线上行为异常。
- 跨语言的「真相」只有一份。事件名、命令名和协议版本都定义在 `packages/shared`，两侧各自引用而不是各自维护。
- Rust 侧的边界让原生逻辑可以脱离窗口测试：`yukinal-core` 里没有 Tauri 类型，因此 sidecar 启动、监督、崩溃与状态路径都能在单元测试中覆盖。
- 依赖审计集中：新增一个原生依赖需要在根 `Cargo.toml` 里连同理由一起声明；pnpm 侧对依赖的生命周期脚本采用显式白名单，不会因为装一个包就悄悄执行构建脚本。

**成本**

- 目录与抽象比「一个应用目录搞定」多得多；初期的样板代码量与阅读成本都更高。
- 门禁必须覆盖两套工具链和跨语言测试，因此 `pnpm check` 步骤较多，任何一步不稳定都会拖慢所有人；这也是把它做成单一入口并让 CI 跑同一条命令的原因。
- 抽象容易过剩。这里曾经有过一个实例：`crates/filesystem` 长期只有文档注释，既没有实现也没有被任何 crate 引用，而「远端文件能不能碰」的真实规则同时散落在 `commands/host.rs`（凭据路径黑名单、路径校验、读写上限）和 `commands/files.rs`（另一份 1 MiB 截断副本）里 —— 占位符本身没有价值，价值在于它逼出的那个问题：**到底谁该拥有这条规则**。它现在有了实现与消费方（策略、上限、有界读写归该 crate，传输由桌面层注入），这也让这条成本的准确说法变得更清楚：代价不是「多了一个目录」，而是「先写下边界、再证明它值得存在」这段时间里，规则会继续留在错误的地方，而这段时间必须短。另一处需要准确的细节：`Cargo.toml` 里两条目性质不同 —— `[workspace.dependencies]` 的那条别名在没人引用时确实是惰性的（不参与编译），但 `members` 里那条会让它进入 `cargo check --workspace`，只是它没有内容，编译一瞬即过。原文写成"未使用的 workspace 依赖不参与编译"，把这两者混为一谈了，这里更正 —— 否则后来者会以为往 `members` 里加空成员是零成本的。它提醒的是：新增边界之前要先确认真的有两个以上消费方。
- `packages/shared` 会变成热点。任何跨层改动都要动它，因此它必须保持没有副作用、没有运行时依赖（只有 `zod`），否则会拖慢每一次构建。

## Alternatives considered

- **每个应用一个独立仓库。** 跨语言契约会被复制到多份，版本对齐只能靠人工，而本项目的核心风险恰好是「两侧理解不一致」。否决。
- **单一语言实现（全部 Rust 或全部 TypeScript）。** 前者会失去 Provider 与工具迭代速度，后者无法直接操作 SSH、PTY、系统凭据库与 SQLite。否决。
- **只做类型共享、不做运行时校验。** 类型在 JSON 边界上是无效的：Rust 序列化出的 `null` 与 TypeScript 的可选字段在编译期看不出冲突。fixture 与 Zod 校验就是为此存在的。否决。
- **不做内部抽象，让上层直接用 russh / rusqlite / keyring 的类型。** 会让后端库的版本升级变成全仓库改造，也会让权限和界面逻辑依赖第三方 API 形状。否决。
