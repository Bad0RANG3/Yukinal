# Yukinal

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Status: development](https://img.shields.io/badge/status-development-yellow.svg)](#项目状态)

Yukinal 是一个面向 AI 协作的远程开发与基础设施桌面工作区。它将服务器连接、健康状态、终端、文件、服务、日志、活动记录和 Agent 操作放在同一界面中，并为每次远程操作保留可解释、可审批、可追溯的执行链路。

项目的重点不是让模型绕开现有运维流程，而是让模型在已有的目标环境、风险规则和用户授权边界内工作。

## 项目状态

Yukinal 目前是可运行的开发版本，版本号为 `0.0.0`。桌面端、Rust 宿主和 Node.js Agent sidecar 已有完整的开发链路；安装包发布、更多认证方式和 MCP 接入仍在后续范围内。因此，不应将当前接口视为跨版本稳定承诺。

## 当前能力

- 通过 Tauri 2 桌面端管理服务器，查看健康概览，并使用终端、远程文件、服务、日志和活动记录。
- 使用 Node.js Agent sidecar 处理流式模型输出、工具调用、审批等待、取消和运行状态。
- 通过一个 OpenAI-compatible Provider 对接 Chat Completions 或 Responses 风格的兼容端点。
- 在桌面宿主可用时，提供服务器信息、Docker 查询与重启、远程文件读写等宿主工具；`system.echo` 始终可用作基础诊断工具。
- 将工具风险、命令风险和目标环境风险交给统一的 Permission Engine 决策。
- 在本地 SQLite 保存非敏感配置，并将 API key、密码和私钥交给系统凭据库。
- 提供浏览器预览模式，以便开发 UI；预览模式会明确标识所有依赖 Tauri 原生能力的功能。

## 执行模型

```text
用户请求
   │
   ▼
Agent 上下文与模型调用 ──► 工具调用请求 ──► Permission Engine
                                                   │
                                                   ▼
                                        批准、拒绝或等待用户
                                                   │
                                                   ▼
                                      Rust 宿主执行受约束操作
                                                   │
                                                   ▼
                                    结果、活动记录与界面反馈
```

这条链路遵循三项原则：

1. 先明确目标和影响，再执行会改变状态的操作。
2. 模型输出、工具调用、审批与结果都以活动事件呈现。
3. Permission Engine 是唯一的授权决策入口。用户可以要求逐项确认，或只在开发、预发布环境委托 Agent 自动执行普通写入；本机、未知或生产环境，以及高危和 critical 操作始终需要人工确认，策略拒绝也始终不能被绕过。

## 架构概览

```text
┌─────────────────────────────────────────────┐
│ React + Tauri WebView                        │
│ 服务器 · 终端 · 文件 · 服务 · Agent 界面      │
└──────────────────────┬──────────────────────┘
                       │ 受限 Tauri IPC
┌──────────────────────▼──────────────────────┐
│ Rust host / yukinal-core                     │
│ SSH · PTY · SFTP · 采集 · SQLite · 凭据 · 监督 │
└──────────────┬─────────────────────┬─────────┘
               │ SSH 与宿主工具       │ stdio NDJSON JSON-RPC
               ▼                     ▼
        远程服务器              Node.js Agent sidecar
                              Provider · tools · policy
```

`packages/shared` 维护跨层类型、Zod schema、事件、IPC 命令和 sidecar 协议。React 不直接启动进程、访问 SSH 或读取凭据；Rust 负责原生资源和 sidecar 生命周期；Agent 通过 schema 约束的宿主工具请求远程能力。

## 安全与数据边界

- 服务器与 Provider 的非敏感配置写入本地 SQLite。密钥、密码和私钥只保存在系统凭据库，数据库仅保存引用。
- Rust 在一次 Agent 运行开始时解析 Provider 凭据，并以短生命周期的运行参数传给 sidecar。凭据不写入 Agent 配置、日志或活动记录。
- SSH 默认采用 Trust On First Use：首次成功认证后保存主机指纹，之后发现不匹配即拒绝连接。生产环境应在首次连接前独立核验指纹。
- Agent 不直接执行 SSH。所有需要访问服务器的操作都先经过 ToolRegistry 和 Permission Engine，再由 Rust 宿主执行。
- 工具输出、审计输入和文件正文都有大小限制；审计层会处理敏感字段。
- Agent 不可读取或写入常见凭据路径，例如 `.ssh`、`.kube`、`.env`、运行时 secret 目录与私钥文件；工具输出在回传模型、界面和审计前会清理可识别的密钥材料。
- Provider 的 API key 只经 OS 凭据库在运行时注入。自定义 header 仅允许非敏感的网关元数据，不能作为另一条持久化凭据通道。
- 浏览器预览模式不提供 SQLite、SSH、Tauri IPC 或 sidecar，并不会用虚构数据伪装这些能力。

## 仓库地图

```text
apps/
  desktop/        Tauri 2、React 和 Rust 命令层
  agent/          Node.js Agent sidecar、工具与权限逻辑
packages/
  shared/         跨层类型、schema、IPC、事件和协议
  provider-sdk/   Provider 抽象与模型侧工具名映射
  agent-sdk/      sidecar JSON-RPC 类型化客户端
crates/
  core/           sidecar、监督和宿主请求
  ssh/            russh 连接、主机密钥、PTY 和 SFTP
  terminal/       多会话 PTY 路由
  collector/      服务器健康数据采集与解析
  credentials/    系统凭据库抽象
  database/       SQLite 模型与 repository
  filesystem/     受约束的文件操作
docs/             架构与长期决策记录
scripts/          检查、构建和 sidecar smoke 工具
```

## 开始开发

需要 Node.js `>=24`、pnpm `11`、Rust `1.85` 或更高版本，以及目标平台运行 Tauri 所需的系统依赖。

```bash
pnpm install
pnpm check
```

启动浏览器中的 UI 预览：

```bash
pnpm --filter @yukinal/desktop dev
```

预览地址默认为 `http://127.0.0.1:1420/`。它适合界面开发，但原生功能会显示为不可用。要启动完整桌面应用，请运行：

```bash
pnpm --filter @yukinal/desktop tauri dev
```

单独运行 Agent sidecar：

```bash
pnpm --filter @yukinal/agent dev
```

首次使用桌面应用时，在“设置 → Provider”中填写兼容端点、模型和 API key。本地端点可不填 key。添加服务器时需要 SSH 用户名以及密码或未加密私钥，并应在第一次连接时核验主机指纹。

## 首次使用引导

首次打开会显示三步引导，也可从窗口顶部“使用引导”重新打开：

1. 选择或保存模型配置，点击“测试模型连接”。测试通过已保存的凭据发送一条简短消息，验证实际文本回复；可能产生少量模型费用，超时或失败可重试。
2. 添加或选择服务器，点击“连接并验证 SSH”。连接成功后继续；失败可编辑地址与认证信息后重试。首次连接采用 TOFU，应提前独立核验主机指纹。
3. 点击“填入首次排查任务”，将只读巡检草稿放入 Agent 面板，并设置“操作前询问”。用户检查后发送；已有草稿或正在运行的任务不会被覆盖。

“稍后设置”会记住跳过状态。浏览器预览可查看引导，但不能测试模型或连接 SSH。

## 验证命令

`pnpm check` 会依次检查公开文档卫生、构建共享库、执行 TypeScript 类型检查与测试、构建桌面端、运行 sidecar stdio smoke；若 Rust 工具链可用，还会执行格式化检查、Clippy、`cargo check` 和跨语言集成测试。

常用的分层命令：

```bash
pnpm typecheck
pnpm test
pnpm build:libs
pnpm smoke:sidecar
```

## 当前限制

- 尚未生成可发布的桌面安装包，Tauri bundle 流程仍被关闭。
- 当前只实现 OpenAI-compatible Provider，尚无 Anthropic、Gemini 等原生 Provider。
- MCP 尚未接入；sidecar handshake 中的 `mcp` capability 为 `false`。
- SSH 认证目前支持密码和未加密私钥，不支持 SSH Agent 或受密码保护的私钥。
- 终端、远程文件、日志、服务、服务器活动和本地数据库只能在 Tauri 桌面应用中使用。

## 文档

- [文档入口](docs/README.md)
- [架构决策记录](docs/adr/README.md)
- [Provider 边界](apps/agent/src/providers/README.md)
- [MCP 规划边界](apps/agent/src/mcp/README.md)
- [第三方声明](NOTICE)

## 许可证

项目原创代码和文档以 [MIT License](LICENSE) 发布。第三方依赖与随仓库分发的字体仍受各自许可证约束，详见 [NOTICE](NOTICE)。
