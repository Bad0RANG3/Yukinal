<div align="center">

<img src="./apps/desktop/src/assets/brand-mark.png" alt="Yukinal" width="96" height="96" />

# Yukinal

**面向 AI 原生的远程开发与基础设施工作区。**

在同一个运维工作区里检查和管理 SSH 服务器：受控的 AI Agent 负责执行，
加固的结构化 SSH 服务负责连接。

[English](README.md) · [**中文**](README.zh-CN.md)

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![Version](https://img.shields.io/badge/interface-2.0.0--perview-informational.svg)](./packages/shared/src/version.ts)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-6f42c1.svg)](#构建与运行)
[![Python](https://img.shields.io/badge/python-3.11%2B-3776ab.svg)](./agent/README.md)
[![Node](https://img.shields.io/badge/node-%3E%3D24-339933.svg)](package.json)
[![Dependencies](https://img.shields.io/badge/status-rewrite%20in%20progress-orange.svg)](./docs/rewrite-cpp-python.md)

</div>

---

> **状态：** Yukinal 2.0 正在重写中：SSH 操作迁移到独立的 C++ 服务和
> Python Agent，React 界面仍是产品 UI。当前 app/package 接口版本为 `2.0.0-perview`。
> Rust/Tauri 宿主与 legacy Node agent
> 已从仓库删除，C++ WebView 宿主是唯一桌面宿主。打包（安装器、签名、公证）
> **尚未实现**。详见[重写状态](./docs/rewrite-cpp-python.md)与
> [交付与发布](./docs/release.md)。

## 目录

- [项目简介](#项目简介)
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
| C++ 桌面宿主：窗口、IPC 路由、SQLite、凭据、审批 | ✅ 已实现 |
| Python Agent：LangGraph 循环、Tool Registry、策略、脱敏审计 | ✅ 已实现 |
| MCP 客户端（stdio + Streamable HTTP），默认需要审批 | ✅ 已实现 |
| 模型工厂：OpenAI 兼容、Anthropic、Gemini | ✅ 已实现 |
| 持久审批：逐次调用指纹绑定与有效期 | ✅ 已实现 |
| 控制平面：租户、节点、任务、审批、策略、审计 | ✅ 已实现（SQLite 验证） |
| 任务/计划持久恢复 | 🚧 部分完成 |
| 安装器、签名、公证、跨平台验收 | ❌ 未实现 |

完整细节与证据见[重写方案](./docs/rewrite-cpp-python.md)与[交付与发布](./docs/release.md)。

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
pnpm desktop:dev:cpp   # C++ WebView 宿主 + Vite（真实桌面）
pnpm desktop:dev       # 仅浏览器 UI 预览（无 SQLite/SSH/PTY）
```

打包只暂存分发布局，**尚不生成安装器**：

```bash
pnpm build:ssh-service
pnpm build:python-agent
pnpm package           # 暂存到 target/release/bundle/<name>/ 并写出清单
pnpm package -- --verify
```

## 配置

复制 [`configs/ssh-service.example.json`](./configs/ssh-service.example.json)，
填入白名单目标，把主机密钥加入 `known_hosts`（或 pin 指纹 / 配置 Host CA），
并将 `YUKINAL_SSH_API_TOKEN` 设为 32–256 位随机 URL-safe 值。

每个目标声明**恰好一个**信任来源（`known_hosts_file`、
`host_key_fingerprint` 或 `host_certificate_authority`）与**恰好一个**认证
来源（私钥文件、私钥环境变量引用、密码环境变量引用，或
`{"type":"agent"}`）。JSON 中永不存储密码明文。

```powershell
$env:YUKINAL_SSH_API_TOKEN = "<随机 URL-safe token>"
native/build/Release/yukinal-ssh-service.exe --config path/to/ssh-service.json
```

要从 UI 驱动该服务，在第二个 shell 设置相同 token/URL 后启动桌面：

```powershell
$env:YUKINAL_SSH_API_TOKEN = "<同一个 token>"
$env:YUKINAL_SSH_API_URL   = "http://127.0.0.1:8765"
pnpm desktop:dev:cpp
```

桌面宿主只在服务已配置时，把 SSH token 转发给 Python Agent 边车。C++
SSH 服务需单独启动。

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
| [重写方案](./docs/rewrite-cpp-python.md) | C++/Python 重写阶段与接口 |
| [C++ 接管与 Rust 移除计划](./docs/rust-removal-plan.md) | 从零实现 C++ 宿主能力的指导 |
| [C++ 桌面宿主迁移](./docs/native-desktop-host.md) | 桌面宿主架构 |
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
