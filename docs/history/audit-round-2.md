# 第二轮审计：构建、可靠性与可恢复性

日期：2026-09-21。该轮只使用 Windows 本机的仓库依赖与回环浏览器预览；不创建虚拟机、不连接真实服务器，也不调用真实 Provider 或 MCP。

## 结论

重构后的主要执行边界仍可通过本地类型、单元/集成测试和 Rust 静态检查。第二轮的重点是确认拆分没有改变取消、超时、权限和审计事件的归属，而不是把本机结果误写成远程环境验收。

## 已验证的运行时不变量

| 不变量 | 证据位置 | 本轮结果 |
| --- | --- | --- |
| 单次 Agent run 有步骤和墙钟上限，停止会中断运行 | `apps/agent/src/runtime/agent-loop.ts`、Agent E2E 测试 | 通过局部 Agent 类型检查和 259 项测试 |
| 工具事件只能带脱敏投影 | `runtime/tool-event-emitter.ts` | 把调用/结果事件从编排循环移到唯一的投影出口，入参、摘要和错误仍先脱敏 |
| 任务恢复不能接受旧 run 的迟到事件 | `commands/event_projection.rs`、任务状态测试 | 保留现有 run/trace 栅栏，未改变事件顺序 |
| OAuth 回调、设备码、token source 的网络上限不被拆分破坏 | `commands/mcp/oauth/{callback,flow}.rs` 与根 `oauth.rs` | callback/body 上限和流程入口保持原有 owner；本轮不发送真实 OAuth 请求 |
| 样式拆分不改变构建资产或字体路径 | `src/styles.css` 的有序 `@import` 清单、桌面测试 | 字体路径改为相对模块路径，构建和字体契约测试通过 |

## 已落地的重构

| 原热点 | 新边界 | 结果 |
| --- | --- | --- |
| Agent loop 的小工具函数 | `runtime/agent-loop-helpers.ts` | 把预算、标题、提示词附件、计划判断和摘要留在无状态 helper 中 |
| Agent 工具事件 | `runtime/tool-event-emitter.ts` | `agent-loop.ts` 从约 1,357 行降至约 1,097 行；循环继续只负责执行次序和生命周期 |
| 桌面全局样式 | `styles/{app-shell,ui-foundation,providers,workspace-motion,agent-composer,agent-history,investigation}.css` | 入口成为有序清单；最大的单个样式模块约 1,272 行，字体资源路径随模块移动而修正 |
| 初始 JS 包 | `AppShell.tsx` 的低频页面 `lazy()` 边界 | 文件/日志/服务/活动/项目/任务/终端按页面加载；终端首次打开后仍保留挂载，避免切回页面时丢会话 |

## 构建发现与处置

第一次桌面 production build 报告单个初始 JS chunk 约 944 KB（未压缩），因为 `AppShell` 静态导入了所有页面，且终端把 xterm 一并带入首屏。本轮按访问边界拆分后，入口 JS 为约 400 KB，终端约 293 KB 单独按需加载；构建不再报告 500 KB 单 chunk 警告。

首次桌面测试也发现两项重构回归：Node 测试没有 Vite 的 `import.meta.env`，而字体契约仍只读取旧样式入口。修复后，preview fixture 在没有 Vite 环境时安全关闭，字体测试读取实际承载 `@font-face` 和 `--font-terminal` 的 `app-shell.css`。

## 本轮不可验证项

- 真实 SSH/PTY 的打开、交互、丢包、重连和远端命令后果。
- 真实 OAuth 供应商、真实模型端点、第三方 MCP 与校园网代理下的差异。
- 安装包安装后的系统权限、跨平台打包和长时间资源曲线。

这些都保留为外部验证项目，不因本地绿色测试而标为通过。
