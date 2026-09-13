# 当前限制

这一节是**全部**已知缺口的唯一来源：每条都写明现在做不到什么、以及为什么（其中一部分是环境限制，比如「从未对真实 API 调用过」，那不是代码问题而是这台机器没有网络）。已经做完的部分见 [版本与发布历史](./changelog.md#版本与发布历史)；有意为之、不会被「补完」的安全边界在下一节。

- **安装包只在 Windows 上构建过。** `pnpm package` 在本机产出了 NSIS 安装程序与 WiX `.msi`（见 [打包与分发](./packaging.md#打包与分发)），但**没有真的安装并启动验证过**；macOS 的 `.app`/`.dmg` 与 Linux 的 `.deb`/`.rpm`/`.AppImage` 连构建都没有执行过。所以这件事的状态是「配置被复核过、Windows 路径跑通并产出成品」，不是「三个平台都验证过」。
- **没有代码签名、公证与自动更新。** 未签名的 Windows/macOS 包会触发系统自己的警告；没有更新通道，升级靠用户自己重新下载。
- **需要用户自己准备 Node.js ≥ 24。** 安装包不内含、不下载、不缓存任何运行时；缺 Node 时给出的是「启动 sidecar 失败」，而不是一条指名 `nodejs.org` 与 `YUKINAL_NODE` 的错误——版本探测是 ADR 0013 明确决定不做的事，代价如上。
- **`.deb` / `.rpm` 的 `Depends:` 是空的。** 打包器不会自动补 `libwebkit2gtk-4.1-0` / `libgtk-3-0`，依赖由发行版自己满足。包名逐发行版不同，在没装过的环境里无法核实，所以这一项是**记下来**，不是填一个猜的名字。
- **MCP 只支持 stdio 传输。** `http` 在类型层面就不存在，界面里也没有这个选项。
- **崩掉的 MCP 服务器不会被自动重启。** 唯一的重启路径是显式的启动命令；目录只会报告它已经死了。理由见 [外部工具（MCP）](./boundaries/mcp.md#边界外部工具mcp)。
- **MCP 工具一律按 `critical` 处理。** 服务器自己的风险注解不被采信，所以每个 MCP 工具在任何运行模式下都要逐项批准，会话授权也不能记住它。代价很直接：一个只读的外部工具也要点一次。
- **MCP 的 `trustLevel` 与 `allowedTools` 目前只被存储。** 还不存在「让用户看过工具描述再决定」的流程，所以 `trustLevel` 永远停在 `unreviewed`、`allowedTools` 永远是空表 —— 这正是每个 MCP 工具都保持 `critical` 的原因。
- **取消 MCP 调用不撤回它的副作用。** 取消让宿主不再等待，但 MCP 线上协议没有「取消一次 `tools/call`」，服务进程那边的调用可能继续跑完。
- **MCP 只在一个自带的 Node fixture 上验证过。** 真实的第三方 MCP 服务器没有被跑过，本环境没有网络也没有 `npx`。
- **55 份 IPC fixture 里有 27 份只有 TypeScript 一侧解析。** 只有 28 份被 Rust 用 `include_str!` 编译进来、并和新序列化的值比一次；剩下 27 份（`provider_*` 里除 `provider_delete` 之外的几份、`server_list` / `server_add` / `server_update` / `server_connect` / `server_disconnect` / `server_delete` / `server_snapshot`、`terminal_*`、`remote_file_*`、`agent_approval_respond`、`agent_run_stop` 与 MCP 那五份）没有任何 Rust 断言钉住 —— Rust 侧改了字段名，这个仓库里不会有任何检查变红。MCP 是其中之一，不是唯一的例外。
- **两套原生适配器从未对真实 API 调用过。** 翻译逻辑、流式状态、取消与错误路径都是照协议文档写的、用假响应测的 —— 写它们的环境没有网络。每个适配器的假设列在 [模型 Provider](./boundaries/provider.md#边界模型-provider) 里。
- **`openai-compatible` 在 chat 方言下的网络层异常没有过脱敏。** 其余所有 Provider 错误路径都经过 `safeProviderMessage()`，这一个是特例。
- **Anthropic 的 `anthropic-version` 不能配置。** 运行配置里没有 `apiVersion` 字段，Rust 因此无法传一个进来，适配器用它自己的默认值；自定义请求头同样没有入口。
- **SSH 证书认证不能在界面里配置。** `crates/ssh` 支持它（证书按 OpenSSH 的 `<私钥>-cert.pub` 约定定位，并且必须真的认证所提供的那把私钥），也有测试；但桌面只映射密码、私钥（含口令）与 ssh-agent，遇到证书会明确报「不支持的认证方式」而不是挑一个默认值 —— 证书要的是**文件路径**，而桌面把认证材料按引用存在系统凭据库里。
- **服务器出示 host 证书时会被拒绝。** 这个构建没有 host CA 信任存储——那是另一件事，不是用户证书认证。
- **ssh-agent 的失败无法再细分。** russh 0.63 没有公开 agent 的错误类型，所以「agent 拒绝签名」与「签名中途连接断开」在我们这一侧是同一个错误，也不会被当成可重试的传输失败。
- **没有多因素认证。** 服务器如果接受了公钥还要第二个因素，我们如实报告「被接受但未完成」，不会接着往下走。
- **`filesystem.edit` 的检查与写入之间仍有窗口。** 它比对读取时返回的内容摘要，并要求 `oldString` 恰好出现一次，否则拒绝；但 SFTP 没有事务 —— 摘要一致之后、写入之前，文件仍可能被别的进程改掉。
- **`delivery` / `resume` 的完整语义只在 sidecar 层可用。** `resume: false` 会登记这次请求而不执行、之后用同一个 `messageId` 才真正启动并沿用同一个 `runId`；`delivery: "sync"` 会等到终态并把结果放进响应。但 `duplicate` / `resumed` / `result` 三个字段没有过 IPC（没有消费方），面板从不发 `resume: false`，同步路径只在 router 层被测过。
- **多模态输入没有实现。** 消息内容的 part 形状为文件/图片/上下文预留了位置，但今天只有文本。
- **Agent 回复里的链接点不开，图片不加载。** 窗口只申请了 `core:default` 能力，没有 opener / shell。要让它可点，得先给桌面端加一个受限于 `http(s)` 的 opener 能力。
- **Markdown 支持的是子集，而且不是一个 CommonMark 实现。** HTML、setext 标题、缩进代码块、引用式链接与脚注都不认，遇到时按纯文本显示；解析器的用例钉住的是「没认出来的东西一个字都不能丢」，而不是规范一致性。
- **Agent 的流式文本与最终文本二选一。** 界面拿到最终 assistant 文本时会替换掉已流式累积的那一行，所以两处不一致时以最终文本为准。

### 有意为之的边界（不是待办）

下面两条看起来像限制，其实是安全模型的形状。它们不会被「补完」，改动它们等于改动授权模型本身（[ADR 0005](./adr.md#adr-0005permission-engine-是唯一的执行授权决策者)、[ADR 0009](./adr.md#adr-0009agent-权限采用显式的运行级委托)）。

- **危险动作必须逐项批准，且无法被「记住」。** `docker.restart` 声明为 `high` 风险，因此在任何环境下都不会被自动批准，也不会被会话授权覆盖；会话授权只覆盖非危险操作（[权限档位：dangerous](./risk-tiers/dangerous.md)）。这不是还没做「总是允许」，而是**拒绝把它做出来**：模型文本不能成为授权的来源，一次「以后都别问了」的委托正是把危险动作交回给模型。代价很具体：一次长任务里若需要重启容器，用户一定会被打断，也必须在看到具体命令之后再点一次。
- **浏览器预览里没有原生能力。** 在浏览器里打开 Web 前端只能看到界面骨架：终端、远程文件、日志、服务、活动、对话历史和本地数据库都需要 Tauri 桌面应用，因为它们全都走 Tauri 命令与原生侧（SSH、PTY、keychain、SQLite）。这是有意的：WebView 不该持有进程句柄，也不该在它的生命周期里决定一个进程的生死（[ADR 0001](./adr.md#adr-0001agent-runtime-作为独立-nodejs-sidecar由-rust-拥有其生命周期)、[ADR 0008](./adr.md#adr-0008rust-负责-sidecar-的启动握手监督与回收)）。代价是：预览只能用来调样式与布局，任何真实操作——包括所有手工验收——都必须在 Tauri 窗口里做。
