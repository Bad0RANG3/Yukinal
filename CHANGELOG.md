# 变更日志

本文件按「已标记的版本」记录用户可见的变化，格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)。

版本号遵循[语义化版本](https://semver.org/lang/zh-CN/)，并且是**唯一**的：`package.json`（六个）、Cargo workspace、`tauri.conf.json` 和 IPC fixture 里的版本由 `packages/shared/src/version.test.ts` 钉在一起，改一处而不改其余会让 `pnpm check` 变红。

`1.0.0` 之前不承诺向后兼容：Tauri IPC 命令、sidecar JSON-RPC 方法和跨层类型都可能变化，破坏性变化会写在这一节里。

## [未发布]

### 新增

- **原生 Anthropic Messages 与 Gemini `generateContent` Provider**（[ADR 0011](docs/adr/0011-native-anthropic-and-gemini-providers.md)）：两套协议各自一个适配器，系统提示、工具调用与终止原因的翻译留在适配器内，agent loop 不按 Provider 身份分支。两者都能解析 `usage` 与推理增量——OpenAI-compatible 那条路径两者都不产出。
- **`filesystem.edit`**：带内容摘要校验的读-改-写。`filesystem.read` 返回内容的 SHA-256，`filesystem.edit` 要求它仍然一致、且 `oldString` 恰好出现一次，否则拒绝而不是覆盖。竞争窗口被收窄，但没有关闭（SFTP 不提供事务）。
- **ssh-agent 与带口令的私钥**（迁移 5 的 `identities.passphrase_ref`）：桌面端第三种认证方式与口令输入框；口令材料只进 OS keychain，SQLite 只存引用。
- **主机指纹的手动核验入口**（[ADR 0012](docs/adr/0012-host-key-trust-model.md)）：探针（只报告服务器出示的指纹，不写入任何东西）、信任、遗忘、状态四项；校验失败同时给出**已钉住的**和**出示的**两个指纹，而不是一个占位字符串。
- **sidecar 崩溃后的有界自动恢复**（[ADR 0010](docs/adr/0010-bounded-sidecar-recovery.md)）：最多 5 次、退避到 30 秒、60 秒后视为健康；`agentStatus.restart` 报告第几次、是否已放弃。用户按下的停止永远不会被回答成一次重启。
- **MCP stdio 客户端**（Rust 侧，`crates/core/src/mcp`）：握手与版本协商、`tools/list`、`tools/call`、每次调用的超时、有界 stderr/诊断尾部、退出记录、每个服务端单实例。`http` 按类型拒绝；崩掉的服务端不会被自动重启。
- **MCP 接入**（[ADR 0014](docs/adr/0014-mcp-integration.md)）：服务器的新增/删除/启动/停止进了设置页，工具目录经新增的 `host.mcp.catalog` 交给 sidecar，外部工具以 `mcp.<服务器>.<工具>` 注册进同一个 registry 并一律声明为 `critical` —— 服务器自己写的 `readOnlyHint` 之类不被采信，因为那是被授权方对自己的一面之词。`host.tool.execute` 按 `mcp.` 前缀分流，与 `docker.*` / `filesystem.*` 共用同一条路径与取消令牌；看列表不会启动任何进程。
- **安装包**（[ADR 0013](docs/adr/0013-installer-distribution.md)）：`bundle.active` 打开并列出目标平台，agent 以 esbuild 打成单文件资源 `<resource_dir>/agent/index.js`，Node.js 由用户自备（≥ 24）。
- **`delivery` / `resume` / `policyId` 的真实语义**：`delivery: "sync"` 会等到运行结束并返回结果，`resume: false` 只登记不执行，未知的 `policyId` 报错而**不会**回落到环境默认策略。

### 变更

- IPC 契约若干处与语义一起收紧：`agent_run_start` 的响应新增 `started`、可选 `result`；`provider_save_openai` 变成带 `kind` 的 `provider_save`；`filesystem.read` 的输出新增 `revision`；`agentStatus` 新增可选的 `restart`。`1.0.0` 之前不承诺兼容。
- 脱敏逻辑从 `sidecar` 私有提升为 crate 级能力（`crates/core/src/redact.rs`），因为 MCP 客户端也要把陌生子进程的输出交给用户。顺带修掉三处真实漏检：`Authorization: Bearer <token>` 的顺序问题（token 曾原样留存）、`secret=` 未被当作敏感标记、以及显式方案名后短凭据的漏检。
- `apps/agent` 的项目关闭了 emit（`noEmit` 写在 `tsconfig.json` 里，`outDir`/`rootDir` 一并去掉）：它曾经把逐文件的 tsc 产物盖在 esbuild 的单文件 bundle 上，让「类型检查通过」之后打包契约反而变红。修在 `package.json` 的脚本上不够——手写一条 `tsc -p tsconfig.json` 会绕过脚本，而这件事真的又发生过一次。
- `initialize` 里的 `capabilities.mcp` 从字面量 `false` 改成读注册表：它报告的是「此刻注册表里有没有 MCP 工具」，不是「这个构建支不支持 MCP」。握手时它通常仍为 `false`，因为工具目录只能在 stdio 通道起来**之后**去取 —— 那正是实话，`agent.list_tools` 与 `system.describe.toolCount` 才是实时视图。

### 修复

- **认证材料从未送达过 SSH 后端**：`AuthenticationInput` 用了变体级 `rename_all`，只重命名变体、不改字段名，于是 Rust 期待 `private_key_pem` / `identity_id`，而共享契约一直发的是 `privateKeyPem` / `identityId`。私钥与 identity 两条保存路径在 IPC 上本来就不可能成功。
- **每一个 `host.*` 请求都会被执行两次**：事件转发器由每次 `start_sidecar` 创建，而 supervisor 不转发 `Exited` 事件，于是崩溃重启后挂上第二个转发器。它现在是窗口级单例。
- **Provider 的未分类流错误会把原始错误文本交出去**：SSE 帧解析失败、读取中途的连接错误与传输失败都走同一条 `catch`，它没有经过脱敏，而网关把收到的请求回显在错误里是常见事——`api_key: sk-…` 会因此进入事件流、界面与审计记录。现在与模型目录那条路径一样先脱敏再截断。
- **`filesystem.edit` 的正文过去会进审计记录**：审计输入掩掉了 `content`，却没掩 `oldString`/`newString`，而它们同样是任意文件内容。两者现在与 `content` 同等处理。
- 一个工具**结果**事件若声称 `status: "running"`，原本会被当作终态落库，在审计里留下「已结束但还在跑」的记录。
- **MCP 子进程曾经只靠 `kill_on_drop` 回收**：宿主正常退出时没有调用 `McpSupervisor::shutdown_all()`，而 `kill_on_drop` 是一个 `Drop` 实现 —— 在 Windows 上强杀宿主时不会执行，于是关掉窗口可能留下一串第三方进程。退出路径现在和 sidecar 一样显式关闭它们，一台服务器关不掉也不影响其余几台。
- **桌面应用在 Windows 上根本起不来 sidecar**：`resource_dir()` 返回的是规范化路径，带 `\\?\` 前缀，而 Node 解析不了它 —— 拿到 `\\?\C:\...\target\debug\agent\index.js` 时它会去 `lstat('C:')`、以 `EISDIR` 退出，agent 一行都没跑，界面上只剩「agent sidecar exited」。交给 Node 的入口路径现在过一遍 `SidecarConfig::for_command_line`，它只去掉有普通等价形式的两种 verbatim 前缀（盘符与 UNC）；`\\?\Volume{…}` 保持原样，因为去掉前缀指的是另一条路径。同一处补上了这次的教训：启动失败时把 sidecar 死前写下的 stderr 打进日志，并在启动前打印解析出来的命令 —— 否则一次握手期间的崩溃在日志里只有「exited」几个字，看日志的人无从下手。

## [0.1.0] - 2026-09-12

首个带版本号、可对照与可回退的开发快照。

### 包含

- 桌面工作区：服务器管理、连接、概览健康快照、终端、远程文件、服务与日志、活动记录、Agent 对话历史。
- Agent 运行时（Node.js sidecar）：完整的 agent loop、内置工具、三层风险事实与唯一授权入口 Permission Engine、审批往返、取消与墙钟上限。
- OpenAI-compatible Provider（Chat Completions 与 Responses 两种方言）。
- 浏览器预览模式：`pnpm desktop:dev` 可以在没有 Rust 的情况下调界面，原生能力明确不可用。
- 完整校验门禁 `pnpm check`：公开文档卫生、凭据扫描、跨语言契约、类型检查、单元测试、sidecar 冒烟与 Rust 侧的 fmt/clippy/test。

### 尚未完成

`README.md` 的[当前限制](README.md#当前限制)一节列出**所有**已知缺口，并且是该列表的唯一来源；这一版仍未完成的部分以那一节为准。

[未发布]: https://github.com/Bad0RANG3/Yukinal/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Bad0RANG3/Yukinal/releases/tag/v0.1.0
