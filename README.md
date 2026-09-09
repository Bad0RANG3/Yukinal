# Yukinal

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Status: development](https://img.shields.io/badge/status-development-yellow.svg)](#当前状态)

Yukinal 是一个面向 AI 的远程开发与基础设施工作区。它把服务器连接、健康概览、终端、文件、日志、服务、活动记录和 Agent 操作放进同一个桌面工作区，让每一次远程动作都能被解释、审批和追踪。

## 当前状态

仓库当前处于可运行的开发版本（`0.0.0`），尚未发布安装包，也不承诺稳定的跨版本兼容性。核心桌面链路和 Agent 链路已经接通；发布构建、更多认证方式和 MCP 集成仍属于后续工作。

## 已实现能力

- Tauri 2 桌面壳：服务器列表、SSH 连接、健康概览、终端、远程文件、日志、服务和活动记录。
- Node.js Agent sidecar：流式文本、工具调用、工具结果、运行状态、停止和审批超时。
- 一个 OpenAI-compatible Provider 实现，支持 Chat Completions 和 Responses 两种流式协议方言，可通过 `baseUrl` 连接 OpenAI、OpenRouter、Ollama、LM Studio、vLLM 或兼容网关。
- 内置工具：`system.echo`；在桌面宿主可用时还包括 `server.info`、`docker.ps`、`docker.logs`、`docker.inspect`、`docker.restart`、`filesystem.read` 和 `filesystem.write`。
- Permission Engine：静态工具风险、命令风险和目标环境风险共同决定执行策略；Agent 可在用户明确委托的自动模式下自主批准并承担结果，策略禁止仍不可绕过。
- Provider 配置、系统凭据库保存 API key，以及 OpenCode、Codex 和 CC Switch 配置导入。
- 纯浏览器预览模式：无需服务器即可查看完整 UI，并明确标识需要 Tauri 原生能力的部分。

## 核心交互模型

```text
用户意图 → Agent 上下文 → 工具调用 → Permission Engine
                           ↓                  ↓
                       Rust 宿主执行 ← 审批 / 策略
                           ↓
                      结果、活动与验证
```

Yukinal 遵守三条产品约束：

1. 先展示信息和目标，再执行改变状态的操作。
2. Agent 的思考、工具调用、结果和审批都以可追踪事件呈现。
3. Permission Engine 是唯一执行入口；用户可选择“操作前询问”或把本次运行的批准判断委托给 Agent，模型输出不能伪造用户批准或绕过策略禁止。

## 架构

```text
┌────────────────────────────────────────────┐
│ React + Tauri WebView                      │
│ server list · overview · terminal · agent  │
└──────────────────────┬─────────────────────┘
                       │ 白名单 Tauri IPC
┌──────────────────────▼─────────────────────┐
│ Rust host / yukinal-core                   │
│ SSH · PTY · SFTP · collectors · SQLite     │
│ OS credentials · sidecar supervisor         │
└───────────────┬──────────────────┬──────────┘
                │ SSH / host tools │ stdio
                │                  │ NDJSON JSON-RPC
          远程服务器        ┌───────▼───────────────┐
                            │ Node.js Agent         │
                            │ loop · tools · policy │
                            │ provider · trace      │
                            └───────────────────────┘
```

跨层类型、Zod schema、事件名、IPC 命令和 sidecar 协议集中在 `packages/shared`。Rust 负责启动、停止、监督 sidecar，并把 `agent.stream` 映射为桌面事件；React 不直接创建进程，也不直接接触 SSH 会话或凭据。

## 安全与数据边界

- 服务器和 Provider 的非敏感配置保存在本地 SQLite；密码、私钥和 API key 通过系统凭据库保存，数据库只保存引用。
- Rust 在一次 Agent 运行开始时解析 Provider 凭据，并通过短生命周期协议参数注入 sidecar；凭据不写入日志或审计活动。
- 远程 SSH 默认使用 Trust On First Use：首次认证成功后记录主机指纹，后续指纹不匹配会拒绝连接。生产环境请在首次连接前核验主机指纹。
- Agent 不直接执行 SSH。需要远程能力时，Agent 通过受 schema 约束的 host tool 请求 Rust，Rust 再执行 SSH、SFTP 或 PTY 操作。
- 工具输出、审计输入和文件正文都有大小边界；敏感字段会在审计层脱敏。
- 浏览器预览没有 Tauri IPC、SQLite、SSH 或 sidecar，所有相关页面都会显示不可用原因，而不是伪造数据。

## 仓库结构

```text
apps/
  desktop/        Tauri 2 + React + Vite 前端与 Rust 命令层
  agent/          Node.js sidecar：loop、tools、permissions、providers
packages/
  shared/         跨层类型、schema、事件、IPC 与协议
  provider-sdk/   Provider 接口与模型侧工具名映射
  agent-sdk/      sidecar JSON-RPC typed client
crates/
  core/           sidecar、监督、宿主请求和共享核心
  ssh/            russh 连接、host key、PTY、SFTP
  terminal/       多会话 PTY 路由
  collector/      服务器健康数据采集与解析
  credentials/    系统凭据库抽象
  database/       SQLite 模型与 repository
  filesystem/     受约束的文件操作
docs/
  README.md       文档入口
  adr/            架构决策记录
scripts/          检查、构建和 sidecar smoke 工具
```

## 快速开始

前置环境：Node.js `>=24`、pnpm `11`、Rust stable（当前 workspace 的最低 Rust 版本为 `1.85`），以及 Tauri 在目标平台所需的系统依赖。

```bash
pnpm install
pnpm check
```

在浏览器中查看 UI 预览：

```bash
pnpm --filter @yukinal/desktop dev
```

浏览器预览使用 `http://127.0.0.1:1420/`，原生能力会显示为不可用。启动完整桌面壳：

```bash
pnpm --filter @yukinal/desktop tauri dev
```

单独开发 sidecar：

```bash
pnpm --filter @yukinal/agent dev
```

第一次使用桌面壳时，请在“设置 → Provider”配置兼容端点、模型和 API key；本地端点可以不填写 key。添加服务器时需要 SSH 用户名和密码或未加密私钥，并在首次连接时核验 host key。

## 检查与测试

根目录的 `pnpm check` 按顺序执行：发布文档卫生检查、共享库构建、TypeScript 类型检查、Agent bundle、全部单元测试、桌面 Vite 构建、sidecar stdio smoke，以及在 Rust 可用时执行 `fmt`、`clippy`、`cargo check` 和跨语言集成测试。

常用的分层命令：

```bash
pnpm typecheck
pnpm test
pnpm build:libs
pnpm smoke:sidecar
```

## 已知限制

- 尚未提供可发布的桌面安装包；`tauri.conf.json` 当前关闭 bundle 发布流程。
- Agent 目前只有 OpenAI-compatible Provider，没有 Anthropic、Gemini 等原生协议适配器。
- MCP 适配器尚未实现，sidecar capability 中的 `mcp` 当前为 `false`。
- SSH Agent 登录和带密码保护的私钥尚未实现；当前支持密码和未加密私钥。
- 终端、远程文件、日志、服务、服务器活动和本地数据库只能在 Tauri 桌面壳中使用。

## 文档

- [文档入口](docs/README.md)
- [架构决策记录](docs/adr/README.md)
- [Provider 实现说明](apps/agent/src/providers/README.md)
- [MCP 适配器边界](apps/agent/src/mcp/README.md)
- [第三方声明](NOTICE)

## 许可证

本项目原创代码和文档以 [MIT License](LICENSE) 发布。第三方依赖和随仓库分发的字体仍受各自许可证约束，详见 [NOTICE](NOTICE)。
