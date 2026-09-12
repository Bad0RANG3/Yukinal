# Yukinal

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Status: development](https://img.shields.io/badge/status-development-yellow.svg)](#项目状态)

Yukinal 是一个把「远程开发与基础设施运维」和「AI Agent」放进同一个桌面窗口的工作区。它把 SSH 连接、服务器健康快照、终端、远程文件、服务与日志、活动审计，以及一个可审批的 Agent 面板组织到一起。

它要解决的问题是：当人们想用模型操作自己的服务器时，常见的做法是直接把 shell 交给模型。这样权限不可解释、越权无法追溯、误操作无法预防。Yukinal 把模型放在「提议者」的位置上——模型只能提出工具调用请求，是否执行由 Permission Engine 决策，实际操作由 Rust 宿主在已解析的目标上完成，整个过程落成可回放的活动记录与执行审计。模型不会绕开现有的运维流程，它是在既有的目标、风险规则和用户授权边界内工作。

## 目录

- [这份文档是什么](#这份文档是什么)
- [项目状态](#项目状态)
- [今天真正可用的能力](#今天真正可用的能力)
- [执行与授权模型](#执行与授权模型)
- [架构总览](#架构总览)
- [安全与数据边界](#安全与数据边界)
- [仓库地图](#仓库地图)
- [开始开发](#开始开发)
- [首次使用引导](#首次使用引导)
- [验证命令](#验证命令)
- [打包与分发](#打包与分发)
- [边界：模型 Provider](#边界模型-provider)
- [边界：外部工具（MCP）](#边界外部工具mcp)
- [Agent 回复的 Markdown 渲染](#agent-回复的-markdown-渲染)
- [当前限制](#当前限制)
- [有意为之的边界（不是待办）](#有意为之的边界不是待办)
- [架构决策记录（ADR 0001–0015）](#架构决策记录adr-00010015)
- [版本与发布历史](#版本与发布历史)
- [维护这份文档的规则](#维护这份文档的规则)
- [许可证](#许可证)

## 这份文档是什么

**整个仓库只有这一份文档。** 它同时承担四件事，过去由四类文件分别承担：

- **项目说明**：这是什么、现在能做什么、还差什么。功能范围变化时，[当前限制](#当前限制) 一节必须同步更新，它是**全部**已知缺口的唯一来源。
- **架构决策记录**：[架构决策记录（ADR 0001–0015）](#架构决策记录adr-00010015) 一节。这些编号就是代码注释里 `ADR 0004` 这类写法的指向物；仓库里**没有**独立的 `docs/adr/` 目录了，读到 `ADR NNNN` 就翻到那一节。
- **边界说明**：三个跨模块的边界各自成节 —— [模型 Provider](#边界模型-provider)、[外部工具（MCP）](#边界外部工具mcp)、[Markdown 渲染](#agent-回复的-markdown-渲染)。它们的共同点是一句话说不清、且写错了不会立刻报错。
- **发布历史**：[版本与发布历史](#版本与发布历史) 一节取代了过去的变更日志文件。

写进这份文档的内容需要满足至少一条：

- 它约束**两个以上模块**，读者无法只读一份代码就理解全貌（例如「Docker 工具名如何在 Provider 边界变形」）。
- 它是一个**公开契约**，改动会让另一侧编译或运行失败（例如 Tauri IPC 命令表、sidecar 的 JSON-RPC 方法名）。
- 它属于**安全模型**，行为是否正确无法只靠单侧测试判断（例如「谁能生成执行票据」）。
- 它解释**为什么是这个形状**，而这个理由在代码里只能看到结果、看不到权衡。

留在代码里的内容：某个函数、某个常量、某条正则为什么这样写，写在它旁边；一次性的排查结论、临时开关、尚未落地的想法不进文档，那会让读者把「打算做」误读成「已经做」；复述类型的字段清单也不进文档，类型本身就是契约，重复一遍只会产生两份会漂移的真相。

一条硬规则：**文档里写的每一句话都必须能在仓库里找到对应的实现或测试。** 如果某个能力只完成了一半，就写清楚哪一半完成了；不要用「支持 X」这种无法验证的措辞盖过去。

新读者的推荐顺序：先读 [项目状态](#项目状态) 与 [今天真正可用的能力](#今天真正可用的能力)，再读 [执行与授权模型](#执行与授权模型)（这是整个项目的重点），然后按需要进入 [架构总览](#架构总览)、[安全与数据边界](#安全与数据边界) 或三个边界小节；准备改协议或跨层类型时，先读 `packages/shared/src/` 与 `packages/shared/fixtures/ipc/`，它们是契约本身。

## 项目状态

Yukinal 目前是一个可以跑起来的开发版本，版本号为 `0.1.0`。这个版本号只有一处来源（`packages/shared/src/version.ts` 的 `APP_VERSION`），其余六个 `package.json` 与 `tauri.conf.json`、Cargo workspace 和 IPC fixture 都由 `packages/shared/src/version.test.ts` 钉在同一个值上，改一处而漏改其余会让 `pnpm check` 变红。桌面端、Rust 宿主和 Node.js Agent sidecar 之间的完整链路已经打通并可验证：从界面发起一次 Agent 运行，经过模型流式输出、工具调用、权限决策、宿主执行，到审计落库和界面反馈，都有真实代码和测试覆盖。

但这不是一个可以分发的产品：

- **安装包已经在本机构建出来了，但没有签名。** `pnpm package` 在 Windows 上产出了 NSIS 安装程序与 WiX `.msi`（见 [打包与分发](#打包与分发)），两者都在 `target/release/bundle/` 下。未签名意味着 Windows SmartScreen 与 macOS Gatekeeper 会对首次启动发出警告；也没有公证与自动更新。**macOS 与 Linux 的安装包从未构建过**（各自只能在各自平台上打）。
- **接口不承诺向后兼容。** 在发布 `1.0` 之前，Tauri IPC 命令、sidecar JSON-RPC 方法和跨层类型都可能变化。
- **仍然有明确的能力空缺。** 多模态输入没有实现；SSH 证书认证在后端可用但界面不能配置；两套原生 Provider 适配器从未对真实 API 调用过（写它们的环境没有网络）。详见[当前限制](#当前限制)。

## 今天真正可用的能力

以下每一条都能在仓库中找到对应实现；括号里是主要位置。

**桌面工作区（需要 Tauri 窗口）**

- 服务器条目的增删改查，落本地 SQLite；SSH 密码与私钥只进操作系统凭据库（I/O 在 `apps/desktop/src-tauri/src/commands/server/`，命名与挂载规则在 `crates/core/src/identity.rs`）。
- 连接管理：连接、断开、连接状态与最近错误；同一服务器的连接会被缓存复用（`commands/terminal.rs` 的 `ensure_session`）。
- 概览页的真实健康快照：7 个采集器（OS、CPU、内存、运行时长、磁盘、网络、Docker）各带 5 秒命令超时，采集结果入库并可回看（`crates/collector`）。
- 终端：基于 russh 的 PTY（`xterm-256color`），支持多会话、写入、改尺寸和关闭，数据通过 `terminal:data` 等事件流回界面（`crates/terminal`）。
- 远程文件：SFTP 目录列表与有上限的文本读取（上限 1 MiB，超出部分标记为已截断），读取结果带回内容的 SHA-256 摘要（`commands/files.rs`、`crates/filesystem`、`crates/ssh`）。写入只作为 Agent 工具存在（`filesystem.write` 覆盖写、`filesystem.edit` 先读后改），界面没有写文件的入口。
- 服务与日志：固定的只读探测命令，先试 `systemctl` 再退到 `docker ps`；日志先试 `journalctl` 再退到 `/var/log/syslog`、`/var/log/messages`，最多 120 行并做级别分类；探测不到时明确返回 `unavailable`，不会编造内容（`commands/services.rs`、`commands/logs.rs`）。
- 活动记录：连接、配置变更、Agent 工具执行都会写入 `activities` 表并推 `activity.created` 事件（`commands/activity.rs`、`commands/host.rs`）。
- Agent 对话记录：会话与消息持久化到 `chat_sessions` / `chat_messages`，面板里的记录视图按日期分组（今天 / 昨天 / 最近 7 天 / 更早），可搜标题与消息正文、按进行中 / 已归档 / 全部筛选、每页 50 条往下翻、就地重命名，并归档或删除（`commands/chat.rs`、`apps/desktop/src/features/agent/AgentHistoryPane.tsx`）。
- Agent 面板：流式文本、工具调用卡片、审批按钮、停止运行、模型选择、运行模式、批准方式与目标策略切换。Agent 的回复按 Markdown 渲染（标题、列表、代码块、表格、行内代码），解析器是仓库自己的、不注入 HTML，链接与图片因此不可点也不下载（`apps/desktop/src/lib/markdown/`、[Markdown 渲染](#agent-回复的-markdown-渲染)）。窗口里只有「你与 Agent 的对话正文」可以拖动选中，其余界面不参与选择。
- MCP 服务器：配置、启动、停止与删除（命令在 `commands/mcp.rs`，目录与工具名解析在 `crates/core/src/mcp/catalog.rs`，进程归宿主），工具目录由宿主把服务器起起来问出来，再经既有的 `host.mcp.catalog` 交给 sidecar —— 没有第二条执行通道。见 [外部工具（MCP）](#边界外部工具mcp)。

**Agent 运行时（Node.js sidecar）**

- 一次完整的 agent loop：组装上下文 → 调用模型 → 解析工具调用 → 请求授权 → 执行 → 把结果回灌模型进入下一轮。单次运行受 `maxSteps`（默认 25）与墙钟上限（默认 15 分钟）约束（`apps/agent/src/runtime/agent-loop.ts`）。
- 9 个内置工具：`system.echo` 在无宿主时也可用；`server.info`、`docker.ps`、`docker.logs`、`docker.inspect`、`docker.restart`、`filesystem.read`、`filesystem.write`、`filesystem.edit` 需要 Rust 宿主在线，实际执行发生在宿主侧（`apps/agent/src/tools/builtin/`）。
- 权限决策：三层风险事实合成一个决策，产出可执行的 ticket（`apps/agent/src/permissions/`）。
- 审批往返：等待用户批准，2 分钟未响应按「已过期」处理并拒绝，不会永久挂起运行。
- 取消：停止一次运行会中止在途的 HTTP 流、工具执行和等待中的审批，并把取消状态如实上报。
- 执行追踪：每次运行有一个 `TraceRecorder` 账本，工具事件携带的 `traceId` / `stepId` 都由它发出，被策略拒绝或被驳回的调用也会把步骤收尾（不会留下永远 `running` 的步骤），完成的运行在结果里带上自己的 `traceId`，Rust 侧写入的审计行因此可以按运行检索（`apps/agent/src/trace/`）。
- 运行模式与批准方式两个正交的轴：`goal`/`plan`/`readonly` 决定这次运行**能改到什么程度**，`ask`/`auto` 决定**允许的部分由谁点头**（`packages/shared/src/types/risk.ts`）。
- MCP 工具：宿主问出来的外部工具以 `mcp.<服务器>.<工具>` 进入同一个 registry，声明里一律是 `critical`（服务器的自我描述不被采信），所以每个调用都要用户逐项批准；`host.tool.execute` 按 `mcp.` 前缀分流，与 `docker.*` / `filesystem.*` 共用同一条路径和同一套取消令牌。

**Provider**

- 三种协议各一个适配器：**OpenAI-compatible**（Chat Completions 与 Responses 两种请求方言）、**Anthropic Messages**、**Gemini `generateContent`**。三者都支持模型目录、SSE 文本增量、工具调用增量、取消、超时和安全的错误摘要；两个原生适配器还会解析 token 统计与推理增量（`apps/agent/src/providers/`，细节见 [模型 Provider](#边界模型-provider)）。
- 协议选择是配置里的一列（`provider_configs.kind`），`buildProvider()` 是唯一按 Provider 身份分支的地方；`wireApi` 只对 OpenAI-compatible 有意义，其余两种带上它会被拒绝，而不是被忽略。
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

决策由三层事实合成，过程是确定的：工具自身声明的静态风险（`ToolDeclaration.risk`）与命令分析的结果（`analyzeCommand()`，16 条规则，规则如 `rm-rf`、`drop-database`、`curl-pipe-shell`）取较大者作为内在风险；目标环境的风险下限（本机与开发 `low`、预发布 `medium`、生产与未知 `high`）再**只向上抬高**它——读类动作在生产仍保持读类，否则「生产环境只读自动执行」这条策略永远无法成立。合成后的档位折叠为 `read` / `write` / `dangerous` 三档，由目标环境选出一张内建策略表，最后施加不可绕过的约束。

不可绕过的约束有三条：`dangerous` 档位**永远不会自动执行**（即使策略表说 `auto` 也强制改成 `ask`）；只读运行模式（`plan` / `readonly`）先于一切委托，任何非 `read` 档位直接 `deny`；Agent 的自动委托范围是封闭的——`auto` 只覆盖 `write` 档位且目标环境为 `development` 或 `staging` 的调用，高危与 critical、本机、未知和生产目标始终等待用户，任何 `deny` 都不能被运行模式或会话授权重新打开。

授权结果有四种 ticket 来源，ToolRegistry 会逐一复核，任一字段不匹配就拒绝执行：`policy_auto`（环境策略自动批准，来源必须标记 `policy`）、`agent_auto`（用户在运行级委托 Agent，档位必须是 `write` 且环境是开发或预发布）、`session_auto`（用户在本会话批准过同名同目标的操作，来源必须标记 `user`）、`user_approved`（与挂起审批的 ID 完全匹配的逐项批准）。所有 ticket 还须满足工具名与目标四元组（host、serverId、workspaceId、environment）与决策完全一致。任一字段不匹配得到的是 `denied_by_policy` 的工具结果，而不是一条被忽略的日志。

会话授权的作用域是**单次运行**：引擎实例比单次运行长命，所以 loop 在没有运行在飞时调用 `clearGrants()`；没有这一步，「批准本会话」会一直授权到 sidecar 进程退出，跨越多次无关运行。它始终绑定具体的工具与目标，并且**永远不覆盖 dangerous 档位**——不只是「危险工具」，也包括被环境升级到该档位的普通写入（生产与未标注环境的风险下限都是 `high`）。那类操作每次都重新询问，必须逐项批准。

## 架构总览

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
- **ToolRegistry 是唯一的执行入口，Permission Engine 是唯一的授权决策入口。** 详见 [执行与授权模型](#执行与授权模型)。
- **Agent 不直接执行远程操作。** sidecar 通过 `host.tool.execute` 向宿主提出请求，宿主会重新校验目标服务器 ID、环境、工作区归属和文件路径策略，然后才执行；sidecar 自己不连 SSH、不访问 SQLite 与凭据库。
- **Provider 差异被关在 Provider 边界内。** agent loop 只依赖 `LLMProvider` 与统一的 `StreamEvent`，不根据 Provider 身份分支；工具名的点号与双下划线转换也只发生在一个地方。
- **有界性是一条设计约束，不是实现细节。** 帧大小、命令输出、文件读取、日志行数、审计条数、审批等待时长、单次运行的步数与墙钟时间都必须有明确上限，并且上限要写在文档里（见 [安全与数据边界](#安全与数据边界)）。

## 安全与数据边界

**凭据**

- 服务器密码、SSH 私钥、私钥口令和 Provider API key 只写入操作系统凭据库（macOS Keychain、Windows Credential Manager、Linux Secret Service，经由 `keyring`），SQLite 只保存 `keychain://<service>/<account>` 形式的引用（`crates/credentials`）。带口令的私钥把口令存成**第二个**凭据条目（同一个 account 加 `-passphrase` 后缀），连接时才解析。
- Provider 的 key 在 Rust 侧于使用点解析，随 `agent.run.start` 的一次性参数交给 sidecar。它不写入 Agent 配置文件、不写入日志、不写入活动记录。Agent 的进程配置只有 `YUKINAL_DATA_DIR`、`YUKINAL_LOG_LEVEL`、`YUKINAL_MAX_RUN_MS`。
- ssh-agent 是第三种认证方式，它只发送 `{method:"agent"}`，**不带任何秘密**（有一条测试专门钉住「agent 不允许夹带密码」）。口令与 agent 两条路径都没有对真实服务器跑过 —— 本环境既没有 ssh-agent 也没有可连的服务器。
- 自定义请求头只允许非敏感的网关元数据（`Referer`、`Origin`、`User-Agent`、`X-App-Name` 等 9 个名字），且值不能是 `Bearer`/`Basic` 凭据；`Authorization` 在任何情况下都会被凭据库里的 key 覆盖（`crates/core/src/provider.rs` 的 `sanitize_custom_headers`）。
- 审计输入按键名脱敏（`apiKey`、`password`、`content`、`oldString`、`newString` 等），`filesystem.read` 的文件正文不写入审计，输出摘要命中敏感标记时整体替换为「已省略」。sidecar 诊断日志在离开进程边界前会清理凭据和多行私钥块（`crates/core` 的脱敏模块与 `apps/agent/src/security/`）。

**主机指纹**

- 首次成功认证后把主机指纹按 `host:port` 记录到数据目录下的 `known_hosts`（自有格式 `v1:host:port:SHA256:…`，指纹写法与 OpenSSH 一致）；之后指纹不一致即拒绝连接，并且错误里同时给出**已钉住的**与**服务器出示的**两个指纹 —— 两个都看得见，才谈得上判断。
- 服务器编辑页提供状态、探针、信任、遗忘四个动作：探针只报告服务器出示的指纹、不写入任何东西，「信任此指纹」只有在这次会话里**真的探过**之后才可用，而不匹配时界面不提供任何「忽略 / 仍然继续」的出口。pin 按 `host:port` 关联而不是按 server id，所以同一台机器在两个条目下共用一条信任状态。
- 没有变的是**首次连接仍然默认 TOFU**：未知主机会被接受并钉住，所以生产环境应在首次连接前独立核验指纹。SSH crate 另有一条「必须匹配已知指纹」的严格策略（未钉住时会在建立 TCP 之前就拒绝），桌面端的连接路径没有启用它。

**Agent 能碰什么**

- 宿主工具只接受 `host: "remote"` 且 `serverId` 以 `srv_` 开头的目标，并会核对目标环境与该服务器注册的环境是否一致、工作区是否真的挂在该服务器上；不一致直接拒绝。
- 文件工具的路径必须是绝对路径、不含控制字符，且在宿主侧按三类规则被拒绝（大小写不敏感）：路径中包含 `/.ssh/`、`/.kube/`、`/.aws/`、`/.azure/`、`/.config/gcloud/`、`/proc/`、`/run/secrets/`、`/var/run/secrets/` 之一；文件名为 `shadow`、`gshadow`、`sudoers`、`id_rsa`/`id_dsa`/`id_ecdsa`/`id_ed25519`、`.env` 与除 `.env.example`/`.env.sample`/`.env.template` 之外的 `.env.*`、`credentials`/`credentials.json`/`secrets`/`secrets.json`；或后缀为 `.pem`、`.key`、`.p12`、`.pfx`、`.jks`。这条封锁在宿主侧生效，被攻破的 sidecar 也无法绕过。
- 服务器名、日志内容、命令输出和远端文件正文都当作不可信数据：系统提示词明确要求不要把远端内容当指令，工具输出在回传模型、界面和审计之前会做敏感值清理。
- 外部动作都有上限：单帧 8 MiB、远程命令输出 4 MiB、界面文件读取 1 MiB、Agent 文件读取默认 128 KiB（上限 1 MiB）、文件写入 512 KiB、日志 120 行、服务 200 条、活动与执行审计每次最多 100 条、工具输出摘要 4000 字符、模型文本 20 万字符。
- **审计。** 每次工具执行都会由宿主写成 `tool_executions` 行，并额外生成一条 `activities` 记录。自动执行的来源会被如实记录为 `policy`、`agent` 或 `user`：Agent 自主批准不会伪装成用户批准。只有一个**终态**结果可以被落库（`pending`/`running`/`waiting_approval` 会被拒绝），所以审计里不会出现「已结束但还在跑」的行。

## 仓库地图

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
scripts/             校验、构建辅助、sidecar smoke、打包、桌面窗口检查
```

## 开始开发

前置条件：

- Node.js `>= 24`（根 `package.json` 的 `engines`，同时也是 esbuild 的 `--target` 与安装包对用户的要求）
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

首次使用的顺序：在「设置 ▸ Provider」里选协议（OpenAI-compatible / Anthropic / Gemini）、填写端点、模型和 API key（本地端点可以留空 key），再到「服务器」里添加一台服务器（需要用户名，以及密码、私钥（带口令的私钥请一并填口令）或 ssh-agent），首次连接会按指纹策略处理服务器身份，连接后即可使用概览、终端、文件、服务与日志。

## 首次使用引导

首次打开会显示三步引导，也可从窗口顶部「使用引导」重新打开：

1. 选择或保存模型配置，点击「测试模型连接」。测试通过已保存的凭据发送一条简短消息，验证实际文本回复；可能产生少量模型费用，超时或失败可重试。
2. 添加或选择服务器，点击「连接并验证 SSH」。连接成功后继续；失败可编辑地址与认证信息后重试。首次连接采用 TOFU，应提前独立核验主机指纹。
3. 点击「填入首次排查任务」，将只读巡检草稿放入 Agent 面板，并设置「操作前询问」。用户检查后发送；已有草稿或正在运行的任务不会被覆盖。

「稍后设置」会记住跳过状态。浏览器预览可查看引导，但不能测试模型或连接 SSH。

## 验证命令

`pnpm check` 是唯一的本地门禁（`scripts/check.mjs`），CI 也是跑同一条命令（`.github/workflows/check.yml`，三个平台各跑一遍）。它按固定顺序执行，遇到必需步骤失败即停止：

1. `node scripts/check-publication.mjs` —— 公开文档卫生（禁止引用未发布的内部材料；公开标准的编号是例外，见脚本里的 `PUBLIC_STANDARD`）
2. `node scripts/check-secrets.mjs` —— 已跟踪文件中不得出现凭据形态的字符串
3. 构建契约库：`@yukinal/shared`、`@yukinal/provider-sdk`、`@yukinal/agent-sdk`
4. `pnpm -r --if-present typecheck` —— 全工作区类型检查
5. 构建 Agent：先 `tsc -p tsconfig.build.json --noEmit` 类型检查，再用 esbuild 打成**单个**自包含 ESM 文件 `apps/agent/dist/index.js`
6. `node scripts/check-packaging.mjs` —— 打包契约：`bundle.resources` 的目标位置与 `tauri.conf.json`、图标、`beforeBuildCommand` 顺序、esbuild 目标与 `engines.node` 一致，以及「安装后的 agent 目录里恰好两个文件：bundle 与声明 ESM 的 `agent/package.json`」
7. `pnpm -r --if-present test` —— 所有工作区单元测试
8. 构建桌面端：`pnpm --filter @yukinal/desktop build`（Vite）
9. `node scripts/smoke-sidecar.mjs` —— 用真实 stdio 传输启动 sidecar（跑的是构建出来的 bundle，不是 `tsx src/index.ts`），断言握手、ping、工具列表、方法可用性、不完整请求返回 `INVALID_PARAMS`、坏帧不会杀死进程、父进程关闭 stdin 后干净退出
10. `node scripts/smoke-packaged-agent.mjs` —— 把 bundle 按 `bundle.resources` 声明的位置摆进一个临时目录（没有 `node_modules`、没有 `package.json`），再跑一遍同样的冒烟：这是「安装之后到底能不能起来」唯一能在这里验证的部分
11. 若 `cargo` 在 PATH 上：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo check --workspace --all-targets`、`cargo test --workspace -- --test-threads=1`（最后一项会带上 `YUKINAL_TEST_NODE` 与 `YUKINAL_TEST_ENTRY`，让跨语言集成测试启动真实的 sidecar 产物，且要求该产物必须存在）

分层执行（需要缩小范围时用）：

```bash
pnpm typecheck                     # 全工作区类型检查
pnpm test                          # 全工作区单元测试
pnpm build:libs                    # 只构建 packages/**
pnpm smoke:sidecar                 # 只跑 sidecar stdio 冒烟
pnpm package                       # 构建安装包（契约库 → agent bundle → 打包契约 → tauri build）
cargo fmt --all --check            # Rust 格式
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace -- --test-threads=1
```

Windows 上还可以手动验证「窗口真的被创建出来」（需要先构建出 `target/debug/yukinal-desktop.exe`）：

```powershell
pwsh -File scripts/check-desktop-window.ps1
```

## 打包与分发

这一节回答三件事：怎么产出安装包、安装包里到底有什么、以及哪些部分是**故意没做或还没验证**的。把一个没跑过的步骤写成「已支持」，下一个人会在发布当天才发现。

**现状。** `apps/desktop/src-tauri/tauri.conf.json` 的 `bundle.active` 是 `true`，并在 `bundle.resources` 里把 agent 放到 `<resource_dir>/agent/index.js`；agent 由 esbuild 打成单文件，不再靠 `node_modules` 解析 `zod`、`@yukinal/shared`、`@yukinal/provider-sdk`；`scripts/check-packaging.mjs` 在每次门禁里把「配置 ↔ Rust 解析器 ↔ 实际产物」三者钉在一起；`scripts/smoke-packaged-agent.mjs` 把同一个文件放到「没有 `node_modules` 的目录」里再跑一遍协议冒烟。

**在 Windows 上，`pnpm package` 已经真的跑通过**，产出两份未签名安装程序：

```text
target/release/bundle/nsis/Yukinal_0.1.0_x64-setup.exe     （NSIS 安装程序）
target/release/bundle/msi/Yukinal_0.1.0_x64_en-US.msi      （WiX 安装包）
```

这条路径以前一次都没有跑完过，原因不在打包器而在配置：`build.beforeBuildCommand` 里的 `pnpm build:libs` 假定当前目录是仓库根，而 Tauri 执行它时的当前目录是 `apps/desktop`（`beforeBuildCommand` 的运行目录是应用目录，即 `src-tauri` 的上一级），那里没有这个脚本，于是 `tauri build` 在编译任何 Rust 之前就失败。现在它写作 `pnpm -w run build:libs`：`--filter` 与 `-w` 都能从子目录解析到 workspace 根，因此这条命令在哪个目录下执行都成立。这个顺序本身仍由门禁守着。

**本机产出安装包。** 前置条件：Node.js `>= 24`、pnpm `11`（`packageManager` 固定 `11.8.0`）、Rust stable（含 `rustfmt`、`clippy`）；Linux 还需 Tauri 系统依赖，与 `check.yml` 安装的是同一串：`libwebkit2gtk-4.1-dev librsvg2-dev patchelf build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev`；Windows 首次打包需要联网——Tauri CLI 会自行下载 **WiX 3 与 NSIS**（默认放进全局工具缓存，已下载过一次之后离线也能打包）。

```bash
pnpm install --frozen-lockfile
pnpm check                 # 门禁
pnpm run package           # 契约库 -> agent 单文件 -> 打包契约 -> tauri build
```

多余参数会透传给 `tauri build`——只编译不产出安装程序用 `pnpm run package -- --no-bundle`。它**先**跑契约库、agent 单文件与打包契约，**再**调用打包器，因为 `tauri build` 会先把整个 Rust workspace 按 release 编译一遍才去看资源文件（`profile.release` 里 `lto = "thin"`、`codegen-units = 1`），早几秒钟失败比十分钟后失败好得多。**不做跨平台交叉打包**：要哪个平台就在那个平台上跑，macOS 的 `.app`/`.dmg` 只能在 macOS 上打。打包器下载工具链那一步可能失败在网络上，和代码无关。

产物落在 cargo workspace 的 `target/release/bundle/` 下，`pnpm run package` 结束时会把它实际生成的文件连大小一起列出来。目录与平台的对应关系是固定的：Windows 是 `.../msi`、`.../nsis`，macOS 是 `.../macos`、`.../dmg`，Linux 是 `.../deb`、`.../rpm`、`.../appimage`。

**安装包里有什么。**

- 桌面程序本身：Rust 宿主、前端静态资源（由 Tauri 内嵌进可执行文件）、图标和元数据。
- Agent sidecar：`<resource_dir>/agent/index.js` 加上一份一行的 `agent/package.json`（**两份资源文件**）。bundle 本身**是一个文件**，esbuild 把 `zod`、`@yukinal/shared`、`@yukinal/provider-sdk` 以及 agent 自己的全部源码内联进去（当前约 900 KiB），`node:` 开头的内置模块是唯一保留的外部件。
- `<resource_dir>` 由 Tauri 决定：macOS 是 `Yukinal.app/Contents/Resources`，Windows 是可执行文件旁边的 `resources` 目录，Linux 是随包安装的资源目录。
- **为什么必须是一个文件**：安装后的应用没有 `node_modules`，任何一个没被内联的裸模块名都会让 `node <resource>/agent/index.js` 直接以 `ERR_MODULE_NOT_FOUND` 退出。`scripts/smoke-packaged-agent.mjs` 针对的就是这件事。
- **那份一行的 `package.json` 不是装饰**：bundle 是 ESM，而安装后的 `<resource_dir>/agent/` 里没有相邻的 `package.json` 时，Node 靠**语法探测**判断模块系统。实测（Node 26.5.1、空目录）确实能启动，但同一文件加上 `--no-experimental-detect-module` 就会失败；`NODE_OPTIONS` 会被子进程继承，所以用户环境里只要存在那个开关，应用就会以一句 Node 解析错误启动失败，而我们的代码里没有任何地方能解释这件事。显式声明 `{"type": "module"}` 把加载方式变成权威来源。代价是这份文件**只能**有 `type` 一个键——多出 `name`/`exports` 会让 Node 把 `agent/` 当成一个包并按包规则解析——`scripts/check-packaging.mjs` 因此断言 `agent/` 下恰好这两个目的地并检查那份 JSON 的内容。
- **安装包里没有 Node.js**：不捆绑、不内嵌、不下载、不 vendor 任何运行时，用户机器上的 `node` 就是运行时（理由与残留风险见 [ADR 0013](#adr-0013安装包分发随包分发-agent-bundle但使用用户自己的-node)）。安装包里同样没有 `node_modules`、pnpm、源码、测试、构建脚本与任何开发期工具。
- **历史教训**：`apps/agent` 曾经允许 tsc 输出到 `dist`，跑一次 `tsc -p tsconfig.json` 就会把逐文件产物覆盖 esbuild 的单文件 bundle，而「类型检查通过」这句话本身不会提示任何异常，真正变红的是几步之后的打包契约。这条路径**真的发生过两次**（第二次是有人绕过 `package.json` 的脚本直接调用 tsc，所以修在脚本上不够）。现在 `noEmit` 写在 `apps/agent/tsconfig.json` 里，`outDir`/`rootDir` 一并去掉——构建产物只有一个来源，无论 tsc 怎么被调用。

**用户需要准备什么。**

| 需要 | 从哪来 | 缺了会怎样 |
| --- | --- | --- |
| Node.js >= 24（与根 `package.json` 的 `engines.node`、CI 的 `node-version`、esbuild 的 `--target=node24` 是**同一个数**） | 用户自己装，`node`（Windows 上是 `node.exe`）出现在 `PATH` 上 | 表现为「启动 sidecar 失败」 |
| WebView2 运行时（Windows） | Windows 10/11 一般自带；安装程序默认 `downloadBootstrapper`，即安装时联网下载引导程序 | 窗口起不来 |
| webkit2gtk-4.1 / GTK3（Linux） | 见下面的已知缺口 | 窗口起不来 |

**缺 Node 或缺 bundle 时应用怎么报。** Rust 侧的解析顺序（`crates/core/src/sidecar/config.rs`）是：`YUKINAL_AGENT_COMMAND`（可配 `YUKINAL_AGENT_ARGS`，分号分隔）→ `YUKINAL_AGENT_ENTRY`（可配 `YUKINAL_NODE`）→ `<resource_dir>/agent/index.js` → 开发兜底（从当前工作目录向上找 `apps/agent/dist/index.js`）。安装包路径排在开发兜底之前是有意的：安装后的应用没有仓库树可向上走，开发运行没有 staged 资源，两种顺序各自在真正重要的场景里给出正确答案。四条都不成立时错误消息会点名构建步骤：

```text
no agent bundle to launch (searched <它找过的每一个路径>); run `pnpm --filter @yukinal/agent build`
```

`searched` 里包含打包路径，所以用户和排查的人都能看见它到底去哪儿找过 —— 有一条测试专门钉住这一点。**没做到的**：`node` 本身不存在或版本过旧时**没有**版本探测。这是 ADR 0013 的知情决定（每一次启动都多起一个进程做 `node --version`，而且仍然抓不到「装了但太旧」，那种情况表现为 stderr 上的一行解析错误加退出码），代价是缺 Node 时用户看到的是系统那句启动失败，而不是一条指名 `nodejs.org` 与 `YUKINAL_NODE` 的消息。

**运行时会用到的路径与开关。**

| 什么 | 在哪 |
| --- | --- |
| agent bundle | `<resource_dir>/agent/index.js` |
| agent 数据目录 | 桌面启动时把 Tauri 的 `app_data_dir()` 通过 `YUKINAL_DATA_DIR` 给 sidecar，除非环境里已设同名变量。identifier 是 `dev.yukinal.workspace`，于是 Windows 是 `%APPDATA%\dev.yukinal.workspace`，macOS 是 `~/Library/Application Support/dev.yukinal.workspace`，Linux 是 `~/.local/share/dev.yukinal.workspace` |
| 覆盖启动方式（排查用） | `YUKINAL_AGENT_COMMAND`、`YUKINAL_AGENT_ENTRY`、`YUKINAL_NODE`、`YUKINAL_AGENT_TIMEOUT_SECS`（缺省 10 秒）、`YUKINAL_LOG_LEVEL` |
| sidecar 日志 | stdout 只承载协议帧，日志一律走 stderr |

**为什么配置长这样。** `tauri.conf.json` 是严格 JSON，写不了注释，所以理由记在这里：

- **`bundle.resources` 用映射而不是列表**：目标路径是契约（Rust 侧 `packaged_entry()` 解析的就是 `<resources>/agent/index.js`），映射把目标写死，源路径相对 `src-tauri`，所以是 `../../agent/dist/index.js`；列表形式保留源目录结构，装出来会是 `resources/agent/dist/index.js`。
- **`bundle.targets` 显式列出三个平台的产物**（`nsis`/`msi`、`app`/`dmg`、`deb`/`rpm`/`appimage`）：tauri-bundler 会按当前宿主过滤这张表，所以同一份配置在三个平台都对；不写 `"all"` 是因为「某个平台默认多了或少了一种产物」应该是一次看得见的改动。
- **`bundle.icon` 必须列全五项**：打包器没有图标会直接拒绝运行（空数组时 MSI 找不到 `.ico` 会失败，macOS 生成不了 app icon）。Tauri 2 配置 schema 里 `bundle.icon` 的默认值是**空数组**，那五项来自 CLI 内嵌的项目模板而非打包时默认值，所以必须显式写出。图标已随仓库提交（`apps/desktop/src-tauri/icons/`，17 个文件），`scripts/check-packaging.mjs` 每次门禁都复查这五项在列表里而且文件真的存在。
- **`build.beforeBuildCommand` 的顺序是契约库 → agent 单文件 → 前端**：原来只构建前端，`tauri build` 会先编译十分钟 Rust 然后才发现要打包的资源文件根本不存在；而「构建成功、但装进去的 agent 是上一次的」是更糟的静默错误。
- **故意没有签名配置**：没有 `bundle.signingIdentity`、没有 Windows 证书指纹、没有公证、没有 `createUpdaterArtifacts`。本仓库没有证书，写一个不能工作的签名配置只会让构建失败。
- **依赖记录位置**：esbuild 声明在 `apps/agent/package.json` 的 `devDependencies`，因为跑它的脚本就在那个 workspace 里；生命周期脚本白名单在 `pnpm-workspace.yaml` 的 `allowBuilds` 里。

**标记（图标）的来源与重新生成**（只在标记改变时才需要）：源图是 `apps/desktop/design/app-icon.png`（1024×1024），生成脚本是 `scripts/generate-app-icon.ps1`，用 PowerShell 的 `System.Drawing` 画出来，因此**只能在 Windows 上重新生成**（并在仓库根目录下运行）。由源图生成整套图标：

```bash
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/generate-app-icon.ps1
pnpm --filter @yukinal/desktop icon     # 即 tauri icon design/app-icon.png
```

实测：用当前源图重新生成一遍，除 `icon.icns` 之外的 16 个文件与仓库里提交的逐字节相同；`icon.icns` 同一源图连续生成两次的 SHA-256 也不同（文件大小一样），这是 `tauri icon` 组装 ICNS 容器的方式导致的，**不要**为了「顺手重新生成一次」而提交新的 `icon.icns`。

**CI。** `.github/workflows/check.yml` —— 每次 push 到 `main` 与每个 PR，三个操作系统各跑一次 `pnpm check`，**不打包**。`.github/workflows/package.yml` —— 只在 `workflow_dispatch` 和 `v*` 标签上触发；Windows、macOS、Ubuntu 22.04 各跑 `pnpm check` 再跑 `pnpm run package`，把 `target/release/bundle/**` 当作 artifact 上传。打包之所以是单独作业：① 打包不是编译检查，它会下载平台工具链、按 release profile 重编译整个 Rust workspace，每个平台几分钟，失败原因常常是「下载镜像慢」而不是代码错了，而「一条会因为网络超时而红的门禁，最后会被人忽略」；② 打包作业仍然**先跑同一条 `pnpm check`**，而不是复制一份更松的步骤清单——测试不过的树打出来的安装包，比没有安装包更糟；③ 用 `ubuntu-22.04` 而不是门禁用的 `ubuntu-latest`，因为 `.deb`/AppImage 的 glibc 下限等于构建机的 glibc。

**未签名与未验证的部分。**

- 所有产物**未签名**：没有 macOS 签名身份与公证，没有 Windows 代码签名证书。macOS 上首次打开需要右键「打开」，Windows 上 SmartScreen 会提示「未知发布者」。没有更新器，升级靠重新下载安装包。
- **已核对**：`tauri.conf.json` 通过 Tauri CLI 自带的 `config.schema.json` 校验；`bundle.icon` 列的图标都在；`tauri icon` 能重现提交的图标（`icon.icns` 例外见上）；agent 单文件能在没有 `node_modules` 的目录里按协议应答；门禁每次复查资源映射、目标平台、图标与构建顺序；Windows 上的 `pnpm package` 已完整跑通并产出上述两份安装程序。
- **没跑过**：**装完之后应用能否真的拉起 sidecar** —— 安装包产出过，但没有真的安装并启动验证过。macOS 的 `.app`/`.dmg`、Linux 的 `.deb`/`.rpm`/`.AppImage` 连构建都没有在本仓库执行过（本机是 Windows）。
- **`.deb` / `.rpm` 的 `Depends:` 是空的。** 打包器不会自动补 `libwebkit2gtk-4.1-0` / `libgtk-3-0`，依赖由发行版自己满足。包名逐发行版不同，在没装过的环境里无法核实，所以这一项是**记下来**，不是填一个猜的名字——写错了会让安装直接失败，比现在这种「装得上但起不来」更难查。

## 边界：模型 Provider

`apps/agent/src/providers/` 是 Agent 与模型之间的唯一接口层；每个文件是一个协议适配器，都实现 `@yukinal/provider-sdk` 的 `LLMProvider`，把上游协议收敛成 agent loop 需要的两样东西——**模型目录**与**统一的流式事件**。Provider 的职责只有适配上游 API：不做权限判断、不执行宿主操作、不决定工具是否能被调用，也不允许上游协议细节渗进 agent loop。

| 文件 | `id` | 上游协议 |
| --- | --- | --- |
| `openai-compatible.ts` | `openai-compatible` | OpenAI Chat Completions / Responses（`wireApi` 选方言），覆盖 OpenAI、OpenRouter、Ollama、LM Studio、vLLM 与内部网关 |
| `anthropic.ts` | `anthropic` | Anthropic Messages API（`POST /v1/messages`） |
| `gemini.ts` | `gemini` | Gemini `generateContent`（`POST /v1beta/models/{model}:streamGenerateContent`） |

**装配图。**

```text
agent loop
   │  只用 LLMProvider + ChatRequest + StreamEvent
   ▼
buildProvider()（唯一按 kind 分支的地方；kind 是选择器，不是适配器的配置字段）
   ├─ OpenAiCompatibleProvider ─ baseUrl · wireApi · apiKey · customHeaders · timeoutMs
   ├─ AnthropicProvider        ─ baseUrl · apiVersion · apiKey · customHeaders · timeoutMs
   └─ GeminiProvider           ─ baseUrl · apiKey · customHeaders · timeoutMs
   ▼
各自的上游端点
```

`kind`（谁翻译）与 `wireApi`（同一种翻译里的哪套方言）是**正交的两个轴**，不合并成一个扁平枚举——那会让每加一个方言都要动 `kind`（乃至数据库语义）。`wireApi` 只对 `openai-compatible` 有意义，所以另外两种 kind 带着它既不会被发送、也不会被接受：`packages/shared` 的 schema 把这种组合判为**非法输入**，而不是「被忽略的字段」。

**抽象**（`packages/provider-sdk/src/types.ts`）：

```ts
export interface LLMProvider {
  readonly id: string;
  readonly model?: string;
  listModels(): Promise<ModelInfo[]>;
  stream(request: ChatRequest): AsyncIterable<StreamEvent>;
}
```

`ChatRequest` 里有 `model`、`messages`、可选的 `tools`、`temperature`、`maxOutputTokens`，以及两个必须被认真对待的字段：`signal`（取消）与 `timeoutMs`（不许挂死）。`StreamEvent` 是判别联合：`text_delta`、`reasoning_delta`、`tool_call`、`usage`、`done`、`error`。`ProviderError` 携带 `retryable` 与可选状态码，让上层不必解析错误文本。消息在边界内侧使用中立形状 `LlmMessage`，由 Provider 负责转换成上游形状。

**硬规则：agent loop 不根据 Provider 身份分支。** 新增协议的正确做法是新增一个实现，而不是在 loop 里加 `if (provider === ...)`。

**一个 Provider 怎么被装配。** 配置来自 Rust 侧解析的 `RuntimeProviderConfig`，每次运行注入一次：

| 字段 | 含义 |
| --- | --- |
| `kind` | `openai-compatible` / `anthropic` / `gemini`——决定由哪个适配器翻译 |
| `baseUrl` | 基地址。`openai-compatible` 是完整地址（尾部 `/` 会被去掉）；两个原生 kind 填到域名层级，适配器自己接协议路径 |
| `model` | 模型 ID |
| `apiKey` | 可选。本地端点（Ollama 等）可以没有 |
| `customHeaders` | 可选。仅限非敏感的网关元数据 |
| `timeoutMs` | 可选。一次运行为 120 秒，模型目录请求为 30 秒，缺省 60 秒 |
| `wireApi` | `chat`（默认）或 `responses`。**只在 `kind: "openai-compatible"` 时允许出现** |

`anthropic` 与 `gemini` 在行里没有（或只有空白）base URL 时回落到协议自己的公开端点（`https://api.anthropic.com`、`https://generativelanguage.googleapis.com`），因为协议本身就是那家服务商的。`openai-compatible` **没有**这种回落——那个 kind 覆盖的是我们不拥有的端点，「替用户编一个 OpenAI 形状的默认地址等于把密钥送去一个他没选过的服务商」。解析与构造发生在 `apps/agent/src/rpc/router.ts` 的 `buildProvider()`：`agent.run.start` 与 `provider.models` 都要求携带这个配置，缺失、`kind` 不在契约里、或某个 kind 还没有适配器时立刻返回 `INVALID_PARAMS`——**这是构造失败，不是运行失败**。

**请求头处理顺序**（三个适配器同一条规则）：先合并 `customHeaders`；如果存在 API key，则**先删除所有大小写形式的既有凭据头**，再写入自己的那一条（`Bearer <key>` / `x-api-key` / `x-goog-api-key`）；凭据库里的 key 永远是权威来源，自定义头不能覆盖它。`anthropic` 另外无条件删掉自定义头里的 `anthropic-version`，因为版本头归适配器所有。

**两种请求方言（只有 `openai-compatible` 有方言）。**

- `chat`（默认）：`POST {baseUrl}/chat/completions`，body 为 `{model, messages, tools, stream: true, temperature, max_tokens}`，`temperature` 缺省 0。文本取 `choices[0].delta.content`；工具调用按 `delta.tool_calls[].index` 建槽位，`id`、`function.name`、`function.arguments` 都是**分片到达**的，因此名称与参数片段是累加拼接出来的，`[DONE]` 时把每个非空槽位一次性产出；参数是拼完再 `JSON.parse`，解析失败时**退化为 `{raw: <原文本>}` 而不是丢弃这次调用**；流在没有 `[DONE]` 的情况下结束（连接被中途关闭）时，已累积的工具调用仍会被放出。
- `responses`：`POST {baseUrl}/responses`，`input` 是消息数组转换后的结果（`tool` 角色转成 `function_call_output` 项，带工具调用的 assistant 消息转成 `output_text` 项加 `function_call` 项），`tools` 展开成 `{type: "function", name, description, parameters}`。处理 `response.output_text.delta`、`response.output_item.added`、`response.function_call_arguments.delta`；`response.failed` 与 `response.incomplete` 产出 `error` 事件。

**模型目录。**

| kind | 端点 | 过滤 |
| --- | --- | --- |
| `openai-compatible` | `GET {baseUrl}/models` | 无 |
| `anthropic` | `GET {baseUrl}/v1/models` | 无（`display_name` 作为显示名） |
| `gemini` | `GET {baseUrl}/v1beta/models` | 只保留 `supportedGenerationMethods` 含 `generateContent` 的项，并去掉名字里的 `models/` 前缀 |

`openai-compatible` 用严格 Zod schema 校验响应（`data` 为最多 1000 个 `{id}` 的数组；**`data` 缺失视为空目录**，因为部分网关就是这样回答的）；校验失败或 HTTP 状态异常时抛 `ProviderError`、不返回半截数据。目录与方言无关。设置界面通过 Tauri 命令 `provider_models` 触发一次读取，传给 Provider 的超时是 30 秒、宿主给这次 RPC 的期限是 35 秒（让 Provider 自己的超时先触发并返回可读错误）；失败时界面**不重试**，用户仍可手动填写模型 ID。返回的每个条目都填成 `{id, label, supportsToolCalling: true, supportsStreaming: true}`——端点不会告诉我们这些能力，因此这是**乐观默认值**，不是能力探测结果。

**工具名如何跨过边界。** Provider 只会看到双下划线形式的名字（例如 `docker__ps`）。转换由 `packages/provider-sdk/src/name-index.ts` 的 `createProviderNameIndex()` 完成，它接收 `ToolDeclaration[]` 并返回 `specs()`（按注册顺序生成 Provider 侧声明）、`providerFor(internalName)`（未知工具直接抛错）、`internalFor(providerName)`（未声明过的名字返回 `undefined`）。构建索引时检查两个内部名是否映射到同一个 Provider 名，命中即抛错；`system.describe` 也会报告这类冲突，而宿主在握手阶段拒绝启动存在冲突的 sidecar。因此 Provider 实现里**不应该**出现任何点号与双下划线之间的转换代码。

**取消、超时与错误。** `ChatRequest.signal` 接到内部 `AbortController` 并传给 `fetch`，用户按停止会真正中断在途请求；超时由独立定时器触发，到期即中止。用户主动取消产出 `done` 且 `finishReason` 为 `cancelled`，超时则以 `error` 事件的形式浮出——调用方需要区分这两者。上游错误文本统一经 `safeProviderMessage()`（把 `api key`、`authorization`、`bearer`、`token` 形式的赋值替换为 `[redacted]`，截断到 300 字符）。这条覆盖「Provider 自己构造的错误」：目录读取失败、`response.failed`/`response.incomplete`、Anthropic 的 `error` 事件、Gemini 的 `error`/`promptFeedback.blockReason`。**唯一没有走这条的是 `openai-compatible` 在 chat 方言下抛出的网络层异常**（catch 分支直接透传 `error.message`）——它只可能是 fetch 自身的文本，但按本条规则它也该过一遍清理。

**Anthropic（Messages API）。** 头为 `x-api-key` 与 `anthropic-version`（缺省 `2023-06-01`，可用 `config.apiVersion` 覆盖）。系统提示是**顶层 `system` 字段**（Messages API 的 `messages` 数组里没有 `system` 角色）。工具流量是内容块：assistant 的 `toolCalls` 变成 `tool_use` 块；`tool` 角色消息变成 user 轮里的 `tool_result` 块并用 `tool_use_id` 指回调用，连续的 `tool_result` 合并进同一轮。`tools` 用 `{name, description, input_schema}` 形状，面向模型的名字原样传递。**`max_tokens` 是必填字段**（省略直接 400），缺省取 4096：这一代所有 Claude 模型都接受它，更大的值在旧模型上会被拒；宁可让回复被截断成 `length` 让上层看见，也不要让请求失败。工具参数以 `input_json_delta.partial_json` 的字符串分片到达，按块下标累加后再解析；没有 `message_stop` 的截断流仍会放出已累积的工具调用。终止原因：`tool_use` → `tool_calls`，`max_tokens` → `length`，`end_turn`/`stop_sequence` → `stop`。目录 `GET {baseUrl}/v1/models`，**顶层用严格 schema**（多出未知字段说明目录格式漂移，此时抛错让界面回退到手工填写），**条目级别不严格**（模型对象正是随 API 增删字段的地方）。

**Gemini（`generateContent`）。** 密钥走 `x-goog-api-key` 头而**不是** `?key=` 查询参数（查询串会进日志）。系统提示走 `systemInstruction`，工具走 `tools[].functionDeclarations`。`functionCall` 一次到达就是完整的 `{name, args}`、没有分片拼接。**Gemini 不返回工具调用 id，只有函数名**，所以适配器用每流递增的 `gemini_call_<n>` 合成 id，并把这个不对称写在代码里：回灌结果实际是按**函数名**匹配的，因此同一回合里同名函数被调用两次是协议层面的歧义。思考模型的 part 带 `thought: true`，映射到中立的 `reasoning_delta` 而不是 `text_delta`（否则推理摘要会混进回答里）。`promptFeedback.blockReason`（没有候选的拒答）产出**不可重试**的 `error` 事件，而不是被当成传输失败。

**哪些事件会产出、哪些不会。**

| 事件 | `openai-compatible` | `anthropic` | `gemini` |
| --- | --- | --- | --- |
| `text_delta` | 是 | 是 | 是 |
| `reasoning_delta` | 否，不解析推理增量 | 是，`thinking_delta` | 是，`thought: true` 的 part |
| `tool_call` | 是 | 是 | 是 |
| `usage` | 否，不解析 token 统计 | 是，`message_delta.usage` | 是，`usageMetadata` |
| `done` | 是 | 是 | 是 |
| `error` | 是 | 是 | 是 |

消费方必须容忍任何一个变体缺席——**今天的 agent loop 就丢掉了 `reasoning_delta` 与 `usage`**，所以「适配器产出了它」不等于「界面上能看到它」。

**明确未验证的假设。** 两个原生适配器都是按公开协议形状实现的，但**没有对真实上游跑过**——写它们的环境没有网络，测试注入的是假的 `fetch` 与构造好的 SSE 流。因此测试证明的是**翻译逻辑**（请求体形状、事件解析、分片拼接、终止映射、取消与脱敏），**不是协议保真度**。适配器里每一处无法离线核实的取值都用注释标成了假设（例如 `anthropic-version` 的缺省日期、`max_tokens` 的缺省值）。**第一次接真实端点时应当先跑一轮 `provider.test`。**

**怎么新增一个 Provider。** ① 只实现接口：新建实现 `LLMProvider` 的类，`stream()` 用异步生成器产出 `StreamEvent`，不要引入 Provider 专属返回类型给上层；② 把 `kind` 加进契约：`packages/shared/src/types/provider.ts` 的 `AI_PROVIDER_KINDS` 与 `packages/shared/src/schemas/provider.ts` 的 `AiProviderKindSchema`，Rust 侧 `crates/database/src/models/` 的 `AiProviderKind`，两端拼写必须逐字一致；③ 装配：在 `buildProvider()` 里按 `kind` 构造，并在 `commands/provider.rs` 的 `runtime_provider_config()` 里产出对应协议需要的字段；④ 数据库两向都走 `kind`：写入用 `kind.as_str()`，读取用 `From_db_column()`，**认不出的取值是硬错误而不是被当成 `openai-compatible`**（那正是「一个用错协议去调的 Provider」）；⑤ 补齐界面与保存路径——kind 的显示顺序、默认端点与「哪些字段有意义」的判断在 `apps/desktop/src/lib/providers.ts`，「只写适配器等于没做」；⑥ 写测试且不依赖真实网络；⑦ **不要改变这些语义**：权限模型、ToolRegistry 的票据校验、宿主工具的输入输出形状、以及工具名的映射规则。

**安全的凭证边界。** Agent 每次运行只接收一份 `RuntimeProviderConfig`；API key 是唯一允许的认证材料，必须来自操作系统凭据库，不得出现在 Provider 日志、活动记录或审计里。`customHeaders` 只允许非敏感的网关元数据，值必须非空、不超过 4096 字符、不含换行，也不能以 `Bearer ` 或 `Basic ` 开头。**当前 `provider_save` 实际上不写入自定义头**（落库时 `custom_headers` 恒为 `None`），这层允许列表用于约束历史数据与导入来源。**没有凭据引用的 Provider 只有在 `baseUrl` 指向本机时才被视为可用**（`localhost`、`127.0.0.1`、`[::1]`）；其他地址缺少密钥时会在**运行前**被判为不可用，而不是等请求失败。

## 边界：外部工具（MCP）

**MCP 已接入，而边界与最初写下的约束完全一致。** `apps/agent/src/mcp/` 里有代码，而且只有两类：把宿主交来的目录变成一个 `Tool`（`tool.ts`），以及把它注册进 `ToolRegistry`（`catalog.ts`）。**这个目录里没有任何进程、没有任何 MCP 协议实现**——进程、`initialize` 握手、`tools/list`、`tools/call`、超时、退出记录、stderr 尾部都在 `crates/core/src/mcp/`，配置命令在 `apps/desktop/src-tauri/src/commands/mcp.rs`，交给 sidecar 的目录与工具名解析在 `crates/core/src/mcp/catalog.rs`。

**接入形状（九条决定）。**

1. **进程、配置、目录、执行全部由宿主负责，agent 只做注册与调用。** 新命令 `mcp_server_list`/`mcp_server_save`/`mcp_server_delete`/`mcp_server_start`/`mcp_server_stop` 读写 `mcp_servers` 表并驱动 supervisor，其中 **`mcp_server_list` 不启动任何进程**——看一眼列表不该派生第三方程序。目录通过既有宿主 RPC 通道新增方法 `host.mcp.catalog` 交给 sidecar，**没有第二条执行通道**；`host.tool.execute` 里 `mcp.` 前缀的工具名由 `commands/host.rs` 分流，与 `docker.*`/`filesystem.*` 走同一条路、同一套取消令牌。
2. **目录是唯一的「读操作带副作用」。** MCP 没有静态工具表——想知道一个服务器有哪些工具，唯一的办法是把它的进程起起来问它。边界写在三个条件里：只碰 `enabled` 且传输为 stdio 的行；只启动 supervisor **从未管过**的 id；总预算 4 秒，超预算的服务器这一次不出现、报告为 `timeout`。
3. **崩掉的 MCP 服务器永不自动重启。** 已跟踪但已死的句柄只被**报告**（退出码 + `not restarted` + 「显式启动」的下一步），三个方向都不例外：目录不拉起它、`mcp_server_start` 是唯一重启路径、一次失败的调用给出 `transport` + `retryable: false` 与 `detail.restarted: false`。理由与 sidecar 的有界自动恢复同源而结论相反：重启一个第三方工具服务器可能把上一次的副作用再执行一遍，而宿主无法判断那是否安全。
4. **`http` 传输在类型层面就不存在。** 配置类型拒绝它；保存一个 `http` 行会被拒绝且**不写库**；表里已存在的 `http` 行在列表、目录与启动路径上都会得到同一句完整理由（出站网络策略不存在），界面上「启动」是禁用的；**没有任何静默 no-op 的路径**。摆一个保存时才失败的传输方式等于用下拉框骗人。
5. **MCP 工具一律声明 `critical`，并且不采信服务器的自我描述。** 描述符里根本没有风险字段，适配器也不读：MCP 的工具注解（`readOnlyHint` 之类）是**服务器对自己的一面之词**，让它降低自己的风险档位就是把授权决策交给被授权方。`critical` → 档位 `dangerous` → 权限引擎在**任何**批准方式下都要求用户逐项批准，会话授权拒绝记住它，只读与计划模式直接拒绝。
6. **远端声明是文档，不是契约。** 本地输入 schema 是「一个对象，内容由服务器校验」；服务器的 `inputSchema` 被追加到**描述**里（有长度上限），因为拿模型或服务器能影响的文档去校验模型输入，是一个带额外步骤的验证漏洞。描述文本一律视为不可信数据：可以展示，不能执行，不能当作校验依据。
7. **来源可分辨且双向钉住。** `Tool` 新增可选的 `origin`，registry 用它填工具的来源声明，并强制两条不变量：`mcp.` 前缀的工具必须声明来源为 mcp，声明来源为 mcp 的工具必须落在 `mcp.` 命名空间里——外部来源无法冒充内置工具。`agent.tool_call`/`agent.tool_result` 事件因此能回答「这次调用是内置工具还是五分钟前某个第三方服务器声明的工具」。
8. **`capabilities.mcp` 只在真的有目录时为真。** agent 在 stdio RPC 起来**之后**异步取一次目录（宿主只有在握手完成后才会转发 sidecar 请求），拿不到就记一行日志并继续、**不重试**；该标志取值必须来自注册表实际内容而不是意图。
9. **`allowedTools` 与 `trustLevel` 仍然只被存储。** 保存它们的界面不存在，保存命令保留原值。注册一个工具**不是**一次授信——每个 MCP 调用都要逐项批准，所以「注册了」与「被信任」已经是两件事。

**运行期需要知道的事实。**

- **取消不撤回副作用。** 取消让宿主不再等待，但 MCP 线上协议只有三个方法，没有「取消一次 `tools/call`」，所以服务进程那边的调用可能继续跑完。这是如实记录的缺陷，不是被忽略的细节。
- **宿主退出时显式关闭 MCP 服务器。** 退出路径与 sidecar 一样调用 `shutdown_all()`，一台服务器关不掉也不影响其余几台——`kill_on_drop` 是一个 `Drop` 实现，在 Windows 上强杀宿主时不会执行，而 MCP 服务器是我们自己派生出来的第三方进程。任务管理器强杀仍会留下子进程，这一点没有消除。
- **结构化归属没有落库。** 事件上的 `origin` 是真的，但落库时没读它，审计行里只有工具名这一条线索。
- **服务器发起的请求不会被应答。** 我们声明零能力，所以 `notifications/tools/list_changed` 只被记成一条诊断，目录不会因此刷新；`sampling` / `roots` 这类请求不会被回答。

**十条接入约束逐条对照。**

| 约束 | 满足方式 |
| --- | --- |
| 外部工具必须先变成 Yukinal 的工具声明 | 适配器只产出 `Tool`，此后与内置工具走同一条路径：权限、票据、超时竞赛、取消、trace、审计都无特殊待遇 |
| 命名空间与名称冲突 | 强制 `mcp.` 前缀与来源声明互相匹配；段在宿主侧规范化，两条 id 撞车时只让一个进目录；Provider 侧名称冲突在注册期拒绝 |
| 风险等级由本地决定 | 一律 `critical`，不读远端注解；档位不是「默认 `medium`」而是最严的那一档，见下面的取舍说明 |
| 输入、输出与超时由本地强制 | 本地 schema 只校验「是一个对象」；远端 `inputSchema` 只作文档；输出摘要 4000 字符上限；每次调用有超时（工具侧 45 秒比宿主侧 30 秒长，好让先放弃的是宿主），且 `retry.maxAttempts` 为 1 |
| 目标必须在本地解析 | `ToolTarget` 由调用侧给出，远端文本不参与解析。`mcp.` 分流发生在目标校验之前，因为一次 MCP 调用打给的是本机派生的进程、不是某台 SSH 服务器 |
| 描述文本一律视为不可信数据 | 只搬运与展示，不执行、不校验、不拼进系统提示词 |
| 进程生命周期仍然归 Rust | 只有 `crates/core/src/mcp/` 派生进程，agent 侧一行 `spawn` 都没有；按 id 去重，所以一个服务器只有一个进程；`http` 在类型层面被拒绝，因为出站网络策略还不存在 |
| 不要暗示已经可用 | 能力报告来自注册表实际内容，不是意图；没有目录就不显示工具数量 |
| 数据库变更走新迁移 | 本次没有改表结构，所以没有新迁移；数据库侧新增的只有 `McpServersRepository::get()` |
| 新增界面要补齐契约 | 两侧都补齐了：五个命令与仓库方法、Zod schema、`IPC_COMMANDS` / `IPC_SCHEMAS` 的五条、五份 fixture，以及 `apps/desktop/src/lib/mcp.ts` 与 `McpSettings.tsx` |

**一处与最初约束不同的地方，是有意收紧的。** 约束里写的是「外部工具默认至少 `medium`」，实际做成了 `critical`；
约束里还说 `trustLevel` 为 `unreviewed` 的工具「不应被自动注册」，而它们**确实被注册了**。两者并不矛盾：
注册一个工具只让它出现在模型可用的列表里，而**每一次调用都要用户逐项批准**——「不自动信任」在这套模型里由权限引擎
保证，而不是由「不注册」保证。后者在没有评审流程的今天等于让 MCP 完全不可用（`allowedTools` 永远是空表、
`trustLevel` 永远是 `unreviewed`，没有任何界面会改它们）。这是显式的取舍：**将来若出现「用户看过描述并降低某个工具档位」
的流程，这里就是它该接入的地方**；在那之前，任何 MCP 工具都不该有一个比 `critical` 更低的值。

**未验证的部分。** MCP 只在一个自带的 Node fixture 上验证过（`crates/core/tests/fixtures/mcp-server.js`，9 种模式）。**真实的第三方 MCP 服务器没有被跑过**，本环境没有网络也没有 `npx`。另外 `crates/core/src/mcp/` 里那条「与 fixture 的行为一致」的集成测试计数在记录写下时就已经不准（当时写 17，实际是 18），所以它只证明与 fixture 一致，不证明任何真实服务器的兼容性。

## Agent 回复的 Markdown 渲染

Agent 面板过去把模型回复当纯文本铺在动态里（`# 结论`、`- 一条`、`| 列 |` 都是原样显示的字符）。把它渲染成 Markdown 意味着要在渲染进程里处理**不可信文本**（模型输出可含任意内容，且会被 `filesystem.read` 之类的工具结果间接影响）；常见做法是 Markdown 库加 `dangerouslySetInnerHTML` 加净化库，那是「**默认放过、列举禁止**」：漏掉一条规则就是一个洞，而每加一个语法特性都要重新审视那张表。窗口侧还有一条硬约束：桌面端的能力清单只给了 `core:default`，没有 opener/shell 能力，因此 `<a href="https://…">` 被点击时**不会**交给系统浏览器，而是让 webview 自己导航过去——点一下链接，整个工作区就没了。

**决定：Markdown 由仓库自己的解析器处理，并且只产出数据；渲染只创建我们自己写的元素。** 解析器在 `apps/desktop/src/lib/markdown/`（`markdown.ts` 是一个转发入口，实现按行内与块级拆成 `inline.ts` / `block.ts`；纯函数，输入文本、输出块与行内结构，不依赖 React、不依赖 DOM，因此可单独测试）；渲染在 `apps/desktop/src/components/MarkdownText.tsx`。

- **没有任何 `dangerouslySetInnerHTML`** —— 正文里的 HTML 是普通文字（`<script>` 显示成 `<script>`），因为它永远不会被当成标记解析，也就没有一张净化规则表需要维护对错。
- **不生成可导航链接** —— 链接渲染成带样式的文字、地址放在 `title` 里；只有 `http`/`https`/`mailto`/页内锚点这几种 scheme 会被认成链接，其余（`javascript:`、`data:`、`file:`）整段当普通文本。
- **图片不下载** —— `![alt](url)` 只显示替代文字与地址；渲染一条消息不该替用户向陌生主机发请求。
- **支持的是子集，而且刻意选过**：ATX 标题、`-`/`1.` 列表（可嵌套、可勾选）、围栏代码块（``` 与 `~~~`）、引用、表格、分隔线、段落，行内代码/粗体/斜体/删除线/链接/裸 URL。**未闭合的围栏按「流式到这里为止」处理**，因为流式输出里它一定闭不上。**不支持** HTML、setext 标题（`---` 下划线）与缩进代码块——后两者不是懒：`---` 同时是分隔线、列表内容同样缩进，猜错会把整段正文变成标题或代码。
- **关键词着色在正文里全量保留**：Markdown 只负责结构，什么词是错误、路径还是标识符仍由原来的 token 规则决定，与日志页和工具卡片是同一套判断。
- **选中策略同时反转**：只有「你与 Agent 的对话正文」可以拖动选中，界面其余部分都不参与选择。做法是在 `.app-shell` 上关掉 `user-select`、只在对话正文上打开——`user-select` 是继承属性，不给每个容器补一条 `none` 才不会有「下一个加的元素忘了写」这种洞。

**代价**：我们自己维护这个解析器，只认对话正文需要的那一小撮语法，所以 CommonMark 的边角行为不承诺完全一致（列表的紧凑/宽松判定、lazy continuation 等）；未支持的语法按纯文本显示，退化方式是「不好看」而不是「内容消失」，解析器测试专门钉住「没认出来的东西一个字都不能丢」。

## 当前限制

这一节是**全部**已知缺口的唯一来源：每条都写明现在做不到什么、以及为什么（其中一部分是环境限制，比如「从未对真实 API 调用过」，那不是代码问题而是这台机器没有网络）。已经做完的部分见 [版本与发布历史](#版本与发布历史)；有意为之、不会被「补完」的安全边界在下一节。

- **安装包只在 Windows 上构建过。** `pnpm package` 在本机产出了 NSIS 安装程序与 WiX `.msi`（见 [打包与分发](#打包与分发)），但**没有真的安装并启动验证过**；macOS 的 `.app`/`.dmg` 与 Linux 的 `.deb`/`.rpm`/`.AppImage` 连构建都没有执行过。所以这件事的状态是「配置被复核过、Windows 路径跑通并产出成品」，不是「三个平台都验证过」。
- **没有代码签名、公证与自动更新。** 未签名的 Windows/macOS 包会触发系统自己的警告；没有更新通道，升级靠用户自己重新下载。
- **需要用户自己准备 Node.js ≥ 24。** 安装包不内含、不下载、不缓存任何运行时；缺 Node 时给出的是「启动 sidecar 失败」，而不是一条指名 `nodejs.org` 与 `YUKINAL_NODE` 的错误——版本探测是 ADR 0013 明确决定不做的事，代价如上。
- **`.deb` / `.rpm` 的 `Depends:` 是空的。** 打包器不会自动补 `libwebkit2gtk-4.1-0` / `libgtk-3-0`，依赖由发行版自己满足。包名逐发行版不同，在没装过的环境里无法核实，所以这一项是**记下来**，不是填一个猜的名字。
- **MCP 只支持 stdio 传输。** `http` 在类型层面就不存在，界面里也没有这个选项。
- **崩掉的 MCP 服务器不会被自动重启。** 唯一的重启路径是显式的启动命令；目录只会报告它已经死了。理由见 [外部工具（MCP）](#边界外部工具mcp)。
- **MCP 工具一律按 `critical` 处理。** 服务器自己的风险注解不被采信，所以每个 MCP 工具在任何运行模式下都要逐项批准，会话授权也不能记住它。代价很直接：一个只读的外部工具也要点一次。
- **MCP 的 `trustLevel` 与 `allowedTools` 目前只被存储。** 还不存在「让用户看过工具描述再决定」的流程，所以 `trustLevel` 永远停在 `unreviewed`、`allowedTools` 永远是空表 —— 这正是每个 MCP 工具都保持 `critical` 的原因。
- **取消 MCP 调用不撤回它的副作用。** 取消让宿主不再等待，但 MCP 线上协议没有「取消一次 `tools/call`」，服务进程那边的调用可能继续跑完。
- **MCP 只在一个自带的 Node fixture 上验证过。** 真实的第三方 MCP 服务器没有被跑过，本环境没有网络也没有 `npx`。
- **55 份 IPC fixture 里有 27 份只有 TypeScript 一侧解析。** 只有 28 份被 Rust 用 `include_str!` 编译进来、并和新序列化的值比一次；剩下 27 份（`provider_*` 里除 `provider_delete` 之外的几份、`server_list` / `server_add` / `server_update` / `server_connect` / `server_disconnect` / `server_delete` / `server_snapshot`、`terminal_*`、`remote_file_*`、`agent_approval_respond`、`agent_run_stop` 与 MCP 那五份）没有任何 Rust 断言钉住 —— Rust 侧改了字段名，这个仓库里不会有任何检查变红。MCP 是其中之一，不是唯一的例外。
- **两套原生适配器从未对真实 API 调用过。** 翻译逻辑、流式状态、取消与错误路径都是照协议文档写的、用假响应测的 —— 写它们的环境没有网络。每个适配器的假设列在 [模型 Provider](#边界模型-provider) 里。
- **`openai-compatible` 在 chat 方言下的网络层异常没有过脱敏。** 其余所有 Provider 错误路径都经过 `safeProviderMessage()`，这一个是特例。
- **Anthropic 的 `anthropic-version` 不能配置。** 运行配置里没有 `apiVersion` 字段，Rust 因此无法传一个进来，适配器用它自己的默认值；自定义请求头同样没有入口。
- **SSH 证书认证不能在界面里配置。** `crates/ssh` 支持它（证书按 OpenSSH 的 `<私钥>-cert.pub` 约定定位，并且必须真的认证所提供的那把私钥），也有测试；但桌面只映射密码、私钥（含口令）与 ssh-agent，遇到证书会明确报「不支持的认证方式」而不是挑一个默认值 —— 证书要的是**文件路径**，而桌面把认证材料按引用存在系统凭据库里。
- **服务器出示 host 证书时会被拒绝。** 这个构建没有 host CA 信任存储——那是另一件事，不是用户证书认证。
- **ssh-agent 的失败无法再细分。** russh 0.63 没有公开 agent 的错误类型，所以「agent 拒绝签名」与「签名中途连接断开」在我们这一侧是同一个错误，也不会被当成可重试的传输失败。
- **没有多因素认证。** 服务器如果接受了公钥还要第二个因素，我们如实报告「被接受但未完成」，不会接着往下走。
- **`filesystem.edit` 的检查与写入之间仍有窗口。** 它比对读取时返回的内容摘要，并要求 `oldString` 恰好出现一次，否则拒绝；但 SFTP 没有事务 —— 摘要一致之后、写入之前，文件仍可能被别的进程改掉。
- **`delivery` / `resume` 的完整语义只在 sidecar 层可用。** `resume: false` 会登记这次请求而不执行、之后用同一个 `messageId` 才真正启动并沿用同一个 `runId`；`delivery: "sync"` 会等到终态并把结果放进响应。但 `duplicate` / `resumed` / `result` 三个字段没有过 IPC（没有消费方），面板从不发 `resume: false`，同步路径只在 router 层被测过。
- **选了 `policyId` 也不会提示它与目标环境不匹配。** 覆盖只决定**用哪张策略表**：危险与关键动作在任何策略下都仍然需要逐项批准，只读与计划模式在任何策略下都拒绝非只读操作；事件流上的 `policyId` 才是实际生效的那个。但「生产策略 + 预发布目标」这种情况，界面不会额外警告。
- **适配器产出的 `usage` 与 `reasoning_delta` 到不了界面。** 两个原生适配器都解析它们，但 agent loop 目前丢弃这两个事件，所以没有 token 统计可看。
- **多模态输入没有实现。** 消息内容的 part 形状为文件/图片/上下文预留了位置，但今天只有文本。
- **Agent 回复里的链接点不开，图片不加载。** 窗口只申请了 `core:default` 能力，没有 opener / shell。要让它可点，得先给桌面端加一个受限于 `http(s)` 的 opener 能力。
- **Markdown 支持的是子集，而且不是一个 CommonMark 实现。** HTML、setext 标题、缩进代码块、引用式链接与脚注都不认，遇到时按纯文本显示；解析器的用例钉住的是「没认出来的东西一个字都不能丢」，而不是规范一致性。
- **Agent 的流式文本与最终文本二选一。** 界面拿到最终 assistant 文本时会替换掉已流式累积的那一行，所以两处不一致时以最终文本为准。

## 有意为之的边界（不是待办）

下面两条看起来像限制，其实是安全模型的形状。它们不会被「补完」，改动它们等于改动授权模型本身（[ADR 0005](#adr-0005permission-engine-是唯一的执行授权决策者)、[ADR 0009](#adr-0009agent-权限采用显式的运行级委托)）。

- **危险动作必须逐项批准，且无法被「记住」。** `docker.restart` 声明为 `high` 风险，因此在任何环境下都不会被自动批准，也不会被会话授权覆盖；会话授权只覆盖非危险操作。这不是还没做「总是允许」，而是**拒绝把它做出来**：模型文本不能成为授权的来源，一次「以后都别问了」的委托正是把危险动作交回给模型。代价很具体：一次长任务里若需要重启容器，用户一定会被打断，也必须在看到具体命令之后再点一次。
- **浏览器预览里没有原生能力。** 在浏览器里打开 Web 前端只能看到界面骨架：终端、远程文件、日志、服务、活动、对话历史和本地数据库都需要 Tauri 桌面应用，因为它们全都走 Tauri 命令与原生侧（SSH、PTY、keychain、SQLite）。这是有意的：WebView 不该持有进程句柄，也不该在它的生命周期里决定一个进程的生死（[ADR 0001](#adr-0001agent-runtime-作为独立-nodejs-sidecar由-rust-拥有其生命周期)、[ADR 0008](#adr-0008rust-负责-sidecar-的启动握手监督与回收)）。代价是：预览只能用来调样式与布局，任何真实操作——包括所有手工验收——都必须在 Tauri 窗口里做。

## 架构决策记录（ADR 0001–0015）

这一节是决策记录本身。**代码注释里 `ADR 0004` 这类编号指向的就是下面每一条**；「决定变了就新增一条，并在旧记录里标明被哪一条取代」是这里的规矩，所以有些条目的状态里带着「某节由某条取代」的注记——那是历史，不是笔误。

每条都记三样东西：**决定**是什么、**为什么**是这个形状、以及**代价**是什么。没有代价的决定不值得记，所以代价那一栏从简不了。

### ADR 0001：Agent Runtime 作为独立 Node.js sidecar，由 Rust 拥有其生命周期

`Accepted` · 2026-09-09。**两处已被取代**：「发布成安装包时必须随应用分发受信任的 Node 运行时」由 [ADR 0013](#adr-0013安装包分发随包分发-agent-bundle但使用用户自己的-node) 取代；「当前不自动重启崩溃的 sidecar」由 [ADR 0010](#adr-0010崩溃后的自动恢复有界且只恢复能力不复活状态) 取代。独立进程、stdio 协议、宿主独占原生能力与凭据这些结论仍然有效。

**决定。** Agent 运行时做成独立 Node.js 进程，由 Rust 宿主拥有其启动、通信和退出，不由 React 或用户操作拥有。

**为什么。** Agent 循环需要三样 WebView 给不了的东西：成熟的流式 HTTP 客户端、可快速迭代的工具与 schema 生态、能在运行中途被取消的执行环境。窗口生命周期、进程生命周期与凭据边界若纠缠在一起，界面刷新就会杀掉一次正在等待用户批准的运行。同时 Agent 也不能拥有远程访问能力，否则权限决策退化为「模型说要连，那就连」。

**怎么落地。** React 只调用白名单命令（`agent_spawn`、`agent_status`、`agent_kill`、`agent_logs`、`agent_run_start`、`agent_run_stop`、`agent_approval_respond`），不知道 Node 路径、PID 或 bundle 位置。sidecar 不连 SSH、不访问 SQLite 与凭据库；需要远端数据时发 `host.tool.execute` 或 `host.context.fetch`，这些请求发出前已过 ToolRegistry 与 Permission Engine，宿主收到后**还会再校验一次目标**。

**代价。** 要维护整套额外契约（JSON-RPC 方法、双向请求、schema 校验、跨语言集成测试、进程监督）；发布安装包要处理运行时与 bundle 的绝对路径；帧有界，日志与模型输出必须在各层截断或分页。

**备选与否决理由。** 把 Agent 放进 Tauri WebView（没有 Node 运行时，且让「窗口是否打开」成为前提）；用 Rust 实现整个 agent loop（失去 TS 迭代速度，并把模型协议差异带进原生核心）；由 React 直接派生子进程（多给界面一项系统能力，且进程状态与界面状态耦合）；让 sidecar 自己持有 SSH 与凭据（授权决策失去唯一入口）。

### ADR 0002：SSH 后端采用 russh，并封装在 `SshBackend` 之后

`Accepted` · 2026-09-09。**认证一节已被后续实现取代**：现在还有 ssh-agent、带口令的私钥，以及用户证书认证。选型本身仍然有效。

**决定。** `crates/ssh` 用 russh 作默认实现，通过 `SshBackend` trait 向上层暴露能力，russh 类型不越过 crate 边界。

**为什么。** 不自研 SSH 协议；不调用系统 `ssh` 子进程——那会继承用户的 `~/.ssh/config`、agent 与 known_hosts 行为，导致「同一份配置在不同机器上权限含义不同」。

**具体取舍。** 用 `ring` 而不是默认的 `aws-lc-rs`，因为后者需要每个平台上都有 C 工具链和 cmake，而 `ring` 提供预编译汇编，CI 不必再加一套构建链；保留 `rsa` 特性，因为仍有大量服务器协商 `rsa-sha2-*`。连接与认证整体受 15 秒硬超时约束，会话空闲超时 60 秒，keepalive 间隔由配置决定（桌面端 30 秒，0 表示关闭），keepalive 失败只记录日志。命令执行分两条路径：只读命令允许在传输错误上重连并重试一次；**有副作用的命令必须走 `execute_once()`，不重试**——一次丢失的响应不能变成第二次重启。单条命令输出传输层上限 4 MiB。SFTP 子系统按需惰性建立并缓存在会话里。

**代价。** 需自维护主机密钥策略与自己的 `known_hosts` 文件格式，不能复用用户已有的语义；首次信任模式意味着第一次连接本身就是信任决策；认证覆盖不全（见[当前限制](#当前限制)）；老旧服务器兼容特判必须留在 crate 内部。

**备选与否决理由。** libssh2 绑定（C 工具链与线程模型适配成本高）；调用系统 `ssh` 子进程（见上）；同时支持多种后端让用户选（会让主机密钥、超时、取消出现多套实现，测试面翻倍——等真需要再加，且必须走同一条 trait）。

### ADR 0003：只实现一个 OpenAI-compatible Provider，两种请求方言覆盖兼容端点

`Accepted` · 2026-09-09。**两处已被取代**：「只实现一个 Provider」由 [ADR 0011](#adr-0011增加原生-anthropic-与-gemini-provider身份分支仍只允许发生在装配点) 取代；「`reasoning_delta` 与 `usage` 不会产出」同样失效——两个原生适配器都会发这两个事件，**只有 OpenAI-compatible 这条路确实仍不产出**。「Provider 身份只在装配点分支」与「不做方言自动探测」两条仍然有效。

**决定。** 当时唯一的 Provider 实现是 `openai-compatible.ts`，实现 `LLMProvider`，把不同兼容端点收敛成模型目录与统一的 `StreamEvent` 流；方言由 `wireApi` 选择（`chat` 或 `responses`）。

**为什么。** 协议差异会立刻扩大状态空间与测试面，而且容易顺着调用链渗进权限判断与工具执行逻辑，而这两者都不应该知道上游是谁。绝大多数实际可用端点（OpenAI 官方、OpenRouter、Ollama、LM Studio、vLLM、企业内部网关）都能说一种兼容方言。

**代价。** 兼容方言的差异真实存在（有的网关拒绝 `tool_choice`、有的不接受 `temperature: 0`、有的在流里插空 delta），目前表现为连接失败或空回复；超时与用户取消在事件层表现不同。

**备选与否决理由。** 三个原生 Provider 一次做完（第一阶段风险在权限与执行链路，所以推迟——而 [ADR 0011](#adr-0011增加原生-anthropic-与-gemini-provider身份分支仍只允许发生在装配点) 后来做了）；引入第三方统一抽象库（把上游差异转成第三方库差异，并多一层不可控中间件）；在 loop 里按 Provider ID 分支；把 `wireApi` 做成自动探测（探测失败要发两次真实请求，且让「发出去的到底是什么」不可解释）。

### ADR 0004：内部工具名用点号，Provider 边界用双下划线，映射集中在一处

`Accepted` · 2026-09-09。成本一节「未来接入 MCP 时外来名称必须先被命名空间化并经过冲突检查」**已经落地**，见 [ADR 0014](#adr-0014mcp-接入宿主独占进程与目录agent-只注册与调用)。

**决定。** 内部一律用点号名称（`docker.ps`、`filesystem.read`）；发给 Provider 的名称改为双下划线（`docker.ps` → `docker__ps`）；映射只有一个实现——规则定义在 `packages/shared/src/naming/tool-name.ts`，映射与声明列表由 `packages/provider-sdk/src/name-index.ts` 生成，任何工具实现、Provider 实现或界面代码都不允许手写这套转换。

**为什么。** 多数 function calling 网关对函数名有更严格的字符集与长度限制（常见规则是不允许点号），**静默改写比拒绝更严重**——模型回传一个没见过的名字，若进了日志和审计，调用链就再也无法与真实工具对应。

**可判定性规则。** 内部名称必须是 `namespace.action` 形式，每段匹配小写标识符规则，整体不得包含双下划线；因为各段不允许下划线，`__` 与 `.` 互换无歧义。索引构建时检查两个内部名是否映射到同一个 Provider 名，命中即抛错；注册期有同样的宽松检查。**不猜测**：模型返回未声明过的工具名时返回「未知工具」错误结果而不是模糊匹配；宿主在握手阶段检查冲突列表，只要 sidecar 报告存在名称冲突就拒绝发布这个运行中的 sidecar。名称受 64 字符上限约束。

**代价。** 多一层间接（调试时须记住模型看到的 `docker__ps` 与内部 `docker.ps` 是同一个东西，这层刻意只存在于两个文件里）。

**备选与否决理由。** 内部也用双下划线（命名风格取决于上游网关，审计可读性差）；每个 Provider 各自做转换（规则复制、冲突检查分裂）；网关拒绝点号时自动降级为下划线（映射一旦不确定，审计就不可信）；用哈希作 Provider 侧名称（模型无法从名字推断用途，排障可读性归零）。

### ADR 0005：Permission Engine 是唯一的执行授权决策者

`Accepted` · 2026-09-09。**已知缺口：无。**

**决定。** 风险**事实**可以有多个来源，但只有一个地方能把事实变成**决策**——`PermissionEngine.evaluate()`。

**三层事实。** 工具声明（工具作者声明的静态风险底线）、命令分析（16 条规则）、目标环境的风险下限（本机与开发 `low`、预发布 `medium`、生产与未知 `high`）。合成过程是确定的五步，细节见 [执行与授权模型](#执行与授权模型)。

**危险档位的加严（已决策）。** 曾经存在一处分裂：引擎命中会话授权时按**内在风险**判断，而执行闸口拒绝任何 dangerous 且非逐项批准的调用；生产环境的风险下限是 `high`，于是任何普通写入都被升级到 dangerous，「在生产目标上批准本会话」会得到一个引擎报自动、执行必然拒绝的决策。收敛方向是**收紧引擎**，三条理由：引擎在更早位置已确立同一不变式，会话授权分支把它重新打开属于自相矛盾；引擎是唯一授权决策入口，它**不能宣布一个执行层会拒绝的批准**——错误的自动批准不可见，而安全的失败（多一次确认点击）是可见的；执行闸口是唯一防线，放宽它等于让未经逐项批准的写入通过。因此会话授权的判断依据是**环境升级后的最终风险**。代价：在生产或未标注环境上，普通写入不再能通过「批准本会话」免除确认。

**代价。** 需维护风险事实与票据两套契约，新增工具必须声明风险、超时和重试策略否则注册被拒；需区分「内在风险」与「最终风险」；未来的团队策略或 RBAC 只能扩展策略来源，不能在其他模块建立第二个授权决策点。

**备选与否决理由。** 每个工具自行判断（规则复制 N 处，无法统一解释）；由 Provider 或模型声明风险等级（模型输出是最不可信的输入，用它决定自己的权限等于没有权限）；只按环境做策略不做命令分析（会漏掉「同一条只读工具被塞进破坏性命令」；误报的代价是一次确认点击，漏报的代价是生产事故）；把审批做成全局开关（一次同意会覆盖它没有涵盖的操作）。

### ADR 0006：sidecar 通过 stdio 上的 NDJSON JSON-RPC 通信，协议版本 1.0

`Accepted` · 2026-09-09。**两处注记**：`capabilities.mcp` 取值为假时的**理由**由 [ADR 0014](#adr-0014mcp-接入宿主独占进程与目录agent-只注册与调用) 取代——取值没变，解释变了；方法面已经扩大，多了 `provider.test` 与 `host.mcp.catalog`。协议版本、帧规则与受理凭证的幂等语义都没变。

**决定。** Rust 启动 sidecar 并持有句柄，双方通过 stdin/stdout 传 NDJSON 编码的 JSON-RPC 2.0。细节（握手顺序、方法表、帧上限、畸形帧、受理表、错误码、转发闸门）见 [架构总览](#架构总览)。

**为什么。** 不需要端口和本机防火墙配置；父进程关闭管道即可发现子进程退出；帧是可读 JSON，冒烟脚本用真实传输跑完整握手；三语言共享同一份定义；双向复用一条流，「Agent 请求宿主」不需要第二套连接或超时模型。

**代价。** 浏览器开发模式无法复用 stdio sidecar；两侧各有一份版本常量须手工保持一致，不一致的后果是启动失败（「半懂协议的组合比启动失败更危险」）；大块内容必须在各层截断；NDJSON 没有 schema 版本协商能力，字段变更只能靠加可选字段或提升协议版本。

**备选与否决理由。** 本地 HTTP 或 WebSocket（端口分配与冲突，并把能执行工具的接口暴露给本机其他进程）；让 WebView 直接连 sidecar（绕过 Tauri 命令白名单）；用 Tauri 自带 sidecar 通道（无法满足双向请求与自定义取消语义，也难在纯 Node 环境测试）；gRPC 或消息包编码（需代码生成与额外运行时依赖，收益只是体积更小）。

### ADR 0007：pnpm 与 Cargo 双 workspace，并优先稳定变化最频繁的边界

`Accepted` · 2026-09-09。**一处更正**：决定里列的 workspace 成员写于 `crates/time` 出现之前，**漏了它**——根 `Cargo.toml` 现在还有 `crates/time`（时间戳的唯一实现）。双 workspace、依赖集中声明、契约库先构建与 fixture 双向校验这些结论不变。

**决定。** pnpm workspace 管 TypeScript 包、Cargo workspace 管 Rust crate，优先把**变化最频繁、被引用最多**的边界固定成可编译、可测试的接口。依赖版本集中声明在根 `Cargo.toml` 的 `[workspace.dependencies]`，内部 crate 以路径引用。`rusqlite` 用 `bundled` 省掉系统 SQLite 差异；`keyring` 启用三个平台的原生后端。

**三条工程规则。** 实现细节只能向内依赖，`crates/*` 的公开类型不含后端库类型（没有 russh 类型出现在 `SshBackend` 之上）；**契约库先构建**——消费方导入 `packages/*/dist/*.d.ts`，本地门禁第一步就是构建它们，之后才做类型检查，顺序不能颠倒；契约在运行时也被校验——每个 Tauri 命令在界面侧用 schema 解析参数与返回值，Rust 侧用对齐的 serde 结构，fixture 被两侧同时解析。

**代价（含两处自我更正，值得原样保留）。**

① 目录与抽象比「一个应用目录搞定」多得多，初期样板与阅读成本更高；② 门禁要覆盖两套工具链和跨语言测试，`pnpm check` 步骤较多，这也是把它做成单一入口并让 CI 跑同一条命令的原因；③ **抽象容易过剩**——`crates/filesystem` 曾长期只有文档注释，既没实现也没被任何 crate 引用，而「远端文件能不能碰」的真实规则同时散落在两个 command 文件里。占位符本身没有价值，价值在于它逼出的问题：**到底谁该拥有这条规则**。它现在有了实现与消费方，成本的准确说法是「先写下边界、再证明它值得存在」这段时间里规则会继续留在错误的地方，而这段时间必须短；④ `Cargo.toml` 里两条目性质不同——`[workspace.dependencies]` 的别名在没人引用时确实惰性，但 `members` 里那条会让它进入 `cargo check --workspace`，所以**新增边界之前要先确认真的有两个以上消费方**；⑤ `packages/shared` 会变成热点，必须保持没有副作用、没有运行时依赖，否则会拖慢每一次构建。

**备选与否决理由。** 每个应用一个独立仓库（跨语言契约会被复制多份）；单一语言实现（纯 Rust 失去 Provider 与工具迭代速度，纯 TS 无法直接操作 SSH、PTY、系统凭据库、SQLite）；只做类型共享不做运行时校验（类型在 JSON 边界上无效）；不做内部抽象让上层直接用后端库类型（后端库升级变成全仓库改造）。

### ADR 0008：Rust 负责 sidecar 的启动、握手、监督与回收

`Accepted` · 2026-09-09。**三处已被取代**：「不自动重启」与「随包分发 Node 运行时」及「`bundle.active` 为 `false`」分别由 [ADR 0010](#adr-0010崩溃后的自动恢复有界且只恢复能力不复活状态) 与 [ADR 0013](#adr-0013安装包分发随包分发-agent-bundle但使用用户自己的-node) 取代。另有一处随之失效：「上一次异常退出的退出码保留到下一次成功启动」——自动恢复落地后该记录只在用户主动启动时清除，自动重启会**刻意把它留下**，界面因此可能同时显示「正在运行」与「上次崩溃过」。

**决定。** 进程与传输归 `yukinal-core` 的 sidecar 模块，状态、日志、单实例和退出记录归同 crate 的 supervisor 模块；Tauri 命令只做参数编组。

**启动顺序是固定的：`spawn → subscribe → initialize 握手 → 发布运行状态`。** 必须先订阅事件再握手，否则握手期间的输出会丢失；握手失败（含协议版本不匹配、工具名冲突）必须关闭子进程，不能发布半初始化的运行状态。解析顺序见 [打包与分发](#打包与分发)。

**生命周期规则。** 一个 supervisor 只保留一个运行中的 sidecar；启动与停止**共用同一把锁**（避免两个并发启动各自看到空槽位、最后留下无人管理的进程）；stderr 保留最近 200 行；sidecar 监听父进程 stdin，父进程消失时主动结束，Rust 正常关闭时先请求 sidecar 退出、`kill_on_drop` 作为兜底；Windows 上以 `CREATE_NO_WINDOW` 启动避免控制台闪窗；**崩溃不会被伪装成「运行完成」**——supervisor 清空运行状态并记录退出信息，界面通过状态轮询发现（运行时 1.5 秒一次，未运行时 5 秒一次）。

**界面看到的是事实，不是推断。** 状态里回报是否在运行、PID、协议版本、Agent 版本、握手时登记的工具数量、**实际启动的入口路径**以及上次退出记录。报告入口路径是有意的：开发机上有多个构建产物时，「到底启动了哪一个」必须能从界面直接读出。

**代价。** 发布安装包时必须提供绝对 bundle 路径；当时 `bundle.active` 为 `false`；不自动重启的代价是崩溃中断用户会话（若做自动重启，必须同时解决「正在等待的审批怎么办」）。

**备选与否决理由。** 由 React 启动子进程；把 sidecar 做成操作系统服务或开机自启（引入安装、升级、权限、生命周期成本，与「桌面应用按需工作」不符）；崩溃后立刻自动重启（重启风暴，且让在途审批与运行状态含义模糊——「先做可见性，再考虑自动恢复」）；把 Node 运行时静态链接进桌面二进制（推迟到真正做安装包时决定）。

### ADR 0009：Agent 权限采用显式的运行级委托

`Accepted` · 2026-09-09。

**决定。** 每次 Agent 运行可在请求里带一个用户选择的委托模式：`ask`（只读自动执行；写入、部署、重启和危险操作执行前等待用户批准，**即使策略表允许自动执行**）、`auto`（用户明确委托 Agent 在**开发或预发布**目标上批准 `write` 档位操作；高危与 critical、本机、未知和生产操作仍需逐项人工批准，策略拒绝仍然拒绝）、省略（保持「只按环境策略执行」）。

**为什么。** 仅按环境策略执行无法同时服务两种真实用法——逐项审查变更，与委托明确任务（「把这个容器重启一下，确认健康后再看日志」）；反复弹确认框只会让人习惯性点批准，反而降低警觉。而放开自动执行又带来「模型输出不能等于授权」的问题：若模型在文本里说「用户已经同意」，协议里必须有东西能证明这句话是假的。系统还需区分三种自动执行理由，否则活动记录无法说明责任落在谁身上。

**委托不是新的授权入口。** `auto` 只让引擎在既有策略之上做一次收窄判断——只有档位是 `write`、目标环境是开发或预发布、且策略本身没有拒绝时，结果才变成自动，且**来源标记为 agent**；其余一律回落 `ask`。

**第二个正交轴。** 运行模式决定这次运行**能改到什么程度**，批准方式决定**允许的部分由谁点头**；两者独立，所以「只做计划 + 自动批准计划里允许的部分」是合法组合。只读与计划模式下的非读操作由引擎直接拒绝，判断发生在策略、委托和会话授权**之前**——限制是被执行的，不是被请求的。

**区分体现在审计而不只是提示词。** 引擎生成带来源的决策，loop 依来源生成三种自动票据，registry 复核自洽性，数据库允许的来源从两种扩展为三种（通过一次表重建迁移完成并保留已有数据）。

**代价。** 需维护票据与决策两套结构的自洽性；两轴增加界面与文档复杂度（必要——合并成一个「权限等级」会让「只读 + 自动批准」无法表达）；**自动委托边界硬编码在权限引擎里，放宽它需要一条新的 ADR 而不是一次配置改动**（有意的防误改设计）；会话授权作用域需 loop 主动维护。

**备选与否决理由。** 只提供「本次运行全部允许」总开关；把委托做成持久化用户偏好（会让授权脱离具体任务与具体目标，而整条审计链都建立在「这次是谁批准的」之上）；让模型在工具调用里声明「用户已授权」；用提示词请求模型遵守只读（提示词不是执行机制，所以只读限制实现在权限引擎里）。

### ADR 0010：崩溃后的自动恢复有界，且只恢复能力、不复活状态

`Accepted` · 2026-09-12。带一段**后续修订注记**：原文说「由下一次启动的 sidecar 收尾为中断」写错了前提——持久化的执行记录只在结果事件到达时写一行，没有任何代码会在调用开始时写「在途」记录，所以崩溃留下的形态是「这一行不存在」而不是「这一行停在 running」。真正要守住的是相反的一条：**别让在途状态被写进账本**，这一点已经实现并有测试钉住。本次**有意没做**的是为在途调用写一行并在崩溃时标记中断——那会让账本重新出现需要收尾的行，而应用自身被杀时同样会留下。

**决定。** 只重启**不是被要求的**退出；重启有界；重启必须留下痕迹；重启只恢复能力、不复活任何状态。

**「只重启不是被要求的退出」的判据是 pid 而不是标志位。** 停止路径在关闭子进程**之前**先取走运行槽位，因此「观察到的退出，其 pid 已经不是当前记录的 pid」只有两种可能——用户要求过，或已有更新的进程顶替了它。用标志位意味着要信任现在和将来每一个调用方都会去设置它；用槽位状态则不需要。这条约束写在代码注释里，并由一条集成测试固定（一次被要求的停止不会被重启）。

**有界策略。** 默认最多 5 次，延迟从 1 秒开始翻倍、上限 30 秒；连续服务超过 60 秒视为新基线（跑几小时后的偶发崩溃不应消耗上次事故留下的预算）；预算用尽后 supervisor **停止尝试**并把这件事写进状态，不静默永远重试；**重启本身失败也消耗同一次预算**（失败的重启不产生退出事件，没有其他机制会注意到它）。

**留下痕迹。** 上次退出记录不被清除（只有用户主动启动才清），界面因此能同时显示「正在运行」与「它是被重启回来的」；状态里的重启记录报告第几次尝试、上限是多少、是否已放弃；stderr 尾部插入**进程代次标记**与重启原因——否则上一个进程的遗言和新进程的开场白会连在一起，而「这一行是死掉的那个进程说的吗」恰好是最需要答案的时候。

**只恢复能力。** 崩溃时在途的每一次运行与每一条审批都随进程消失、不会被恢复；用户看到的是一句「sidecar 已退出，本次运行已中断」，重启后重新发起即得到一条新运行。

**顺带修掉的双重转发。** 事件转发器过去由每次启动创建、并依赖退出事件结束自己的任务，而 supervisor 刻意不转发该事件——于是崩溃重启后会挂上第二个转发器，每一帧被转发两次，**每一个宿主请求都会被「执行两次」**。它现在是窗口级单例。这不是自动重启引入的问题，是自动重启会把它从「偶发」变成「每次崩溃必现」。

**代价。** 崩溃循环会消耗最多 5 次尝试后才停手；重启后的进程是全新的（没有会话记忆、没有待审批请求、没有正在执行的工具）；上次退出记录与重启记录会在自动重启后继续存在；判断「这次退出是不是被要求的」依赖「取走槽位」这一约定，是隐式耦合；重启策略目前是编译期默认值，没有用户可调开关。

**备选与否决理由。** 维持不自动重启（把恢复成本转嫁给用户，而崩溃最常见场景恰是用户正在等长任务时）；无条件重启每一次退出（停止按钮会变成谎言）；把运行状态检查点化、重启后接着跑（「上一个进程执行到哪一步、哪些副作用已经发生」在工具已经发出请求之后无法可靠回答）；由界面负责重启（WebView 与窗口生命周期绑定）；用指数退避无限重试（坏 bundle 会形成后台重启风暴，日志尾部被同一段错误刷满，真正原因反而被挤掉）。

### ADR 0011：增加原生 Anthropic 与 Gemini Provider，身份分支仍只允许发生在装配点

`Accepted` · 2026-09-12。

**决定。** ① `AiProviderKind` 变成真正的多值轴，在所有层同步；② `kind`（谁翻译）与 `wireApi`（同一种翻译里的哪套方言）是两个**正交轴，不合并**；③ 唯一允许按身份分支的地方仍是装配点的 `buildProvider()`，agent loop 继续只认 `LLMProvider` 与 `StreamEvent`；④ 凭据边界不变；⑤ 模型目录按各自协议读取，**目录读取失败不阻止运行**（用户始终可手填 model id）；⑥ 保存与配置路径必须按 `kind` 分派——「一个只能保存一种 kind 的设置页等于实现了一个用户配不出来的 Provider」；⑦ 适配器可以产出 `usage` 与 `reasoning_delta`，消费方必须继续容忍它们缺席。

**为什么（推迟的代价可直接观察，且不是「多一点工作量」）。** 兼容端点只能覆盖请求与响应的**形状**，覆盖不了协议本身表达的东西：Anthropic 把系统提示放在顶层字段、工具调用是内容块、参数以增量分片到达、鉴权是 `x-api-key` 加一个**按日期版本化**的头；Gemini 用 `systemInstruction` 与 `functionCall` part、流式需要 `alt=sse`、而且**不返回工具调用 id**。这些翻译不是可选装饰：写错它们会体现在工具调用能不能闭环、取消能不能及时生效、错误信息是不是可读上。用兼容端点凑意味着用户自己搭代理并承担同样的翻译工作——把问题从我们的代码挪到用户的运维里。

**代价。** 三套方言一起维护，其中两套有会过期的版本化头——升级不是可选项，是一次必须跟随的维护动作；Gemini 不返回工具调用 id，所以同一回合里同名函数被调用两次是协议层面的歧义，实现里必须把这个不对称写在注释与工具结果里而不是假装有 id；新增两类真实失败模式（拒答、缺少终止事件的截断流）；`AiProviderKind` 从单值变多值暴露出一处既有隐式错误——数据库读路径曾忽略 `kind` 列并硬编码 openai-compatible，不修它新 kind 存进去会被读错，因此属于本次改动的一部分而不是后续优化。

**备选与否决理由。** 继续只用兼容端点；只做 Anthropic、Gemini 以后再说（两套翻译不同但同构，一次做两个比分两次便宜，且「另一种 kind」这件事本身需要先被证明架构撑得住）；引入统一第三方多 Provider 抽象库（会把权限、命名映射与错误脱敏这些属于我们的决定交给库的形状）；把 `kind` 与 `wireApi` 合并成扁平枚举；自动探测 dialect 或 kind（探测要花真实请求，且让「到底发出去了什么」无法从配置解释）。

### ADR 0012：主机指纹默认 TOFU，但必须能人工核验、钉住与遗忘

`Accepted` · 2026-09-12。

**背景（能力已经在，但没有出口）。** 存储层能存能查，严格策略能在建立 TCP 之前拒绝未钉过的主机，也能钉住指纹；问题是钉住的动作**全仓库零调用方**——没有命令、没有界面元素，而桌面端唯一的生产配置把策略写死为首次信任，所以严格模式只存在于测试里。最糟的一条是**验证失败时用户看不到该看的指纹**：未钉住的主机报一个占位说明，而「钉住的与出示的不一致」最终表现为一次通用握手失败——一次可能的中间人攻击与一次完全正当的服务器密钥轮换对用户呈现相同，且都没给出新指纹，用户既无法判断也无法接受。

**决定。** ① 默认仍是首次信任，已钉住则按 pin 校验，不一致即拒绝；② **新增三个显式入口**——探针（真的连一次，返回服务器**实际出示**的指纹但**不写入任何东西**）、钉住（把用户确认过的指纹写进 store，这是唯一会写 pin 的用户动作）、遗忘（删除某个 `host:port` 的 pin，下一次连接回到首次信任）；③ 验证失败必须同时给出两个指纹；④ **探针结果在被用户确认之前是不可信的**——界面文案必须写「服务器出示的指纹」而不能写「已验证」，探针不得持久化任何状态；⑤ **不提供「不一致时仍然继续」**——没有「记住并继续」按钮，一次变更只能由用户先遗忘再重新探针确认；⑥ 指纹格式与存储位置不变，存文件不存 SQLite；⑦ **pin 按 `host:port` 关联，不按 server id 关联**（同一台机器在两个条目下必须共用同一条 pin，否则会出现两份互相矛盾的信任状态）。

**为什么不是强制严格模式。** 真实结果是用户到处复制粘贴指纹而不是核验，或者干脆绕开这个应用；而维持「只 TOFU 无出口」的现状更糟——一次合法的服务器密钥轮换会让用户永久无法连接，那种「安全」是假的。

**代价。** 探针会真的发起一次连接并在服务器上留下一次（通常未认证的）连接与日志记录，它不是纯本地操作；首次连接仍然是 TOFU，不加一步确认意味着第一次连接仍可能被中间人利用，**这是接受下来的风险，不是遗忘**；pin 与服务器条目不是一对一，用户可能对「我在一个条目上忘了 pin，为什么另一个条目也放行了」感到意外，必须写在界面文案里；遗忘是危险动作（下一次连接会静默重新首次信任）。探针结果可能被误当成「已验证」，只能靠文案缓解，无法靠机制消除。

**备选与否决理由。** 强制严格模式；维持现状；把 pin 存进 SQLite 并按 server id 关联（两个条目会得到两份矛盾 pin，且与已有 `known_hosts` 文件形成两个真相来源）；在不一致时提供「记住并继续」（那是一个把中间人攻击变成一次点击的按钮）。

### ADR 0013：安装包分发：随包分发 agent bundle，但使用用户自己的 Node

`Accepted` · 2026-09-12。

**背景。** 仓库当时**没有安装包**：`bundle.active` 为 `false`，完整图标没有地方引用，没有打包步骤，CI 从不构建桌面应用。当时的形状还有第二个问题：agent 由 tsc 产出并依赖 workspace 包，运行时需要 `node_modules` 在它上面——安装包里没有 `node_modules`，也没有仓库树可供向上查找。

**决定。** ① 启用 Tauri 打包，显式列目标平台与图标；② agent 以**单个自包含 ESM 文件**随包分发，因此安装包里不需要 `node_modules`；③ **不随包分发 Node 运行时**，安装后的应用使用用户系统上的 Node，版本下限与 `engines.node`、esbuild 的 `--target` 一致（Node 24），`YUKINAL_NODE` 仍可把这件事钉死到绝对路径；④ 解析顺序里插入安装包一项，且排在开发兜底之前；⑤ **bundle 在安装包里的位置是契约**，单边改名只会被已安装用户发现；⑥ **缺少 Node 必须是可执行的错误**，但**不做**版本预检；⑦ 本次**不做代码签名、公证与自动更新**——需要本项目并不持有的凭据。

**与早期 ADR 的冲突是知情的。** ADR 0001 与 0008 都要求随包分发受信任的 Node 运行时，这里明确选择不分发：随包分发意味着每个平台一份约 100 MB 的运行时、CI 里每平台一次带完整性校验的下载、以及随之而来的许可与声明义务，而目标用户是开发者、机器上几乎必然已有 Node。ADR 0001 关于「PATH 上的 `node` 可以被替换」的论点被承认为**残留风险**（本应用不是用户自己机器的安全边界；需要钉死的用户可用 `YUKINAL_NODE`）。**这不是永久否决**：当「用户机器上没有 Node」成为真实反馈时应当重新评估，届时本记录要被**取代**而不是被改写。

**代价。** 安装包未签名，是本次留下的最大可用性缺口；「装了但太旧」不会被专门识别（Node 因解析失败退出，用户看到一行语法错误加退出码——不要声称我们能诊断版本过旧）；打包不在本地门禁里，`pnpm check` 保持平台无关、不构建安装包，因此在只装了部分平台工具链的开发机上本地产出的安装包无法被验证——**不能让「配置了」被读成「验证过了」**。多平台构建需要各自工具链，打包工作流是 CI 里最慢也最容易因环境而红的一步。

**备选。** 随包分发受信任的 Node 运行时；把 Node 静态链接进桌面二进制或改用嵌入引擎（前者在 Tauri + Rust 上没有可维护路径，后者不是「打包 Node」而是换掉整个 agent 运行时）；用 Node SEA 之类编译成自包含二进制（仍是实验特性、需按平台各出一份，且会让用户无法用自己熟悉的 `node` 跑同一份代码）；维持不打包（一个装不上的应用不是产品）；只把前端作为静态站点分发（原生能力才是存在理由）。

### ADR 0014：MCP 接入：宿主独占进程与目录，agent 只注册与调用

`Accepted` · 2026-09-12。**一处注记**：背景里「17 个集成测试」这个计数不准，实际是 18 个，跑在同一个已提交的 Node fixture 上——这个数字在记录写下时就已经错了，所以它仍只证明「与 fixture 的行为一致」。

**背景。** MCP 客户端早已存在（进程、握手、工具列表、工具调用、超时、退出记录、stderr 尾部），配置表与 repository 也在，但**没有任何东西把它们连起来**：没有命令读写配置表，没有代码把远端工具变成注册表里的工具，而能力标志必须保持真实。三条既有约束（进程归 Rust、唯一授权决策者、内部名点号分段）决定接入只能有一个形状。还有一件必须在设计期承认的事：**MCP 没有静态工具表**——想知道一个服务器有哪些工具，唯一的办法是把它的进程起起来问它，这让「读取工具列表」成为一个有副作用的操作，而它恰好又是 sidecar 启动时最想要的东西。

**决定。** 九条，逐条与实现对照见 [外部工具（MCP）](#边界外部工具mcp)。

**收益。** 可配置、启动、观察、调用一个 stdio MCP 服务器；「Rust 拥有进程」在 MCP 上仍然成立且**只有一个** MCP 客户端实现；一次 MCP 调用的归属在审计里能落到具体服务器；失败带退出码与「不会自动重启」的说明而不是挂住；`http` 被拒绝这件事只有一个来源，界面、命令、目录都复述同一句话。

**代价与限制。** 取消不撤回副作用（线上协议没有取消一次 `tools/call` 的方法）；目录请求会在启动时派生第三方进程（一个启用但从未启动过的服务器会被起起来，因此 agent 启动多了一次最多 4 秒的等待）；`capabilities.mcp` 在握手时可能仍为假（目录必须在 stdio RPC 之后取，否则宿主握手会等一个永远不来的请求）；没有信任评审流程，所以 `trustLevel` 永远停在 `unreviewed`、`allowedTools` 永远是空表——这是当前唯一诚实的状态；结构化归属没有落库；任务管理器强杀仍会留下子进程；**没有真实的第三方 MCP 服务器被验证过**。

**备选与否决理由。** 让 sidecar 直接派生 MCP 进程（会把「谁回收孤儿进程、谁记录退出码、stderr 归谁」变成两个实现）；给 MCP 工具单独一条执行方法（会造出第二条执行通道，取消、审计、目标校验、错误词汇都要写两遍）；在握手之前同步取目录让能力标志一定为真（宿主在握手完成前不会转发 sidecar 请求，这会死锁到超时；要改就得动启动顺序，需要单独一条 ADR）；采信 MCP 工具注解来定风险档位（那是让服务器决定自己的授权级别；注解可作为**展示**信息但不能作为档位来源）；崩溃后自动重启一次（可能重复执行一个已经产生副作用的调用）；支持 `http` 传输并在失败时提示「暂不可用」（出站网络策略还不存在，这不是「暂时不可用」而是一条尚未定义的安全边界；类型层面拒绝比运行期拒绝更早、更难绕过）。

### ADR 0015：Agent 回复用自建的 Markdown 渲染，且不产生可导航链接

`Accepted` · 2026-09-12。决定与代价见 [Agent 回复的 Markdown 渲染](#agent-回复的-markdown-渲染)；这里记的是为什么不用现成方案。

**为什么不用 Markdown 库加净化库。** 那套做法是「**默认放过、列举禁止**」：漏掉一条规则就是一个洞，而每加一个语法特性都要重新审视那张净化规则表。把整条「字符串变成标记」的路径去掉，比把过滤写得更好更可靠。仓库自己的解析器只认对话正文需要的那一小撮语法，并且只产出数据结构；渲染层只创建元素，于是没有一张规则表需要维护对错，也没有第二条把不可信文本变成标记的路径。

**为什么链接不可点。** 窗口只申请了 `core:default` 能力，没有 opener/shell，所以 `<a href>` 被点击时不会交给系统浏览器，而是让 webview 自己导航过去——「点一下，整个工作区就没了」。让它可点意味着给桌面端加一条受限于 `http(s)` 的出站通道，而这一版还没有对它的策略；先做到「地址看得见、能复制」是这一步能给出的正确答案。

**备选与否决理由。** 用 Markdown 库加净化库走 `dangerouslySetInnerHTML`（换两个依赖、一张净化规则表，以及一条把字符串变成标记的路径）；只用 `white-space: pre-wrap` 保住换行不做结构化渲染（改动最小，但问题没被解决）；现在就让链接可点（见上）。

### 决策记录里的两处历史瑕疵

翻到这一节的人可能会注意到两个不一致，它们**不要**被当成需要修的东西，而是被记录下来的事实：

- 有一条决定从未被单独记成 ADR：**「浏览器预览里没有原生能力」**。它在早期 ADR 里被当成一条备选引用过，还写成过指向一篇并不存在的 ADR 文件的链接。那条链接随这次文档合并消失了；决定本身仍然有效，理由写在[有意为之的边界](#有意为之的边界不是待办)里。
- 决策记录里的计数会过期（例如集成测试的条数）。凡是与代码冲突的计数，**以代码为准**，并顺手把记录改对——这正是「决定变了就新增一条记录」这条规矩存在的原因。

## 版本与发布历史

版本号遵循语义化版本，并且是**唯一**的：六个 `package.json` 与 `tauri.conf.json`、Cargo workspace、IPC fixture 里的版本由 `packages/shared/src/version.test.ts` 钉在一起（那张清单就是它的 `MANIFESTS`，一共七项）。`1.0.0` 之前不承诺向后兼容：Tauri IPC 命令、sidecar JSON-RPC 方法和跨层类型都可能变化，破坏性变化会写在这一节里。这一节取代了过去那份独立的变更日志文件。

### 未发布

**新增**

- **原生 Anthropic Messages 与 Gemini `generateContent` Provider**（[ADR 0011](#adr-0011增加原生-anthropic-与-gemini-provider身份分支仍只允许发生在装配点)）：两套协议各自一个适配器，系统提示、工具调用与终止原因的翻译留在适配器内，agent loop 不按 Provider 身份分支。两者都能解析 token 统计与推理增量——OpenAI-compatible 那条路径两者都不产出。
- **`filesystem.edit`**：带内容摘要校验的读-改-写。读取返回内容的 SHA-256，编辑要求它仍然一致、且 `oldString` 恰好出现一次，否则拒绝而不是覆盖。竞争窗口被收窄，但没有关闭（SFTP 不提供事务）。
- **ssh-agent 与带口令的私钥**：桌面端第三种认证方式与口令输入框；口令材料只进操作系统凭据库，SQLite 只存引用。
- **主机指纹的手动核验入口**（[ADR 0012](#adr-0012主机指纹默认-tofu但必须能人工核验钉住与遗忘)）：探针、信任、遗忘、状态四项；校验失败同时给出**已钉住的**和**出示的**两个指纹，而不是一个占位字符串。
- **sidecar 崩溃后的有界自动恢复**（[ADR 0010](#adr-0010崩溃后的自动恢复有界且只恢复能力不复活状态)）：最多 5 次、退避到 30 秒、60 秒后视为健康；状态里的重启记录报告第几次、是否已放弃。用户按下的停止永远不会被回答成一次重启。
- **MCP stdio 客户端与接入**（[ADR 0014](#adr-0014mcp-接入宿主独占进程与目录agent-只注册与调用)）：握手与版本协商、工具列表与调用、每次调用的超时、有界诊断尾部、退出记录、每个服务端单实例；服务器的新增/删除/启动/停止进了设置页，工具目录经新增的宿主方法交给 sidecar，外部工具以 `mcp.<服务器>.<工具>` 注册进同一个 registry 并一律声明为 `critical`。看列表不会启动任何进程，`http` 在类型层面被拒绝。
- **安装包**（[ADR 0013](#adr-0013安装包分发随包分发-agent-bundle但使用用户自己的-node)）：启用打包并列出目标平台，agent 以单文件资源 `<resource_dir>/agent/index.js` 随包分发（同目录还有一份声明 ESM 的一行 `package.json`），Node.js 由用户自备（≥ 24）。
- **`delivery` / `resume` / `policyId` 的真实语义**：同步投递会等到运行结束并返回结果，登记而不执行会用同一个运行 id 在后续请求里真正启动，未知的策略 id 报错而**不会**回落到环境默认策略。
- **Agent 回复按 Markdown 渲染**（[ADR 0015](#adr-0015agent-回复用自建的-markdown-渲染且不产生可导航链接)）：标题、列表、表格、围栏代码块过去都是原样显示的字符。解析器是仓库自己的纯函数（无新增依赖），只把文本变成数据结构，渲染层只创建元素——正文里的 HTML 显示为文字，链接与图片不产生可导航元素；未闭合的围栏按「流式到这里为止」处理。
- **Provider 可以删除了**：此前只能新增和启用，写错的一份配置就永远留在列表里。删除走行内二次确认，并说清两件从别处看不出来的事：密钥会不会被一并删掉（引用可以被多行共享，所以只有没有别的行还引用它时才回收），以及当前项被删后由谁接替。这条守卫在数据库层按**整张表**问，所以二次确认里那句话是字面成立的。
- **对话记录视图**：记录过去是一次取 50 条按更新时间平铺的卡片，归档后从列表里消失、删除时弹阻塞式确认框。现在按本地日历日分组（今天 / 昨天 / 最近 7 天 / 更早，组标题吸顶）、标题与消息正文的命中直接标在行里、默认只列进行中并显示三个标签各自的真实条数、每页 50 条往下翻、就地改名、删除改成行内二次确认（默认焦点在「取消」上，回车不会删掉东西）、方向键在行间移动、斜杠键跳到搜索框。行上的目标是服务器的**名字**，服务器已被删除时写「已移除的服务器」——不透明的 id 不上界面。

**变更**

- IPC 契约若干处与语义一起收紧：运行开始的响应新增是否真的开始与可选结果；Provider 保存从「只有一种协议」的命令名变成带 `kind` 的统一命令；文件读取的输出新增内容摘要；Agent 状态新增可选的重启记录。`1.0.0` 之前不承诺兼容。
- 脱敏逻辑从 sidecar 私有提升为 crate 级能力，因为 MCP 客户端也要把陌生子进程的输出交给用户。顺带修掉三处真实漏检：`Authorization: Bearer <token>` 的顺序问题（token 曾原样留存）、`secret=` 未被当作敏感标记、以及显式方案名后短凭据的漏检。
- `apps/agent` 的项目关闭了 emit：它曾经把逐文件的 tsc 产物盖在 esbuild 的单文件 bundle 上，让「类型检查通过」之后打包契约反而变红。修在 `package.json` 的脚本上不够——手写一条 `tsc` 命令会绕过脚本，而这件事真的又发生过一次。
- `initialize` 里的 MCP 能力标志从字面量改成读注册表：它报告的是「此刻注册表里有没有 MCP 工具」，不是「这个构建支不支持 MCP」。握手时它通常仍为假，因为工具目录只能在 stdio 通道起来**之后**去取——那正是实话。
- **窗口里只有「你与 Agent 的对话正文」能拖动选中。** 原来整个界面都可选中，随手一拖就把侧栏、工具卡片和标签一起涂蓝，复制出来的内容里混着界面自己的字。代价是日志、文件预览、设置项里的文字不再能框选复制——这与「随手一拖不再涂蓝半个界面」是同一个取舍。
- 对话记录的列表响应从会话数组变成带分页与统计的对象，并新增重命名命令。统计与搜索词同一批数据，但**不**受归档筛选影响——否则标签上的数字会随点它自己而变化，那不是统计，是回音。
- 对话记录这一族的 7 份 IPC fixture 现在两侧都解析（Rust 侧把它们编译进来并断言序列化输出与之逐字节相等），此前这一族一份 Rust 断言都没有。文档里的 fixture 覆盖统计随之修正。
- **设置页里选 Provider 从一个列表收成一个选择框**：原来每个 Provider 占一行，十四个 Provider 就是十四行长条，而这一屏要回答的只有「我在看哪一个」「新建运行会用哪一个」。现在选择框决定前者（切它只换下面表单编辑的对象，**不**顺手切换启用的 Provider），一个按钮决定后者（已是当前时显示为禁用的「当前使用中」）。名称重复时选项补一个最短的区别：网关不同就补主机名，名称、模型、网关全都一样时用 id 尾部 6 位当记号。
- **文档合并成一份。** 原来分散在项目说明、文档目录、架构决策记录目录、打包说明、两个子模块说明与一份变更日志里的内容，现在全部在这份 `README.md` 里；那些文件已经删除。代码注释里指向它们的路径已经改指到本文的相应小节。

**修复**

- **认证材料从未送达过 SSH 后端**：凭据输入的枚举只重命名了变体、没有改字段名，于是 Rust 期待下划线拼写，而共享契约一直发的是驼峰拼写。私钥与身份两条保存路径在 IPC 上本来就不可能成功。
- **每一个宿主请求都会被执行两次**：事件转发器由每次启动 sidecar 创建，而 supervisor 不转发退出事件，于是崩溃重启后挂上第二个转发器。它现在是窗口级单例。
- **Provider 的未分类流错误会把原始错误文本交出去**：SSE 帧解析失败、读取中途的连接错误与传输失败都走同一条分支，它没有经过脱敏，而网关把收到的请求回显在错误里是常见事——密钥会因此进入事件流、界面与审计记录。现在与模型目录那条路径一样先脱敏再截断。
- **离开「服务器」栏目时 Agent 面板会向右跳 12 像素。** 有一条只作用于非服务器栏目的平移规则，而它没有补偿任何东西：两种轨道布局把面板左边缘放在同一个位置，收起时它又是绝对定位。删掉那条规则之后，同一张卡片在任何栏目里都停在同一个位置。
- **`filesystem.edit` 的正文过去会进审计记录**：审计输入掩掉了一个字段，却没掩旧文与新文，而它们同样是任意文件内容。两者现在同等处理。
- 一个工具**结果**事件若声称自己还在运行，原本会被当作终态落库，在审计里留下「已结束但还在跑」的记录。
- **MCP 子进程曾经只靠析构回收**：宿主正常退出时没有显式关闭它们，而析构在 Windows 上强杀宿主时不会执行，于是关掉窗口可能留下一串第三方进程。退出路径现在和 sidecar 一样显式关闭它们。
- **桌面应用在 Windows 上根本起不来 sidecar**：资源目录返回的是带 verbatim 前缀的规范化路径，而 Node 解析不了它——拿到那种路径时它会去访问盘符本身并以 `EISDIR` 退出，agent 一行都没跑，界面上只剩「sidecar 已退出」。交给 Node 的入口路径现在会先去掉有普通等价形式的两种前缀；同一处补上了这次的教训：启动失败时把 sidecar 死前写下的 stderr 打进日志，并在启动前打印解析出来的命令。
- **设置页显示的 sidecar 入口仍然是那条带前缀的路径。** 上面那条修的是命令行，而报给界面的标签还是从清洗前的路径生成的，于是一行读起来像「Node 读不懂、也从来没有真正被执行过」的路径，而这一行的用处恰恰是让用户确认「到底跑了哪个文件」。标签现在与命令行取同一条路径，并有一个用例钉住它。
- **`tauri build` 在本仓库从未跑通过，原因是构建钩子的运行目录。** 见 [打包与分发](#打包与分发)：钩子里的脚本名假定当前目录是仓库根，而 Tauri 在应用目录下执行它。现在用能解析到 workspace 根的写法，Windows 上的完整打包路径已经跑通并产出 NSIS 安装程序与 `.msi`。

### 0.1.0 — 2026-09-12

首个带版本号、可对照与可回退的开发快照。

**包含**

- 桌面工作区：服务器管理、连接、概览健康快照、终端、远程文件、服务与日志、活动记录、Agent 对话历史。
- Agent 运行时（Node.js sidecar）：完整的 agent loop、内置工具、三层风险事实与唯一授权入口 Permission Engine、审批往返、取消与墙钟上限。
- OpenAI-compatible Provider（Chat Completions 与 Responses 两种方言）。
- 浏览器预览模式：可以在没有 Rust 的情况下调界面，原生能力明确不可用。
- 完整校验门禁 `pnpm check`：公开文档卫生、凭据扫描、跨语言契约、类型检查、单元测试、sidecar 冒烟与 Rust 侧的格式、静态检查与测试。

**尚未完成**

以[当前限制](#当前限制)一节为准，它是**所有**已知缺口的唯一来源。

## 维护这份文档的规则

- 这份 `README.md` 是**唯一**的文档文件。不要为某个模块、某条决定或某次发布新建第二个 markdown 文件——那正是这次合并要解决的问题。子模块的说明写进本文的相应小节。
- **新增一条决策**时，在[架构决策记录](#架构决策记录adr-00010015)一节末尾加一条，编号取下一个四位零填充数字，并保持既有的三栏结构：决定、为什么、代价。**已有记录不应被改写来掩盖变化**：决定变了就新增一条，并在旧记录的状态里标明被哪一条取代。
- 每条决策记录都必须写清**代价**。一个没有代价的决定，读者无法判断它是不是被想过。
- 文档中的相对链接必须指向仓库里真实存在的文件；跨小节引用请用本文的锚点。`node scripts/check-publication.mjs` 会检查措辞（禁止引用未发布的内部材料，公开标准的编号是例外），链接是否有效仍需人工确认。
- 功能范围变化时，**同时**更新[今天真正可用的能力](#今天真正可用的能力)、相应的边界小节，以及[当前限制](#当前限制)。缺口被补上时，把它从[当前限制](#当前限制)里**删掉**而不是标记为已完成——那一节只列现在做不到的事。
- 可验证性是硬要求：不要写「支持 X」而仓库里找不到实现或测试。

## 许可证

项目原创代码与文档以 [MIT License](LICENSE) 发布。第三方依赖和随仓库分发的字体仍受各自许可证约束，详见 [NOTICE](NOTICE)。