# Yukinal

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Status: development](https://img.shields.io/badge/status-development-yellow.svg)](#项目状态)

Yukinal 是一个把「远程开发与基础设施运维」和「AI Agent」放进同一个桌面窗口的工作区。它把 SSH 连接、服务器健康快照、终端、远程文件、服务与日志、活动审计，以及一个可审批的 Agent 面板组织到一起。

它要解决的问题是：当人们想用模型操作自己的服务器时，常见的做法是直接把 shell 交给模型。这样权限不可解释、越权无法追溯、误操作无法预防。Yukinal 把模型放在「提议者」的位置上——模型只能提出工具调用请求，是否执行由 Permission Engine 决策，实际操作由 Rust 宿主在已解析的目标上完成，整个过程落成可回放的活动记录与执行审计。模型不会绕开现有的运维流程，它是在既有的目标、风险规则和用户授权边界内工作。

## 目录

- [项目状态](#项目状态)
- [今天真正可用的能力](#今天真正可用的能力)
- [执行与授权模型](#执行与授权模型)
- [架构总览](#架构总览)
- [安全与数据边界](#安全与数据边界)
- [仓库地图](#仓库地图)
- [开始开发](#开始开发)
- [验证命令](#验证命令)
- [当前限制](#当前限制)
- [文档](#文档)
- [许可证](#许可证)

## 项目状态

Yukinal 目前是一个可以跑起来的开发版本，版本号为 `0.1.0`。这个版本号只有一处来源（`packages/shared/src/version.ts` 的 `APP_VERSION`），其余六个 `package.json`、Cargo workspace、`tauri.conf.json` 和 IPC fixture 都由 `packages/shared/src/version.test.ts` 钉在同一个值上，改一处而漏改其余会让 `pnpm check` 变红；发布历史见 [CHANGELOG](CHANGELOG.md)。桌面端、Rust 宿主和 Node.js Agent sidecar 之间的完整链路已经打通并可验证：从界面发起一次 Agent 运行，经过模型流式输出、工具调用、权限决策、宿主执行，到审计落库和界面反馈，都有真实代码和测试覆盖。

但这不是一个可以分发的产品：

- **没有安装包。** `apps/desktop/src-tauri/tauri.conf.json` 中 `bundle.active` 为 `false`，不会生成安装程序；打包后如何随应用分发受信任的 Node 运行时仍未决定。
- **接口不承诺向后兼容。** 在发布 `1.0` 之前，Tauri IPC 命令、sidecar JSON-RPC 方法和跨层类型都可能变化。
- **多个能力边界是明确未完成的。** 例如 MCP 尚未接入；多模态输入、Provider 原生协议、SSH 证书认证都还没有实现。详见[当前限制](#当前限制)。

## 今天真正可用的能力

以下每一条都能在仓库中找到对应实现；括号里是主要位置。

**桌面工作区（需要 Tauri 窗口）**

- 服务器条目的增删改查，落本地 SQLite；SSH 密码与私钥只进操作系统凭据库（`apps/desktop/src-tauri/src/commands/server.rs`）。
- 连接管理：连接、断开、连接状态与最近错误；同一服务器的连接会被缓存复用（`commands/terminal.rs` 的 `ensure_session`）。
- 概览页的真实健康快照：7 个采集器（OS、CPU、内存、运行时长、磁盘、网络、Docker）各带 5 秒命令超时，采集结果入库并可回看（`crates/collector`）。
- 终端：基于 russh 的 PTY（`xterm-256color`），支持多会话、写入、改尺寸和关闭，数据通过 `terminal:data` 等事件流回界面（`crates/terminal`）。
- 远程文件：SFTP 目录列表与有上限的文本读取（上限 1 MiB，超出部分标记为已截断）（`commands/files.rs`、`crates/ssh`）。覆盖写入只作为 Agent 工具 `filesystem.write` 存在，界面没有写文件的入口。
- 服务与日志：固定的只读探测命令，先试 `systemctl` 再退到 `docker ps`；日志先试 `journalctl` 再退到 `/var/log/syslog`、`/var/log/messages`，最多 120 行并做级别分类；探测不到时明确返回 `unavailable`，不会编造内容（`commands/services.rs`、`commands/logs.rs`）。
- 活动记录：连接、配置变更、Agent 工具执行都会写入 `activities` 表并推 `activity.created` 事件（`commands/activity.rs`、`commands/host.rs`）。
- Agent 对话历史：会话与消息持久化到 `chat_sessions` / `chat_messages`，支持归档与删除（`commands/chat.rs`）。
- Agent 面板：流式文本、工具调用卡片、审批按钮、停止运行、模型选择、运行模式与批准方式切换。

**Agent 运行时（Node.js sidecar）**

- 一次完整的 agent loop：组装上下文 → 调用模型 → 解析工具调用 → 请求授权 → 执行 → 把结果回灌模型进入下一轮。单次运行受 `maxSteps`（默认 25）与墙钟上限（默认 15 分钟）约束（`apps/agent/src/runtime/agent-loop.ts`）。
- 8 个内置工具：`system.echo` 在无宿主时也可用；`server.info`、`docker.ps`、`docker.logs`、`docker.inspect`、`docker.restart`、`filesystem.read`、`filesystem.write` 需要 Rust 宿主在线，实际执行发生在宿主侧（`apps/agent/src/tools/builtin/`）。
- 权限决策：三层风险事实合成一个决策，产出可执行的 ticket（`apps/agent/src/permissions/`）。
- 审批往返：等待用户批准，2 分钟未响应按「已过期」处理并拒绝，不会永久挂起运行。
- 取消：停止一次运行会中止在途的 HTTP 流、工具执行和等待中的审批，并把取消状态如实上报。
- 执行追踪：每次运行有一个 `TraceRecorder` 账本，工具事件携带的 `traceId` / `stepId` 都由它发出，被策略拒绝或被驳回的调用也会把步骤收尾（不会留下永远 `running` 的步骤），完成的运行在结果里带上自己的 `traceId`，Rust 侧写入的审计行因此可以按运行检索（`apps/agent/src/trace/`）。
- 运行模式与批准方式两个正交的轴：`goal`/`plan`/`readonly` 决定这次运行**能改到什么程度**，`ask`/`auto` 决定**允许的部分由谁点头**（`packages/shared/src/types/risk.ts`）。

**Provider**

- 一个 OpenAI-compatible Provider，覆盖 Chat Completions 与 Responses 两种请求方言，支持模型目录、SSE 文本增量、工具调用增量、取消、超时和安全的错误摘要（`apps/agent/src/providers/openai-compatible.ts`）。
- 凭据链路：SQLite 只保存 `credentialRef`，密钥存操作系统凭据库，Rust 在每次运行开始时解析并以一次性参数交给 sidecar，不写配置、不写日志。

**浏览器预览模式**

- 执行 `pnpm --filter @yukinal/desktop dev` 可以在普通浏览器里开发界面。预览模式不提供 SQLite、SSH、Tauri IPC 或 sidecar；调用原生命令会抛出「请在 Yukinal 桌面应用中执行此操作」，界面会显示「预览模式」标记，不会用假数据伪装这些能力（`apps/desktop/src/lib/ipc.ts`）。

## 执行与授权模型

```text
用户请求（React）
   │  agent_run_start：prompt · target · permissionMode · mode
   ▼
Rust 宿主解析 Provider 与凭据（SQLite 行 + OS 凭据库）
   │  agent.run.start：providerConfig 随这一次请求下发
   ▼
组装上下文（ContextEngine：工作区 / 服务器 / 最新快照）
   │
   ▼
调用模型（SSE 流式输出）
   │  文本增量 ────────────────────────────────► 界面 agent.thinking
   ▼
工具调用请求
   │
   ▼
Permission Engine 决策（工具风险 × 命令风险 × 目标环境）
   ├─ deny ─► 拒绝结果回灌模型，不执行
   ├─ ask ──► agent.waiting_approval ──► 用户批准 / 拒绝 / 超时过期
   │                                    └─ approve_session 授予本次会话
   └─ auto ─► 自动执行（记录来源：policy / agent / user）
   │
   ▼
ToolRegistry 校验 ticket（工具名 · 目标 · 决策来源 · 审批 ID）
   │  host.tool.execute（JSON-RPC 请求发给 Rust 宿主）
   ▼
Rust 宿主在已解析的目标上执行受限操作（SSH 命令 / SFTP / Docker）
   │
   ▼
结果回灌模型进入下一轮 · agent.tool_result → SQLite 审计 + 活动记录 + 界面
```

这条链路遵循三项原则：

1. **先说明影响，再执行会改变状态的操作。** 只读工具可以直接执行；写入、重启、部署类操作会暂停并等待明确批准。
2. **每一步都可见。** 模型文本、工具调用、风险事实、决策结果、审批与执行结果都以事件形式流到界面，并被写入审计表；没有「黑箱里已经做完了」的路径。
3. **Permission Engine 是唯一的授权决策入口。** 工具声明、命令分析、目标环境只生产风险**事实**；只有 `PermissionEngine.evaluate()` 能把事实加上用户在运行级给出的委托，变成一个可执行的决策。模型文本永远不能等价于一次授权。

授权结果有四种 ticket 来源，ToolRegistry 会逐一复核，任一字段不匹配就拒绝执行：`policy_auto`（环境策略自动批准）、`agent_auto`（用户在运行级委托 Agent）、`session_auto`（用户在本会话批准过同名同目标的操作）、`user_approved`（与挂起审批完全匹配的逐项批准）。

会话授权的作用域是**单次运行**：授权集合在 agent loop 里没有运行在飞时被清空，所以一次「批准本会话」不会悄悄授权后续无关的运行。它始终绑定具体的工具与目标，并且**永远不覆盖 dangerous 档位**——不只是「危险工具」，也包括被环境升级到该档位的普通写入（生产与未标注环境的风险下限都是 `high`）。那类操作每次都重新询问，必须逐项批准：一次会话级别的同意不能替代逐项确认。

## 架构总览

```text
┌───────────────────────────────────────────────────────────────────┐
│ React 19 + Vite（apps/desktop/src）                                │
│ 服务器 · 概览 · 终端 · 文件 · 日志 · 服务 · 活动 · Agent 面板        │
└──────────────────────────────┬────────────────────────────────────┘
                               │ 白名单 Tauri IPC：命令 + 事件
┌──────────────────────────────▼────────────────────────────────────┐
│ Rust 宿主（apps/desktop/src-tauri + crates/*）                     │
│ commands：服务器 · 终端 · 文件 · 日志 · 服务 · 活动 · Provider · Agent│
│ core：SSH · PTY · 采集 · SQLite · OS 凭据库 · sidecar 启动与监督      │
└───────┬───────────────────────────────────────┬───────────────────┘
        │ SSH / SFTP / PTY                      │ stdio 上的 NDJSON JSON-RPC
        ▼                                       ▼
┌───────────────────┐              ┌─────────────────────────────────┐
│ 远程服务器         │              │ Node.js Agent sidecar           │
│ systemd / Docker   │              │ agent loop · tools · 权限引擎    │
└───────────────────┘              │ openai-compatible Provider       │
                                   └───────────────┬─────────────────┘
                                                   │ HTTPS + SSE
                                                   ▼
                                       ┌───────────────────────────┐
                                       │ OpenAI-compatible 端点     │
                                       └───────────────────────────┘
```

分层的实际约束：

- **React 只用 `packages/shared` 里声明的东西。** `IPC_COMMANDS` 是命令白名单，`EVENT_NAMES` 是事件白名单；不在白名单里的原生能力对界面而言不存在。界面不启动进程、不访问 SSH、不读凭据。
- **Rust 拥有原生资源。** SSH 会话、PTY、SQLite、操作系统凭据库和 sidecar 进程句柄都在 Rust 侧；`apps/desktop/src-tauri` 只做参数编组，真正的逻辑在 `crates/*`。
- **`packages/shared` 是跨语言契约的唯一来源。** 类型、Zod schema、IPC 映射、事件名、sidecar 协议和工具命名规则都在这里；`packages/shared/fixtures/ipc/` 下的 JSON 同时被 Rust 测试（`include_str!`）和 TypeScript 测试解析，任何一侧序列化漂移都会让构建变红。
- **Agent 不直接执行远程操作。** sidecar 通过 `host.tool.execute` 向宿主提出请求，宿主会重新校验目标服务器 ID、环境、工作区归属和文件路径策略，然后才执行。
- **Provider 差异被关在 Provider 边界内。** agent loop 只依赖 `LLMProvider` 与统一的 `StreamEvent`，不根据 Provider 身份分支；工具名的点号与双下划线转换也只发生在一个地方。

## 安全与数据边界

**凭据**

- 服务器密码、SSH 私钥和 Provider API key 只写入操作系统凭据库（macOS Keychain、Windows Credential Manager、Linux Secret Service，经由 `keyring`），SQLite 只保存 `keychain://<service>/<account>` 形式的引用（`crates/credentials`）。
- Provider 的 key 在 Rust 侧于使用点解析，随 `agent.run.start` 的一次性参数交给 sidecar。它不写入 Agent 配置文件、不写入日志、不写入活动记录。Agent 的进程配置只有 `YUKINAL_DATA_DIR`、`YUKINAL_LOG_LEVEL`、`YUKINAL_MAX_RUN_MS`。
- 加密（带口令）的 SSH 私钥暂不支持：添加服务器时会明确报错，SSH 后端也会拒绝，而不是静默失败。
- 自定义请求头只允许非敏感的网关元数据（`Referer`、`Origin`、`User-Agent`、`X-App-Name` 等 9 个名字），且值不能是 `Bearer`/`Basic` 凭据；`Authorization` 在任何情况下都会被凭据库里的 key 覆盖（`commands/provider.rs`）。

**主机指纹**

- 首次成功认证后把主机指纹按 `host:port` 记录到数据目录下的 `known_hosts`；之后指纹不一致即拒绝连接。SSH crate 另外提供了「必须匹配已知指纹」的严格策略，但桌面端当前的连接路径固定使用首次信任模式，界面也还没有「手动核验并钉住指纹」的入口。生产环境应在首次连接前独立核验指纹。

**Agent 能碰什么**

- 宿主工具只接受 `host: "remote"` 且 `serverId` 以 `srv_` 开头的目标，并会核对目标环境与该服务器注册的环境是否一致、工作区是否真的挂在该服务器上；不一致直接拒绝。
- 文件工具的路径必须是绝对路径、不含控制字符，且在宿主侧按三类规则被拒绝（大小写不敏感）：路径中包含 `/.ssh/`、`/.kube/`、`/.aws/`、`/.azure/`、`/.config/gcloud/`、`/proc/`、`/run/secrets/`、`/var/run/secrets/` 之一；文件名为 `shadow`、`gshadow`、`sudoers`、`id_rsa`/`id_dsa`/`id_ecdsa`/`id_ed25519`、`.env` 与除 `.env.example`/`.env.sample`/`.env.template` 之外的 `.env.*`、`credentials`/`credentials.json`/`secrets`/`secrets.json`；或后缀为 `.pem`、`.key`、`.p12`、`.pfx`、`.jks`。这条封锁在宿主侧生效，被攻破的 sidecar 也无法绕过。
- 服务器名、日志内容、命令输出和远端文件正文都当作不可信数据：系统提示词明确要求不要把远端内容当指令，工具输出在回传模型、界面和审计之前会做敏感值清理。
- 外部动作都有上限：单帧 8 MiB、远程命令输出 4 MiB、界面文件读取 1 MiB、Agent 文件读取默认 128 KiB（上限 1 MiB）、文件写入 512 KiB、日志 120 行、服务 200 条、活动与执行审计每次最多 100 条、工具输出摘要 4000 字符、模型文本 20 万字符。
- sidecar 的 stdout 只承载协议帧，日志一律写 stderr；宿主转发 `agent.stream` 前只接受白名单事件类型、校验 `runId` 并把单个负载限制在 1 MB。sidecar 诊断日志在离开进程边界前会清理凭据和多行私钥块。

**审计**

- 每次工具执行都会由宿主写成 `tool_executions` 行，并额外生成一条 `activities` 记录。审计输入按键名脱敏（`apiKey`、`password`、`content` 等），`filesystem.read` 的文件正文不写入审计，输出摘要命中敏感标记时整体替换为「已省略」。
- 自动执行的来源会被如实记录为 `policy`、`agent` 或 `user`：Agent 自主批准不会伪装成用户批准。

## 仓库地图

```text
apps/
  desktop/           Tauri 2 外壳：React 界面 + src-tauri 命令层
    src/             AppShell、features/*、stores/*、lib/ipc.ts、lib/labels.ts、lib/runtime.ts
    src-tauri/       Tauri 命令、AppState、窗口与事件转发
    tests/           UI 逻辑单元测试
  agent/             Node.js Agent sidecar
    src/runtime/     agent loop、运行时装配
    src/tools/       工具抽象、registry、内置工具
    src/permissions/ 权限引擎与命令风险规则
    src/providers/   OpenAI-compatible Provider
    src/context/     上下文集装（宿主数据源 + 空数据源）
    src/rpc/         JSON-RPC 方法分发
    src/transport/   stdio 传输、宿主 RPC 客户端
    src/security/    敏感数据清理
    src/mcp/         未来 MCP 边界的约束说明（无实现）
packages/
  shared/            跨层契约：types、schemas、ipc、events、protocol、naming、fixtures
  provider-sdk/      LLMProvider 抽象与 Provider 侧工具名映射
  agent-sdk/         sidecar JSON-RPC 类型化客户端
crates/
  core/              sidecar 启动与监督、宿主 IPC 类型、采集编排、PTY 服务
  ssh/               russh 连接、known_hosts、命令、PTY、SFTP
  terminal/          多会话 PTY 路由与事件归一
  collector/         7 个采集器与本地/SSH runner
  credentials/       OS 凭据库抽象（keychain 引用）
  database/          SQLite schema、迁移、model 与 repository
  filesystem/        本地/远端文件能力的契约占位（尚无实现，未被引用）
docs/                文档入口与架构决策记录
scripts/             校验、构建辅助、sidecar smoke、桌面窗口检查
```

## 开始开发

前置条件：

- Node.js `>= 24`（`package.json` 的 `engines`）
- pnpm `11.8.0`（`packageManager` 固定，建议用 Corepack 启用）
- Rust `1.85` 或更高版本，工具链 `stable` 并包含 `rustfmt` 与 `clippy`（`rust-toolchain.toml`）
- 目标平台运行 Tauri 2 所需的系统依赖（Linux 需要 webkit2gtk 等，见 `.github/workflows/check.yml`）

安装依赖并跑完整校验：

```bash
pnpm install
pnpm check
```

启动浏览器里的界面预览（不需要 Rust）：

```bash
pnpm desktop:dev
# 等价于 pnpm --filter @yukinal/desktop dev
```

预览地址固定为 `http://127.0.0.1:1420/`（Vite 配置了 `strictPort`）。它适合调界面，但原生能力不可用。

启动完整桌面应用：

```bash
pnpm --filter @yukinal/desktop tauri dev
```

该命令会先构建 Agent sidecar 产物，再启动 Vite，然后打开 Tauri 窗口。窗口启动时 Rust 会自动拉起 sidecar（与 `agent_spawn` 命令走同一条启动路径），因此 Agent 面板应该立即处于可用状态。

单独运行 Agent sidecar（调试协议时使用，stdout 上是 NDJSON 帧）：

```bash
pnpm agent:dev
# 等价于 pnpm --filter @yukinal/agent dev（tsx watch src/index.ts）
```

清理构建产物（不会碰数据库和凭据库）：

```bash
pnpm clean
```

首次使用的顺序：在「设置 ▸ Provider」里填写兼容端点、模型和 API key（本地端点可以留空 key），再到「服务器」里添加一台服务器（需要用户名，以及密码或未加密私钥），连接后即可使用概览、终端、文件、服务与日志。

## 首次使用引导

首次打开会显示三步引导，也可从窗口顶部“使用引导”重新打开：

1. 选择或保存模型配置，点击“测试模型连接”。测试通过已保存的凭据发送一条简短消息，验证实际文本回复；可能产生少量模型费用，超时或失败可重试。
2. 添加或选择服务器，点击“连接并验证 SSH”。连接成功后继续；失败可编辑地址与认证信息后重试。首次连接采用 TOFU，应提前独立核验主机指纹。
3. 点击“填入首次排查任务”，将只读巡检草稿放入 Agent 面板，并设置“操作前询问”。用户检查后发送；已有草稿或正在运行的任务不会被覆盖。

“稍后设置”会记住跳过状态。浏览器预览可查看引导，但不能测试模型或连接 SSH。

## 验证命令

`pnpm check` 是唯一的本地门禁（`scripts/check.mjs`），CI 也是跑同一条命令（`.github/workflows/check.yml`，三个平台各跑一遍）。它按固定顺序执行，遇到必需步骤失败即停止：

1. `node scripts/check-publication.mjs` —— 公开文档卫生（禁止引用未发布的内部材料）
2. `node scripts/check-secrets.mjs` —— 已跟踪文件中不得出现凭据形态的字符串
3. 构建契约库：`@yukinal/shared`、`@yukinal/provider-sdk`、`@yukinal/agent-sdk`
4. `pnpm -r --if-present typecheck` —— 全工作区类型检查
5. 构建 Agent：`pnpm --filter @yukinal/agent build`（`tsc -p tsconfig.build.json`）
6. `pnpm -r --if-present test` —— 所有工作区单元测试
7. 构建桌面端：`pnpm --filter @yukinal/desktop build`（Vite）
8. `node scripts/smoke-sidecar.mjs` —— 用真实 stdio 传输启动 sidecar，断言握手、ping、工具列表、方法可用性、不完整请求返回 `INVALID_PARAMS`、坏帧不会杀死进程、父进程关闭 stdin 后干净退出
9. 若 `cargo` 在 PATH 上：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo check --workspace --all-targets`、`cargo test --workspace -- --test-threads=1`（最后一项会带上 `YUKINAL_TEST_NODE` 与 `YUKINAL_TEST_ENTRY`，让跨语言集成测试启动真实的 sidecar 产物，且要求该产物必须存在）

分层执行（需要缩小范围时用）：

```bash
pnpm typecheck                     # 全工作区类型检查
pnpm test                          # 全工作区单元测试
pnpm build:libs                    # 只构建 packages/**
pnpm smoke:sidecar                 # 只跑 sidecar stdio 冒烟
cargo fmt --all --check            # Rust 格式
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace -- --test-threads=1
```

Windows 上还可以手动验证「窗口真的被创建出来」（需要先构建出 `target/debug/yukinal-desktop.exe`）：

```powershell
pwsh -File scripts/check-desktop-window.ps1
```

## 当前限制

- **没有安装包。** Tauri bundle 关闭；打包方案、随包分发的 Node 运行时、代码签名都还没做。
- **sidecar 崩溃后不会自动重启。** 退出码、信号和最近 200 行 stderr 会保留给界面显示，重启需要用户操作。
- **MCP 未接入。** 协议握手里 `mcp` 能力为 `false`；仓库中只有 MCP 的配置类型和数据库表，没有客户端、没有工具注册、没有界面入口。见 [MCP 边界](apps/agent/src/mcp/README.md)。
- **只有一种 Provider 类型。** Anthropic、Gemini 等原生协议没有实现；接入方式是通过 `baseUrl` 走兼容端点。
- **SSH 认证不完整。** 支持密码和未加密的 OpenSSH 私钥；不支持 ssh-agent、带口令的私钥和证书认证（后者被显式拒绝）。
- **主机指纹没有手动核验入口。** 后端有钉住指纹的能力，但没有暴露成命令或界面操作。
- **终端、远程文件、日志、服务、活动、对话历史和本地数据库都需要 Tauri 桌面应用**，浏览器预览里不可用。
- **危险动作需要逐项批准。** `docker.restart` 声明为 `high` 风险，因此在任何环境下都不会被自动批准，也不会被会话授权记住；会话授权只覆盖非危险操作。
- **`filesystem.write` 是覆盖写**，没有「先读后改」或补丁语义。
- **`delivery` 与 `resume` 字段**目前在协议里被接受，但只用于「同一条消息重试不会开出第二个运行」的去重，不具备完整的同步/恢复语义。
- **`policyId`** 在运行请求里可以被传入，但当前运行路径只使用目标环境推导出的内建策略。
- **`crates/filesystem` 是契约占位**，没有任何实现，也没有被其他 crate 引用。

## 文档

- [文档入口](docs/README.md) —— 文档放在哪里、按什么顺序读、哪些边界不能跨
- [变更日志](CHANGELOG.md) —— 版本号含义、发布历史与破坏性变化
- [架构决策记录](docs/adr/README.md) —— 长期决策与它们的代价
- [Provider 边界](apps/agent/src/providers/README.md) —— Provider 抽象、两种请求方言、模型目录与工具名翻译
- [MCP 边界](apps/agent/src/mcp/README.md) —— 当前实现状态与未来接入必须遵守的约束
- [第三方声明](NOTICE) —— 依赖与随仓库分发的字体许可证

## 许可证

项目原创代码与文档以 [MIT License](LICENSE) 发布。第三方依赖和随仓库分发的字体仍受各自许可证约束，详见 [NOTICE](NOTICE)。
