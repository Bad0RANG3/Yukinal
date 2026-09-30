# Yukinal

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![CI](https://github.com/Bad0RANG3/Yukinal/actions/workflows/check.yml/badge.svg)](https://github.com/Bad0RANG3/Yukinal/actions/workflows/check.yml)

> 以 AI 执行为主、以可视化工作区观察和接管的服务器运维桌面应用。

目标使用方式是：你提出需求，Agent 通过已配置的内建工具、MCP 生态和受控 SSH 命令能力，对添加的服务器调查、配置、执行和验证。运维工作区用来查看服务器状态、AI 的实际操作与失败原因，并在需要时人工接管；文件页要成为本机与远端的双向拖放、预览和传输入口。当前代码只实现了其中一部分，施工与验收基准见[目标架构与施工交接](./docs/architecture-and-build-plan.md)。

## 适合做什么

1. **让 AI 执行目标：**选定服务器，提出想法或故障目标，由 Agent 发现工具、调用命令与配置能力、验证结果；超出委托范围时再批准。
2. **让工程师看清现场：**在工作区查看健康、服务、日志、文件和任务状态，识别旧数据、失败与待核对操作。
3. **直接传输文件：**在 Windows、macOS、Linux 上拖放或选择本机/远端文件，预览、上传、下载并处理冲突与进度。

这是目标产品的三条核心路径，详见[产品定义](./docs/product.md)。工作树已有通用 Agent 命令、双向文件传输和工作区观测的第一轮实现，但目标驱动真实远端操作及三平台安装后的完整业务路径仍待验收；已有本地与回环测试不代表这些目标已交付。

## 工作方式

```text
React 运维与文件工作区 ──白名单 IPC──► Rust 宿主 ──SSH/SFTP/PTY──► 你的服务器
                            │
                            ├── SQLite、系统凭据库、活动与传输记录
                            ├── 本机文件选择/拖放 ↔ 流式 SFTP 传输（目标能力）
                            └── Node.js Agent sidecar ──HTTPS──► 模型 Provider
                                      │
                                      └── 内建/MCP/受控命令工具，由权限引擎和宿主复核
```

模型、远端输出和 MCP 工具描述都作为不可信输入。Agent 不直接持有 SSH 连接或凭据；终端是用户主动操作的独立入口。图示说明各组件的权限与数据边界；工作树中的第一轮实现仍需通过真实服务器与三平台安装版验收。审批、自动委托、停止和恢复的具体含义见[安全与数据边界](./docs/security.md)。

## 当前状态

代码版本为 `1.0.0`。打包流程会下载并校验固定版本的官方 Node.js，再将运行时随应用分发，最终用户不需要另装 Node.js。卸载始终保留数据库、设置和凭据；NSIS 的勾选删除、静默卸载和更新路径都经过保护。PR #3 的三平台发布包 run `36674149301` 已验证 Windows MSI/NSIS 生命周期与数据保留、Ubuntu `.deb` 安装和 macOS DMG 挂载/复制，并在三个 runner 上完成随包 Agent 握手。Windows runner 安装了系统 Node，所以这不等于“完全没有 Node 的干净机器”验收；包 smoke 也不等于真实 SSH/Provider/MCP 运维流程或完整桌面 UI 验收。安装包尚未签名/公证。完整交付证据与剩余事项见[交付与发布](./docs/release.md)。

## 开始开发

需要 Node.js ≥ 24、pnpm 11.8.0、Rust 1.90+ 和当前平台的 Tauri 2 系统依赖。在仓库根目录运行：

```bash
pnpm install --frozen-lockfile
pnpm check
pnpm --filter @yukinal/desktop tauri dev
```

只预览 React 界面可运行 `pnpm desktop:dev`。浏览器预览不提供 SSH、SQLite、PTY 或系统凭据库。构建安装包运行 `pnpm package`；输出目录为 `target/release/bundle/`。

仓库结构：`apps/desktop` 是界面与 Tauri 外壳，`apps/agent` 是模型与工具运行时，`packages/shared` 管理跨层契约，`crates/` 持有 SSH、文件、数据库和其他宿主能力。

## 文档与参与

- [文档索引](./docs/README.md)：阅读顺序和维护规则
- [产品定义](./docs/product.md)：用户、三条核心路径和优先级
- [目标架构与施工交接](./docs/architecture-and-build-plan.md)：目标调用链、实施顺序与逐项验收
- [安全与数据边界](./docs/security.md)：审批、凭据、信任与限制
- [交付与发布](./docs/release.md)：验证方式、安装包状态和发布标准
- [变更记录](./docs/changelog.md) · [贡献指南](./CONTRIBUTING.md) · [安全策略](./SECURITY.md)

项目原创代码与文档采用 [MIT License](./LICENSE)；第三方依赖与随仓库分发的素材见 [NOTICE](./NOTICE)。
