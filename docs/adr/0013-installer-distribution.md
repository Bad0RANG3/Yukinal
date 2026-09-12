# ADR 0013 安装包分发：随包分发 agent bundle，但使用用户自己的 Node

Status: Accepted
Date: 2026-09-12

## Context

仓库至今**没有安装包**。`tauri.conf.json` 里 `bundle.active` 为 `false`，`apps/desktop/src-tauri/icons/` 下那套完整图标没有任何地方引用，`scripts/` 里没有一个打包步骤，CI 也从不构建桌面应用（它只把 desktop crate 当作 workspace 成员 `cargo check`，并构建前端资源）。

ADR 0001 与 ADR 0008 都把一个未决问题留在这里，而且措辞是同一条要求：

- ADR 0008：*「发布安装包时必须提供一个受信任的 Node 可执行文件和一个绝对的 bundle 路径。当前实现默认使用 PATH 上的 `node`（Windows 上是 `node.exe`），这在开发机上可用，在成品里不成立——生产环境不应依赖一个可以被替换的 PATH。」*
- ADR 0001：*「发布成安装包时必须随应用分发受信任的 Node 运行时与 bundle，并给出绝对路径。」*

也就是说：要发行，必须解决两件事——**agent 代码怎么进安装包**，以及**Node 运行时从哪来**。本记录决定怎么做，并且如实记下它与上面两处措辞的冲突。

当前形状还有第二个问题：`apps/agent` 由 `tsc` 产出 `dist/index.js`，代码里 `import "@yukinal/shared"` 等 workspace 依赖，运行时需要 `node_modules` 在它上面。安装包里没有 `node_modules`，也没有仓库树可供向上查找。

## Decision

1. **启用 Tauri 打包**（`bundle.active: true`），显式列出目标平台与图标：Windows `nsis`/`msi`、macOS `app`/`dmg`、Linux `deb`/`rpm`/`appimage`；`bundle.icon` 指向仓库里已有的那套图标。
2. **agent 以单个自包含 ESM 文件随包分发。** 构建步骤先用 `tsc --noEmit` 保住类型检查，再用 esbuild 把 `src/index.ts` 连同 workspace 依赖打成 `dist/index.js`（`--platform=node`，仅在 Node 内建模块处保留外部依赖）。因此安装包里不需要 `node_modules`。
3. **不随包分发 Node 运行时。** 安装后的应用使用**用户系统上的 Node**，版本下限与仓库已声明的 `engines.node`、esbuild 的 `--target` 一致（当前 Node 24）。`YUKINAL_NODE` 仍然可以把这件事钉死到一个绝对路径。
4. **解析顺序（在 ADR 0008 的顺序里插入安装包一项）**：`YUKINAL_AGENT_COMMAND` → `YUKINAL_AGENT_ENTRY` → **`<resource_dir>/agent/index.js`** → dev 祖先查找。安装包路径排在 dev 之前是有意的：安装后的应用没有仓库树可向上走，而 dev 运行没有 staged 资源，两种顺序各自在真正重要的那个场景里给出正确答案，谁也遮蔽不了谁。
5. **bundle 在安装包里的位置是一个契约**：`bundle.resources` 把它映射到 `agent/index.js`，Rust 侧由 `SidecarConfig::packaged_entry()` 用同一个子路径拼出来，`scripts/check-packaging.mjs` 断言两边一致——单边改名只会被已安装的用户发现，CI 发现不了。
6. **缺少 Node 必须是可执行的错误。** 找不到 Node 时给出的消息要同时说明：这是一个不随附运行时的构建、需要的版本下限、以及两条出路（装 Node，或设置 `YUKINAL_NODE`）。**不做** `node --version` 预检——那会在每次启动时多起一个进程，而且仍然抓不到「装了但太旧」（那种情况表现为 stderr 上的解析错误）。
7. **本次不做代码签名、公证与自动更新。** 它们需要本项目并不持有的凭据（证书、Apple 开发者身份、更新服务器），因此只能作为未解决项写进文档，而不是假装已经完成。

## Consequences

**收益**

- 仓库第一次能产出可安装的东西，README 的「没有安装包」不再是事实。
- 安装后的应用启动的是**它自己带的那份 bundle**（绝对路径），不再依赖仓库树或 `node_modules`；ADR 0008 要求的「绝对的 bundle 路径」由此满足。
- 单文件 bundle 让「agent 到底跑的是哪份代码」变得可回答：一个文件、一个版本号。

**成本 — 以及与既有 ADR 的冲突**

- **这条决定与 ADR 0001 / ADR 0008 的明文要求冲突**：它们要求随包分发受信任的 Node 运行时。这里明确选择不分发，理由是随包分发意味着每个平台一份约 100 MB 的运行时、CI 里每个平台一次带完整性校验的下载、以及随之而来的许可与声明义务；而目标用户是开发者，他们机器上几乎必然已有 Node。这是一次**知情的取舍**，不是遗忘：ADR 0001 关于「PATH 上的 `node` 可以被替换」的论点被承认为**残留风险**（本应用不是用户自己机器的安全边界；需要钉死的用户可以用 `YUKINAL_NODE`）。
- **「装了但太旧」不会被专门识别**：Node 会因解析失败而退出，用户看到的是 stderr 里的一行语法错误加上退出码。下限写在文档里，日志保持可见；但不要声称我们能诊断版本过旧。
- **安装包未签名**：Windows SmartScreen 与 macOS Gatekeeper 会对首次启动发出警告或直接拦截。这是本次留下的最大可用性缺口，需要凭据才能解决。
- **打包不在本地门禁里**：`pnpm check` 保持与平台无关，不构建安装包。安装包由独立的打包工作流在 CI 上构建，因此在一个只装了部分平台工具链的开发机上，**本地产出的安装包无法被验证**。这一条必须写进文档，不能让「配置了」被读成「验证过了」。
- 多平台构建需要各自的工具链（Linux 需要 WebKitGTK 一系列依赖、macOS 需要 Apple 工具链），因此打包工作流是 CI 里最慢也最容易因环境而红的一步。

## Alternatives considered

- **随包分发受信任的 Node 运行时（ADR 0001/0008 的原始要求）。** 更安全、更自足，代价是体量、每平台下载与完整性校验、许可声明，以及 CI 里一条新的供应链。**不是永久否决**：当「用户机器上没有 Node」成为真实反馈时应当重新评估，届时本记录要被取代而不是被改写。
- **把 Node 静态链接进桌面二进制（或改用 `rusty_v8`/`deno_core` 之类的嵌入引擎）。** 前者在 Tauri + Rust 上没有可维护的路径；后者不是「打包 Node」，而是换掉整个 agent 运行时——一个远大于本次目标的赌注。
- **用 Node SEA / `pkg` / `deno compile` 把 agent 编译成自包含二进制。** 免掉用户对 Node 的依赖，但 SEA 仍是实验特性、需要按平台各出一份，而且会让用户在排查问题时无法用自己熟悉的 `node` 去跑同一份代码。本次否决，留待需要时重开。
- **维持 `bundle.active: false`。** 一个装不上的应用不是产品，而这正是 README 已经承认的限制。
- **只把前端作为静态站点分发（浏览器预览模式）。** 原生能力（SSH、本地凭据库、文件系统）才是这个应用的存在理由，见 [ADR 0005](0005-browser-preview-without-native-capabilities.md)。
