# Yukinal

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![CI](https://github.com/Bad0RANG3/Yukinal/actions/workflows/check.yml/badge.svg)](https://github.com/Bad0RANG3/Yukinal/actions/workflows/check.yml)

> 面向独立开发者和小型技术团队的安全 AI 服务器运维工作台。

Yukinal 把 SSH 服务器状态、终端、远程文件、服务日志和 AI 排查放进一个桌面工作区。遇到故障时，你可以先收集有来源的只读证据，再让 Agent 提出变更计划；执行前查看目标和风险，批准后复核结果，并从活动记录追溯过程。

## 适合做什么

1. **看清问题：**连接服务器并核验主机身份，查看健康快照、服务与日志，让 Agent 给出带证据的诊断建议。
2. **控制变更：**查看 Agent 拟执行的工具、目标与输入，按风险批准或拒绝，执行后核对验证结果。
3. **保留退路：**对受限远程文件修改保留备份；出现问题时查看备份和恢复入口，恢复仍需单独授权。

这些是项目要完成真实环境验收的三条核心路径，详见[产品定义](./docs/product.md)。目前已有对应实现及大量本地、回环和模拟测试；干净安装后的三条完整路径仍需验证。

## 工作方式

```text
React 桌面界面 ──白名单 IPC──► Rust 宿主 ──SSH/SFTP/PTY──► 你的服务器
                            │
                            ├── SQLite、系统凭据库、活动记录
                            └── Node.js Agent sidecar ──HTTPS──► 模型 Provider
                                      │
                                      └── 提出工具调用，由权限引擎和宿主复核
```

模型、远端输出和 MCP 工具描述都作为不可信输入。Agent 不直接持有 SSH 连接或凭据；终端是用户主动操作的独立入口。审批、自动委托、停止和恢复的具体含义见[安全与数据边界](./docs/security.md)。

## 当前状态

代码版本为 `1.0.0`。打包流程会下载并校验固定版本的官方 Node.js，再将运行时随应用分发，最终用户不需要另装 Node.js。卸载始终保留数据库、设置和凭据；即使 NSIS 确认页的“删除应用数据”复选框被勾选，卸载 hook 也会阻止删除，并提示如何在卸载后手动清理。静默卸载和升级同样保留数据；隔离 fixture 还在 `/S` 模式预置测试用已勾选状态，验证卸载正常退出且 canary 原值不变。当前生成脚本检查和隔离的原生复选框测试均通过，但对应的 Windows CI 远端运行及当前安装包完整安装/卸载验收尚未取得。当前机器仍安装着 Node，尚未在整台机器未安装 Node 的干净环境复验；MSI、macOS 与 Linux 也仍需完成相应的真实安装验收。安装包未签名。真实 Provider、MCP 和远端维护组合只对实际测试过的场景负责。完整交付状态见[交付与发布](./docs/release.md)。

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
- [产品定义](./docs/product.md)：用户、三条核心任务和优先级
- [安全与数据边界](./docs/security.md)：审批、凭据、信任与限制
- [交付与发布](./docs/release.md)：验证方式、安装包状态和发布标准
- [变更记录](./docs/changelog.md) · [贡献指南](./CONTRIBUTING.md) · [安全策略](./SECURITY.md)

项目原创代码与文档采用 [MIT License](./LICENSE)；第三方依赖与随仓库分发的素材见 [NOTICE](./NOTICE)。
