# 架构总览

```text
┌───────────────────────────────────────────────────────────────────┐
│ React 19 + Vite（apps/desktop/src）                                │
│ 服务器 · 概览 · 终端 · 文件 · 日志 · 服务 · 活动 · Agent 面板        │
└──────────────────────────────┬────────────────────────────────────┘
                               │ 白名单 Tauri IPC：命令 + 事件
┌──────────────────────────────▼────────────────────────────────────┐
│ Rust 宿主（apps/desktop/src-tauri + crates/*）                     │
│ commands：服务器 · 终端 · 文件 · 日志 · 服务 · 活动 · Provider · MCP · Agent│
│ core：SSH · PTY · 采集 · SQLite · OS 凭据库 · sidecar 启动与监督      │
└───────┬───────────────────────────────────────┬───────────────────┘
        │ SSH / SFTP / PTY                      │ stdio 上的 NDJSON JSON-RPC
        ▼                                       ▼
┌───────────────────┐              ┌─────────────────────────────────┐
│ 远程服务器         │              │ Node.js Agent sidecar           │
│ systemd / Docker   │              │ agent loop · tools · 权限引擎    │
└───────────────────┘              │ providers/                       │
                                   └───────────────┬─────────────────┘
                                                   │ HTTPS + SSE
                                                   ▼
                                       ┌───────────────────────────┐
                                       │ OpenAI-compatible 端点     │
                                       │ api.anthropic.com          │
                                       │ generativelanguage...      │
                                       └───────────────────────────┘
```

**通信方式。** Rust 启动 sidecar 并持有句柄，双方通过 stdin/stdout 传 NDJSON 编码的 JSON-RPC 2.0，每行一个完整帧，协议版本常量 `1.0`（`packages/shared/src/protocol/jsonrpc.ts` 与 `crates/core/src/sidecar/mod.rs` 各有一份，必须一致）。`initialize` 必须是第一条请求，握手成功后 Rust 还会调用 `system.describe`，协议版本不匹配或报告存在工具名冲突时**拒绝发布这个 sidecar**。反向请求复用同一条流：`host.tool.execute`、`host.context.fetch`、`host.tool.cancel`、`host.mcp.catalog` 都由 Rust 处理。

sidecar 的 stdout 只承载协议帧，**所有日志写 stderr**——一条走错位置的 `console.log` 就会破坏协议。单帧上限 8 MiB，超限帧被丢弃并记录；畸形帧不会杀死进程，只会被报告并跳过；`agent.run.start` 的响应帧必须先于该次运行的任何通知发出，否则界面会先看到事件后拿到 `runId`；带 `messageId` 的 `run.start` 记入一张最多 256 条的受理表，同消息重发且内容一致返回同一个 `runId`（`duplicate: true`），已有受理记录不会被淘汰。宿主侧还有第二道闸门：只转发白名单事件类型、要求 `runId` 非空且不超过 256 字符、单个负载限制 1 MB（比传输层更紧）。

**分层的实际约束。** 这些边界一旦被跨过，就会同时破坏可解释性和可测试性；改动它们需要先写一条新的 ADR。

- **React 只能使用 `packages/shared` 声明的 Tauri IPC 命令与事件。** `IPC_COMMANDS` 与 `EVENT_NAMES` 是白名单：不在其中的原生能力对界面不存在，界面也不能自己启动进程、建立 SSH 连接或读取凭据。契约的两半都有运行期闸门，都在 `apps/desktop/src/lib/ipc.ts`：命令走 `callDesktop()`（参数与返回值用 `IPC_SCHEMAS` 解析），事件走 `listenDesktop()`（负载用 `EVENT_SCHEMAS` 解析）。两者都是解析而不是类型断言——`terminal.data` 携带的是远端主机读回来的字节，正是不能靠断言的地方。事件是通知而非请求，负载校验失败只能丢弃并留下一次警告，不能被「重新请求」。
- **Rust 拥有原生资源，且只做参数编组。** SSH 会话、PTY、SQLite、操作系统凭据库、sidecar 与 MCP 子进程句柄都由 Rust 持有。`apps/desktop/src-tauri` 只做参数编组与事件转发，逻辑落在 `crates/*`，这样不打开窗口也能测试——`yukinal-core` 里没有 Tauri 类型，sidecar 的启动、监督、崩溃与状态路径都能单测覆盖。
- **`packages/shared` 是跨语言契约的唯一来源。** 类型、Zod schema、IPC 映射、事件名、JSON-RPC 协议、工具命名规则都在这里；`packages/shared/fixtures/ipc/` 下的 JSON 被 Rust（`include_str!`）和 TypeScript 同时解析，这是防止两侧静默漂移的机制——类型只保证编译期一致，fixture 保证运行时一致。本地门禁的第一步就是构建这些契约库（消费方导入它们的 `dist/*.d.ts`），顺序不能颠倒。
- **ToolRegistry 是唯一的执行入口，Permission Engine 是唯一的授权决策入口。** 详见 [执行与授权模型](./execution-model.md#执行与授权模型)。
- **Agent 不直接执行远程操作。** sidecar 通过 `host.tool.execute` 向宿主提出请求，宿主会重新校验目标服务器 ID、环境、工作区归属和文件路径策略，然后才执行；sidecar 自己不连 SSH、不访问 SQLite 与凭据库。
- **Provider 差异被关在 Provider 边界内。** agent loop 只依赖 `LLMProvider` 与统一的 `StreamEvent`，不根据 Provider 身份分支；工具名的点号与双下划线转换也只发生在一个地方。
- **有界性是一条设计约束，不是实现细节。** 帧大小、命令输出、文件读取、日志行数、审计条数、审批等待时长、单次运行的步数与墙钟时间都必须有明确上限，并且上限要写在文档里（见 [安全与数据边界](./security.md#安全与数据边界)）。

### 仓库地图

```text
apps/
  desktop/           Tauri 2 外壳：React 界面 + src-tauri 命令层
    src/             AppShell、features/*、components/*、stores/*、lib/ipc.ts、lib/labels.ts、lib/runtime.ts、lib/markdown/
    src-tauri/       Tauri 命令、AppState、窗口与事件转发
    tests/           UI 逻辑单元测试
  agent/             Node.js Agent sidecar
    src/runtime/     agent loop、运行状态机、提示词、运行时装配
    src/tools/       工具抽象、registry、内置工具
    src/permissions/ 权限引擎与命令风险规则
    src/providers/   三种协议的适配器（OpenAI-compatible / Anthropic / Gemini）
    src/context/     上下文集装（宿主数据源 + 空数据源）
    src/rpc/         JSON-RPC 方法分发与 Provider 装配
    src/transport/   stdio 传输、宿主 RPC 客户端
    src/security/    敏感数据清理
    src/mcp/         MCP 工具适配：目录客户端、命名空间与风险档位
    src/trace/       执行追踪账本
packages/
  shared/            跨层契约：types、schemas、ipc、events、protocol、naming、fixtures
  provider-sdk/      LLMProvider 抽象与 Provider 侧工具名映射
  agent-sdk/         sidecar JSON-RPC 类型化客户端
crates/
  core/              sidecar 启动与监督、宿主 IPC 类型、MCP 客户端与工具目录、Provider 选择与请求头消毒、身份命名规则、Docker 行解析、日志脱敏、采集编排、PTY 服务
  ssh/               russh 连接、认证、known_hosts、命令、PTY、SFTP
  terminal/          多会话 PTY 路由与事件归一
  collector/         7 个采集器与本地/SSH runner
  credentials/       OS 凭据库抽象（keychain 引用）
  database/          SQLite schema、迁移、model（按域拆开）与 repository
  filesystem/        远端文件策略与上限（凭据路径黑名单、有界读写），传输由桌面层注入
  time/              时间戳的唯一实现
docs/
  README.md          文档索引与文档治理规则
  architecture.md    本文：链路与仓库地图
  execution-model.md 授权与票据的共享规则
  risk-tiers/        三档权限文档（read / write / dangerous）
  boundaries/        三个跨模块边界（Provider / MCP / Markdown 渲染）
  security.md        安全与数据边界
  limitations.md     当前限制与有意为之的边界
  development.md     开始开发、验证命令与首次使用引导
  packaging.md       打包与分发
  adr.md             架构决策记录 0001–0015
  changelog.md       版本与发布历史
scripts/             校验、构建辅助、sidecar smoke、打包、桌面窗口检查
```
