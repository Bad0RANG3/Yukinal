<div align="center">

<img src="./apps/desktop/src/assets/brand-mark.png" alt="Yukinal" width="96" height="96" />

# Yukinal

**面向 AI 原生的远程开发与基础设施工作区。**

在同一个运维工作区里检查和管理 SSH 服务器：受控的 AI Agent 负责执行，
加固的结构化 SSH 服务负责连接。

[English](README.md) · [**中文**](README.zh-CN.md)

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![Version](https://img.shields.io/badge/interface-2.1.0-informational.svg)](./packages/shared/src/version.ts)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-6f42c1.svg)](#构建与运行)
[![Python](https://img.shields.io/badge/python-3.11%2B-3776ab.svg)](./agent/README.md)
[![Node](https://img.shields.io/badge/node-%3E%3D24-339933.svg)](package.json)
[![Acceptance](https://img.shields.io/badge/Agent-实机验收通过-success.svg)](./docs/release.md)

</div>

---

> **发行版本：`2.1.0`。** 本版把上一版遗留的计划结算链路补齐：Agent 现在可以记录
> 证据、结论与决策摘要，可以跨 run 结算证据步、决策步与验证步，也可以在需要你拍板时
> 停在 `waiting_user` 而不是把任务判失败。五个免审批固定读改走独立的 `read_probe`
> 结构化通道。用真实服务器和真实 Provider 跑的一次完整流程（证据 → 计划 → 方案 →
> 你批准 → 执行 → 验证）现在能走到计划完成、任务完成。详细证据和安装包状态见
> [2.1.0 发行说明](./docs/releases/2.1.0.md)与[交付与发布](./docs/release.md)。
> Windows NSIS、macOS DMG 与 Debian Linux 安装包由 CI 构建；安装包仍未签名，macOS
> 应用未公证。

## 目录

- [项目简介](#项目简介)
- [工作区交互](#工作区交互)
- [架构](#架构)
- [仓库结构](#仓库结构)
- [功能状态](#功能状态)
- [构建与运行](#构建与运行)
- [配置](#配置)
- [常见连接错误](#常见连接错误)
- [安全模型](#安全模型)
- [文档](#文档)
- [参与贡献](#参与贡献)
- [许可证](#许可证)

## 项目简介

Yukinal 是面向少量 Linux 服务器运维的桌面工作区。用户提出目标；Agent 负责
拆解、调用被授权的工具、读取证据，并报告实际发生的结果——运维工程师随时
可以观察、批准或接管。

产品围绕三条用户路径构建：

| 路径 | 起点 | 成功画面 |
| --- | --- | --- |
| **目标驱动的 AI 运维** | 选中服务器并说明目标 | Agent 收集证据、执行被授权的结构化操作、验证远端状态，并展示每一步 |
| **运维可视化与接管** | 打开服务器工作区 | 带新鲜度标注的健康、服务、日志、文件与任务进度；需要时人工接管 |
| **本机与远端文件** | 从系统文件管理器拖入，或在工作区浏览 | 流式上传/下载，带进度、冲突处理与完整性核验 |

模型只负责提出计划和选择工具。**权限、审批与执行边界由程序代码强制执行，
绝不由提示词决定。**

## 工作区交互

选择服务器后，用 **文件 / 终端 / 日志 / 服务** 四个按钮直接切换主界面。
每次只显示一个工具视图，不再叠加标签栏或新增标签菜单；继续与 Agent 沟通时，
直接使用右侧浮动对话栏。工具切换保留终端会话、文件目录、日志筛选和对话草稿。
切换服务器后，当前工具跟随选中目标；原服务器的终端会话保留。

Token 用量在按钮旁展开，不挤动状态行；模型与设置菜单受视口边界约束，
支持 Escape 和点击外部关闭。状态保留与键盘操作见[工作区交互说明](./docs/workspace-tabs-spec.md)。

## 架构

```text
┌──────────────────────────────────────────────────────────────────┐
│  React 桌面界面（TypeScript + Vite）                               │
│  运行在 C++ WebView 宿主内                                          │
└───────────────▲──────────────────────────────────────────────────┘
                │ 宿主 IPC（JSON）
┌───────────────┴──────────────────────────────────────────────────┐
│  C++ 桌面宿主（native/desktop_host）                               │
│  窗口 · IPC 路由 · SQLite · 系统凭据库 · 审批                       │
└───────┬───────────────────────────────────────────────┬──────────┘
        │ NDJSON JSON-RPC 2.0（stdio 边车）              │ 回环 HTTP
┌───────▼──────────────────────────────┐        ┌────────▼──────────┐
│  Python Agent（agent/）               │        │  C++ SSH 服务      │
│  LangGraph · Tool Registry · 策略     │───────▶│  （native/src）    │
│  MCP 客户端 · ModelFactory            │  HTTP  │  仅监听 127.0.0.1 │
│  审计 · 审批 · 检查点                  │        │  结构化操作        │
└───────────────────────────────────────┘        └─────────┬─────────┘
                                                           │ SSH / SFTP / PTY
                                                    ┌──────▼──────┐
                                                    │  目标主机    │
                                                    └─────────────┘
```

可选的**控制平面**（`services/control-plane`，FastAPI + PostgreSQL）提供多租户
注册表与任务代理；节点通过签名请求和 WebSocket 通道主动连接它，不改变单机
桌面路径。

## 仓库结构

| 路径 | 说明 |
| --- | --- |
| [`apps/desktop/`](./apps/desktop) | React 桌面界面（产品 UI） |
| [`native/`](./native) | C++ SSH/SFTP/PTY 服务与 C++ 桌面宿主 |
| [`agent/`](./agent) | Python Agent 运行时与 `yukinal_ssh` 调用方 |
| [`services/control-plane/`](./services/control-plane) | 可选 FastAPI + PostgreSQL 控制平面 |
| [`packages/shared/`](./packages/shared) | 跨层 TypeScript 契约、schema 与 IPC fixture |
| [`packages/agent-sdk/`](./packages/agent-sdk)、[`packages/provider-sdk/`](./packages/provider-sdk) | Agent 传输与 Provider 目录的客户端 SDK |
| [`docs/`](./docs) | 产品、安全、重写与发布文档 |
| [`scripts/`](./scripts) | 构建、检查、打包与冒烟工具 |
| [`configs/`](./configs) | 示例服务配置 |

## 功能状态

| 领域 | 状态 |
| --- | --- |
| 独立 C++ SSH 服务（仅 `127.0.0.1`）与结构化操作 | ✅ 已实现 |
| 主机密钥校验（`known_hosts`、SHA-256 pin 或 Host CA + KRL） | ✅ 已实现 |
| 代理路由（HTTP/HTTPS CONNECT、SOCKS5/SOCKS5H）与 bypass 规则 | ✅ 已实现 |
| 有界 SFTP 列举/元数据/读取/写入/备份、守卫式原子替换 | ✅ 已实现 |
| C++ 桌面宿主：窗口、IPC 路由、SQLite、凭据、审批 | ✅ 已实现 · 含 sidecar 有界自动重启监督（`agent_status.restart`）、删除 MCP 时停止运行中进程，以及实时事件推送（`activity.created`、`server.*`、`terminal.output_lost`） |
| Python Agent：LangGraph 循环、Tool Registry、策略、脱敏审计 | ✅ 已实现 · 部分验收通过；真实多目标长时覆盖仍待补齐，见[交付与发布](./docs/release.md) |
| 长任务（goal 循环）：准入判定、中途复核、进度停滞收束 | ⚠️ 部分实现 · 准入以 host 声明的 `taskBudget` 为权威（`maxRunMs ≥ 10min` 或 `maxSteps ≥ 40`），无预算时回退提示词正则；`mode`/`toolAccess`/无工具回合先于任何预算否决；每次准入把 `sustainedGoal`/`source`/`declaredBudget` 写成 `agent.admitted` 审计事件。`goal` 模式默认**无**工具调用/模型轮次上限（只有总时长与输出字节）；逐轮复核解析失败会先做一次有界修复重试 |
| MCP 客户端（stdio + Streamable HTTP），默认需要审批 | ✅ 已实现 |
| 模型工厂：OpenAI 兼容、Anthropic、Gemini | ✅ 已实现 |
| 本地 RAG 知识索引与显式记忆 hybrid 检索 | ✅ 已实现 · 适用于小型 curated 语料；无神经 embedding 或 ANN 索引 |
| 记忆作用域与写入 | ⚠️ 部分实现 · 作用域仍是 `global`/`server`（无 tenant/user 维度）；模型可经 `memory.search`（只读）与 `memory.propose`（write，逐次人工审批后才入库、带结构化审批预览，`origin=model`）参与，`agent.memory.*` RPC 仍是操作员通道；`expiresAt` 过期与 `supersedes` 活跃版本均在 SQL 侧过滤（过期和被替代行保留供审计），同作用域完全相同内容自动原地去重，桌面记忆库界面支持有效期、来源标记与版本替代保存 |
| 检查点恢复（`turn:<digest>` 定位、跨重启续挂起审批） | ✅ 已实现 · 显式注入严格 msgpack 反序列化；Windows 目录与库文件按当前用户独占 DACL；`budget_binding` 不含逐轮推断的输出预算 |
| 持久审批：逐次调用指纹绑定与有效期 | ✅ 已实现 |
| 控制平面：租户、节点、任务、审批、策略、审计 | ✅ 已实现（SQLite 验证） · 含节点 WebSocket 心跳 `ping`/`pong` 超时探测与 `/v1/metrics/prometheus` 指标导出 |
| 任务/计划持久恢复 | ✅ 已验证保存、桌面重启恢复及 `plan.step_result` 步骤结算；2.1.0 另由真实目标上的完整闭环验证（证据 → 方案 → 审批 → 执行 → 验证 → 计划完成）；真实多目标长时覆盖仍待补齐 |
| 决策摘要与计划审批 | ✅ 已实现 · Agent 可记录证据/结论/决策摘要，发布方案后停在等待用户，选择方案即批准计划并由界面续跑 |
| 免审批固定读（`read_probe`） | ✅ 已实现 · 命令文本来自共享工具目录并编译进 C++，模型不提供 shell 文本 |
| Windows NSIS、macOS DMG、Debian Linux 安装包 | 稳定标签触发三平台构建、冒烟与发布 |
| 代码签名与 macOS 公证 | 未配置 |
| 平台 GUI、拖放及升级证据 | 见交付状态；Agent 验收独立记录 |

完整细节与证据见[架构与构建计划](./docs/architecture-and-build-plan.md)与[交付与发布](./docs/release.md)。

## 构建与运行

**前置条件**

- **Node.js ≥ 24** 与 **pnpm 11.8.0**（用 `corepack enable` 按 `package.json`
  固定版本）。
- **Python 3.11+**。
- **CMake 3.24+** 与 **C++20** 编译器（Windows 上为 MSVC/NMake 或 MinGW；
  macOS/Linux 上为系统编译器加 OpenSSL 头文件）。
- Windows 上还需完整的 **Perl** 发行版（含 `IPC::Cmd`，Strawberry Perl 可用）
  与 Visual Studio 2022 开发者命令行。

**1. 安装工作区依赖**

```bash
pnpm install --frozen-lockfile
```

**2. 配置 Python Agent**

```bash
python -m venv agent/.venv
# Windows
agent/.venv/Scripts/python.exe -m pip install -e "agent[agent,models]"
# macOS / Linux
agent/.venv/bin/python -m pip install -e "agent[agent,models]"
```

**3. 构建 C++ SSH 服务**

```bash
cmake -S native -B native/build -DCMAKE_BUILD_TYPE=Release
cmake --build native/build --config Release
ctest --test-dir native/build -C Release --output-on-failure
```

**4. 运行完整检查门禁**

```bash
pnpm check
```

`pnpm check` 会依次执行仓库卫生与密钥扫描、文档链接门禁、CTest 套件（C++
服务/宿主 + Python Agent）、控制平面测试（其 venv 存在时）、TypeScript
构建/类型检查/单测、桌面打包，以及 Python 边车冒烟。它也会守护自身配置：
显式设置的 `YUKINAL_SSH_CMAKE_BUILD_DIR` 若不是可运行的测试构建，门禁会带着
`cmake` 修复命令直接失败，而不是把原生套件跳过后仍打印绿。它**不包含** lint、
格式化或静态分析（没有 clang-format/clang-tidy、eslint、ruff 或 mypy），
风格与静态问题仍需人工审查。

**5. 运行桌面应用**

```bash
pnpm dev           # 一条命令：构建 C++ 宿主与 SSH 工作进程，起 Vite，拉起桌面
pnpm desktop:dev   # 仅浏览器 UI 预览（无 SQLite/SSH/PTY，不构建原生代码）
```

`pnpm dev` 会自己完成原生构建、启动 Vite（默认 1420，可用
`YUKINAL_DESKTOP_DEV_PORT` 改端口）、把 SSH 工作进程的路径与开关交给宿主，
宿主再拉起 Python 边车。远端调用始终由这条命令托管的进程承担，所以继承自
shell 的 `YUKINAL_SSH_API_TOKEN`、`YUKINAL_SSH_API_URL`、
`YUKINAL_SSH_SERVICE_CONFIG` 会被忽略并打印一行提示 —— 不需要为桌面准备
服务配置文件，也不需要手动对齐两份 token。`pnpm desktop:dev:cpp` 是同一
脚本的别名，保留给既有脚本调用。

`pnpm package` 会暂存带校验清单的分发布局。release-candidate workflow 会生成
Windows NSIS、macOS DMG 与 Debian Linux 安装包，并在托管 runner 上安装、启动、卸载和检查数据保留：

```bash
pnpm build:ssh-service
pnpm build:python-agent
pnpm package           # 暂存到 target/release/bundle/<name>/ 并写出清单
pnpm package -- --verify
```

## 配置

桌面不需要配置文件。服务器在应用里添加（名称、主机、端口、用户名、认证方式），
凭据由宿主保管；首次连接必须先做一次指纹核验：应用向目标探针取回它这次出示的
指纹，与你在服务器上看到的 `SHA256:` 指纹核对一致后确认信任。之后指纹一旦变化，
连接会被直接拒绝，而不是悄悄重连。核验入口就在首次向导的第二步。

每个目标声明**恰好一个**信任来源（`known_hosts_file`、
`host_key_fingerprint` 或 `host_certificate_authority`）与**恰好一个**认证
来源（私钥文件、私钥环境变量引用、密码环境变量引用，或
`{"type":"agent"}`）。JSON 中永不存储密码明文 —— 这条规则同样适用于应用
保管的凭据。

### 不带桌面宿主时

只有要把 C++ SSH 服务当成独立 HTTP 服务用（脚本、CI、其他客户端）时，才需要
[`configs/ssh-service.example.json`](./configs/ssh-service.example.json)：填入
白名单目标、选定信任来源，并把 `YUKINAL_SSH_API_TOKEN` 设为 32–256 位随机
URL-safe 值，然后：

```powershell
$env:YUKINAL_SSH_API_TOKEN = "<随机 URL-safe token>"
native/build/Release/yukinal-ssh-service.exe --config path/to/ssh-service.json
```

这条路径只提供 `ssh.service_status` 与 `ssh.disk_usage` 两条只读工具；完整工具集
由桌面宿主内嵌的 SSH 核心提供，不经过 HTTP，也不需要 token。

## 常见连接错误

| 错误 | 含义 | 下一步 |
| --- | --- | --- |
| `host_key_unverified` | 服务器密钥与已保存的 pin 或 `known_hosts` 条目不符 | 通过桌面信任流程确认服务器身份 |
| `authentication_failed` | 身份、密钥文件或口令错误 | 重新检查所选 SSH 身份与凭据引用 |
| `proxy_auth_required` | 代理拒绝了 Basic 凭据 | 把 `user:password` 存入引用的环境变量，不要写入 JSON |
| `proxy_connect_failed` | 代理未建立 CONNECT 隧道 | 检查代理主机/端口及是否允许 CONNECT 到 SSH 端口 |
| `timeout` / `resolver_busy` | DNS 或网络缓慢 | 检查可达性后重试；DNS 使用有界工作线程池 |

## 安全模型

- **默认拒绝。** 未配置的主机、用户、工具与操作一律拒绝。
- **目标白名单。** 模型从不提供主机；目标通过受控的 `target_id` 白名单解析。
- **强制校验主机密钥。** 未知或变更的密钥 fail-closed。
- **审批绑定到精确动作。** 指纹覆盖目标、工具与完整参数；参数变化需要重新
  审批，审批带有有效期与使用预算。
- **有界执行。** 超时、输出大小、并发与工具调用次数均有上限；敏感值在日志
  与审计中脱敏。
- **不可信输入。** Agent 与 MCP 返回视为数据，绝不作为可覆盖策略的指令。

完整边界见[安全与数据边界](./docs/security.md)。

## 文档

| 文档 | 阅读目的 |
| --- | --- |
| [文档索引](./docs/README.md) | 全部项目文档的导航 |
| [产品定义](./docs/product.md) | 目标用户、场景与功能取舍 |
| [安全与数据边界](./docs/security.md) | Agent 能做什么、何时需要批准 |
| [RAG 知识索引](./docs/rag-knowledge-index.md) | 本地语料的架构、CLI、评估与安全边界 |
| [RAG 示例语料](./examples/rag/README.md) | 用仓库夹具完成索引与评估的完整示例 |
| [原生桌面宿主](./docs/native-desktop-host.md) | 桌面宿主架构与 IPC |
| [SSH 加固](./docs/ssh-hardening-2026-11.md) | SSH/SFTP 信任边界与回归 |
| [目标架构与施工交接](./docs/architecture-and-build-plan.md) | 目标与发布验收条件 |
| [交付与发布](./docs/release.md) | 安装、构建、测试与打包状态 |
| [变更记录](./docs/changelog.md) | 版本历史 |
| [Agent README](./agent/README.md) | Agent 运行时、MCP 与 Provider 配置 |
| [SSH 服务 README](./native/README.md) | 构建与运行 C++ 服务 |
| [控制平面 README](./services/control-plane/README.md) | 多租户注册表与任务代理 |

## 参与贡献

欢迎贡献。请先阅读[贡献指南](./CONTRIBUTING.md)与[文档索引](./docs/README.md)，
再提交聚焦的 PR，让代码、测试与文档保持一致。安全问题请按
[安全策略](./SECURITY.md)私下报告。

## 许可证

Yukinal 采用 [MIT License](./LICENSE) 发布。第三方依赖与随附素材见
[NOTICE](./NOTICE)。
