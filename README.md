# Yukinal

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![CI](https://github.com/Bad0RANG3/Yukinal/actions/workflows/check.yml/badge.svg)](https://github.com/Bad0RANG3/Yukinal/actions/workflows/check.yml)
[![Release](https://img.shields.io/badge/release-1.0.0-blue.svg)](./docs/changelog.md)

Yukinal 是一个把「远程开发与基础设施运维」和「AI Agent」放进同一个桌面窗口的工作区。它把 SSH 连接、服务器健康快照、终端、远程文件、服务与日志、活动审计，以及一个可审批的 Agent 面板组织到一起。

它要解决的问题是：当人们想用模型操作自己的服务器时，常见的做法是直接把 shell 交给模型。这样权限不可解释、越权无法追溯、误操作无法预防。Yukinal 把模型放在「提议者」的位置上——模型只能提出工具调用请求，是否执行由 Permission Engine 决策，实际操作由 Rust 宿主在已解析的目标上完成，整个过程落成可回放的活动记录与执行审计。模型不会绕开现有的运维流程，它是在既有的目标、风险规则和用户授权边界内工作。

## 项目状态

Yukinal 当前版本为 `1.0.0`，这是首个稳定接口基线。版本号唯一来源是 `packages/shared/src/version.ts` 的 `APP_VERSION`；其余六个 `package.json`、`tauri.conf.json`、Cargo workspace 与 IPC fixture 都由 `packages/shared/src/version.test.ts` 钉在同一个值上，改一处而漏改其余会让 `pnpm check` 变红。

1.0.0 表示跨层接口与行为基线已经冻结；以下发布限制仍必须在安装或分发前阅读：

- **安装包已经在本机构建出来了，但没有签名。** `pnpm package` 在 Windows 上产出了 NSIS 安装程序与 WiX `.msi`（见 [打包与分发](./docs/packaging.md#打包与分发)），两者都在 `target/release/bundle/` 下。未签名意味着 Windows SmartScreen 与 macOS Gatekeeper 会对首次启动发出警告；也没有公证与自动更新。**macOS 与 Linux 的安装包从未构建过**（各自只能在各自平台上打）。
- **接口已进入 1.0 兼容基线。** Tauri IPC 命令、sidecar JSON-RPC 方法和跨层类型从 `1.0.0` 起按语义化版本维护；破坏性变更只在下一个主版本发布。
- **仍然有明确的能力空缺。** 多模态输入没有实现；SSH 证书认证在后端可用但界面不能配置；两套原生 Provider 适配器从未对真实 API 调用过（写它们的环境没有网络）。详见[当前限制](./docs/limitations.md#当前限制)。

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
- Agent 面板：流式文本、工具调用卡片、审批按钮、停止运行、模型选择、运行模式、批准方式与目标策略切换。Agent 的回复按 Markdown 渲染（标题、列表、代码块、表格、行内代码），解析器是仓库自己的、不注入 HTML，链接与图片因此不可点也不下载（`apps/desktop/src/lib/markdown/`、[Markdown 渲染](./docs/boundaries/markdown.md#agent-回复的-markdown-渲染)）。窗口里只有「你与 Agent 的对话正文」可以拖动选中，其余界面不参与选择。
- MCP 服务器：配置、启动、停止与删除（命令在 `commands/mcp.rs`，目录与工具名解析在 `crates/core/src/mcp/catalog.rs`，进程归宿主），工具目录由宿主把服务器起起来问出来，再经既有的 `host.mcp.catalog` 交给 sidecar —— 没有第二条执行通道。见 [外部工具（MCP）](./docs/boundaries/mcp.md#边界外部工具mcp)。

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

- 三种协议各一个适配器：**OpenAI-compatible**（Chat Completions 与 Responses 两种请求方言）、**Anthropic Messages**、**Gemini `generateContent`**。三者都支持模型目录、SSE 文本增量、工具调用增量、取消、超时和安全的错误摘要；两个原生适配器还会解析 token 统计与推理增量（`apps/agent/src/providers/`，细节见 [模型 Provider](./docs/boundaries/provider.md#边界模型-provider)）。
- 协议选择是配置里的一列（`provider_configs.kind`），`buildProvider()` 是唯一按 Provider 身份分支的地方；`wireApi` 只对 OpenAI-compatible 有意义，其余两种带上它会被拒绝，而不是被忽略。
- 凭据链路：SQLite 只保存 `credentialRef`，密钥存操作系统凭据库，Rust 在每次运行开始时解析并以一次性参数交给 sidecar，不写配置、不写日志。

**浏览器预览模式**

- 执行 `pnpm --filter @yukinal/desktop dev` 可以在普通浏览器里开发界面。预览模式不提供 SQLite、SSH、Tauri IPC 或 sidecar；调用原生命令会抛出「请在 Yukinal 桌面应用中执行此操作」，界面会显示「预览模式」标记，不会用假数据伪装这些能力（`apps/desktop/src/lib/ipc.ts`）。

## 快速开始

前置条件：Node.js `>= 24`、pnpm `11.8.0`、Rust `1.85` 以上（工具链 `stable`，含 `rustfmt` 与 `clippy`），以及 Tauri 2 在当前平台的系统依赖。完整清单见 [开始开发](./docs/development.md#开始开发)。

```bash
pnpm install
pnpm check                            # 完整门禁：文档卫生、凭据扫描、跨层契约、类型检查、单元测试、冒烟与 Rust 侧检查
pnpm --filter @yukinal/desktop tauri dev   # 完整桌面应用（需要 Rust）
pnpm desktop:dev                      # 浏览器预览：只能调界面，原生能力不可用
pnpm package                          # 打安装包（当前只有 Windows 路径被跑通过）
```

首次使用要按顺序做三件事：先在「设置 ▸ Provider」里填一个协议与密钥，再在「服务器」里添加一台服务器并连接，之后概览、终端、文件、服务与日志才可用。逐步说明见 [首次使用引导](./docs/development.md#首次使用引导)。

## 文档

这份 `README.md` 只介绍项目：它是什么、现在能做什么、怎么跑起来。**规则、约束与决策不写在这里**，各自成篇放在 `docs/` 下 —— 它们的读者是改代码的人，改动它们要动的是契约而不是说明。

| 文档 | 写什么 | 什么时候读 |
| --- | --- | --- |
| [文档索引](./docs/README.md) | `docs/` 的阅读顺序与文档治理规则 | 想知道某件事写在哪 |
| [架构总览](./docs/architecture.md) | 一次运行的完整链路、分层边界、仓库地图 | 想知道某个东西住在哪一层 |
| [执行与授权模型](./docs/execution-model.md) | 三层风险事实如何合成一个决策、票据与授权的共享规则 | 要改权限、审批或目标解析 |
| [权限档位：read](./docs/risk-tiers/read.md) · [write](./docs/risk-tiers/write.md) · [dangerous](./docs/risk-tiers/dangerous.md) | 三档各自的完整规则：构成、策略表取值、谁能批准、什么会拒绝它 | 要判断一次调用会走哪条路 |
| [安全与数据边界](./docs/security.md) | 凭据、主机指纹、Agent 能碰什么、各项上限 | 要碰凭据、文件路径或上限 |
| [边界：模型 Provider](./docs/boundaries/provider.md) | 三种协议的适配器与它们的假设 | 要加 Provider 或改协议翻译 |
| [边界：外部工具（MCP）](./docs/boundaries/mcp.md) | 接入形状、十条约束、已知未验证处 | 要碰 MCP 的进程、目录或调用 |
| [Agent 回复的 Markdown 渲染](./docs/boundaries/markdown.md) | 为什么自己写解析器，以及它的子集边界 | 要改渲染或加语法 |
| [当前限制与有意为之的边界](./docs/limitations.md) | **全部**已知缺口，以及不会被补完的安全边界 | 想确认某件事是不是还没做 |
| [开始开发](./docs/development.md) | 前置条件、各种启动方式、验证命令、首次使用引导 | 第一次跑起来 |
| [打包与分发](./docs/packaging.md) | 安装包怎么产出、装了什么、签名与平台现状 | 要出安装包 |
| [架构决策记录](./docs/adr.md) | ADR 0001–0015，代码注释里的 `ADR NNNN` 指向这里 | 想知道某个形状是为什么 |
| [版本与发布历史](./docs/changelog.md) | 未发布与已发布的变化、每个版本的包含与缺口 | 要写发布说明 |

| [贡献指南](./CONTRIBUTING.md) | 开发环境、提交规范、审查流程与验证要求 | 准备提交代码或文档 |
| [安全策略](./SECURITY.md) | 漏洞私下报告渠道、支持版本与响应流程 | 发现潜在安全问题时 |
| [行为准则](./CODE_OF_CONDUCT.md) | 社区互动与执行标准 | 参与讨论或协作时 |

## 许可证

项目原创代码与文档以 [MIT License](./LICENSE) 发布。第三方依赖和随仓库分发的字体仍受各自许可证约束，详见 [NOTICE](./NOTICE)。
