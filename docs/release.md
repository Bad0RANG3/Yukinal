# 交付与发布

本页的表格和“远端 CI 快照”记录已经取得的实现与安装包证据；后续 Windows 构建记录按日期保存旧产物与环境证据。新的产品目标与尚需完成的业务验收以[目标架构与施工交接](./architecture-and-build-plan.md)为准。历史段落中“待首次远端验收”或旧“三条路径”描述的是当时状态，不能作为新目标已完成的证明。

## 当前状态

仓库版本为 `1.0.0`，用于标记当前跨层接口基线。三平台安装包已在 CI runner 完成限定范围的安装/复制冒烟；这不代表用户干净环境、完整桌面 GUI 或真实 Provider、MCP 和远端维护场景都已验收。版本号以 `packages/shared/src/version.ts` 为源，并由测试检查各清单与 IPC fixture。

| 项目 | 已有证据 | 发布前仍需完成 |
| --- | --- | --- |
| 代码门禁 | 2026-09-30 当前工作树使用 Node 26.5.1 执行 `pnpm check` 全绿，包含发布卫生、secret scan、JS 类型/单测/构建、sidecar 冒烟、Rustfmt、Clippy、cargo check、workspace Rust 测试与 Rust-to-sidecar 集成测试；Agent 278 项、桌面 Rust 库 206 项 | 保持每个目标平台持续通过；门禁通过不等于新 A/O/F/R 目标已有真实远端和安装后验收 |
| 新产品目标 | 已有部分内建/MCP 工具、状态概览、SFTP 浏览和文本预览；未发布工作树增加了受控 `server.exec`、主目标输入/明确目标选择与可选受限委托、任务执行时间线、不可自动重试的远端结果未知状态、Overview 近期动态、本机队列与远端目录拖放、本机预览直接上传、流式传输管理器和进度 UI。项目 SSH/SFTP 回环覆盖桌面 Host RPC 命令成功/未授权拒绝/重复调用/未知结果，以及 `TransferManager` 安全发布 | 真实 OpenSSH 扩展协商、真实目标机、覆盖发布、传输崩溃恢复、三平台安装版系统文件拖入/拖出、目标驱动真实 Provider/MCP/SSH 端到端路径及 GUI 均未验收，按 A/O/F/R 条件继续施工；覆盖上传继续 fail-closed |
| JavaScript 依赖审计 | 2026-09-30 本机 `pnpm audit` 检查完整锁文件，生产及开发依赖均未发现已知漏洞；主分支/PR advisory job 与 tag/手动发布包 job 均执行完整锁文件审计 | 确认 GitHub 下一次 advisory 和发布运行成功；出现上游新公告时重新评估 |
| Rust 依赖审计 | 2026-09-30 本机以 `cargo-audit 0.22.2` 扫描 628 项依赖，未发现已知漏洞；仍有 2 条允许提示：[`glib 0.18.5` 的 `VariantStrIter` unsound 告警](https://rustsec.org/advisories/RUSTSEC-2024-0429)及 [`proc-macro-error 1.0.4`](https://rustsec.org/advisories/RUSTSEC-2024-0370) 未维护。升级前曾有 7 条提示；已把 yanked 的 `wnaf 0.14.0` 更新至 `0.14.1`，Tauri 升到 `2.12.0` 后移除了 `urlpattern` 带来的 5 条 `unic-*` 告警，并获得上游 ACL 跨来源权限修复，代价是 Rust MSRV 提高到 1.90。Yukinal 源码没有直接引用 `glib::VariantStrIter` 或 `proc-macro-error`；`glib` 和 `proc-macro-error` 都来自 Linux Tauri/GTK3 图形依赖树，后者仅经 `glib-macros 0.18.5` 与 `gtk3-macros 0.18.2` 引入，`--target x86_64-pc-windows-msvc` 下无该依赖输出。为避开旧 RustSec Action 的 Node 20 运行时并收紧权限，advisory job 现固定安装 `cargo-audit 0.22.2` 后直接执行 `cargo audit`，仅保留 `contents: read`；主分支、PR 和 `v*` tag 都运行审计，tag 上跳过重复的三平台 `pnpm check`（由发布包工作流执行） | GitHub 下一次 main/PR/tag 运行确认新 CLI 和 tag 触发配置。Linux GTK 链中的两条提示仍无法在当前 Tauri 2.x 依赖边界内安全升级；GTK4/WebKitGTK 6 迁移仍是[未合并的 Tauri 3.0 变更](https://github.com/tauri-apps/tauri/pull/14684)，届时重新评估，不静默屏蔽告警 |
| Windows 包 | 本轮 `pnpm package` 从 dirty 工作树重新生成 unsigned MSI（47,427,584 bytes，SHA-256 `90ed4998ed364421107eeee0a5cad66663992d4815d9211fbe64cbb9ea062c69`）和 NSIS（33,268,000 bytes，SHA-256 `29a5d6ccc054a1c91c9b17f18bb3665edf510302f7e3be25e30348487ddc9386`），release manifest 与生成的 NSIS 卸载策略检查通过；本轮未在系统中安装这两个新产物。本机此前已完成 NSIS 隔离安装/卸载及数据保留验收；另完成 NSIS checkbox、MSI 管理员映像载荷及原生窗口 smoke。PR #2 提交 `a3dbb71` 的 GitHub package run [36667041840](https://github.com/Bad0RANG3/Yukinal/actions/runs/36667041840) 与三平台/依赖门禁 [36667041930](https://github.com/Bad0RANG3/Yukinal/actions/runs/36667041930) 全部通过；PR #3 的手动三平台发布包 run [36674149301](https://github.com/Bad0RANG3/Yukinal/actions/runs/36674149301) 也通过 NSIS 安装/更新/启动/卸载与数据保留、MSI 安装/启动/关闭/卸载及 manifest 校验 | 完全没有 Node.js 的干净 Windows 系统复验；新工作树产物的安装后 GUI/A/O/F 操作验收、代码签名、公证及跨版本回滚策略 |
| macOS / Linux 包 | PR #3 手动三平台发布包 run [36674149301](https://github.com/Bad0RANG3/Yukinal/actions/runs/36674149301) 成功：Ubuntu 22.04 对 `.deb` 实际安装、检查 dpkg 安装状态、验证包内资源并用 bundled Node 完成 Agent 握手，随后 purge；macOS runner 对 DMG 实际挂载、将 `Yukinal.app` 复制到临时 Applications、检查资源并完成 bundled Agent 握手。两平台 release manifest 校验通过，安装包及 manifest 均上传为 [Linux artifact](https://github.com/Bad0RANG3/Yukinal/actions/runs/36674149301/artifacts/11080610014) 和 [macOS artifact](https://github.com/Bad0RANG3/Yukinal/actions/runs/36674149301/artifacts/11079725899) | 这些是隔离的 CI runner 安装/复制 smoke，不覆盖完整桌面 GUI 实际操作、签名/公证、升级/重启或干净用户主机复验 |
| Node.js 运行时 | 固定版本的官方运行时按 SHA-256 校验；2026-09-30 本机 `node scripts/check-node-runtime-pins.mjs` 将六个平台的 24.21.0 归档 pin 全部与官方 SHASUMS 清单核对一致；打包时会重复该校验。针对上述 MSI SHA-256 对应的管理员映像，2026-09-30 两次 `smoke-installed-windows.ps1` 均确认受限 PATH 找不到 Node、实际 sidecar 使用与载荷一致的 bundled `node.exe`、Agent 握手成功；正常关窗和强制终止父进程后 sidecar 均退出。当前 NSIS SHA-256 也通过同一进程级检查：受限 PATH 找不到 Node、sidecar 哈希与随包 runtime 一致、Agent 握手成功并在应用正常退出后停止。PR #3 的 Linux `.deb` 和 macOS DMG 安装后 smoke 也由包内 Node 启动 Agent 并完成协议握手 | Windows 系统未安装 Node 的干净机复验仍缺；Linux/macOS runner 预装 Node，冒烟 harness 也由 runner Node 启动，因此这些结果只证明应用选用包内 runtime，不证明系统完全没有 Node 时可用 |
| 真实环境与协议回归 | 本机 acceptance fixture 通过真实 AgentLoop 与 Host RPC client 覆盖只读诊断、审批变更、验证失败，以及文件恢复被拒绝或批准后的 revision/原内容核验；新增 native Host RPC + 真实临时 SQLite 测试，记录 evidence 后关闭并重建 `AppState`，再经 fetch RPC 读回内容与宿主绑定的 run ID；自动回归还覆盖 Agent 停止/运行时限、工具超时、Host RPC 断连时拒绝所有挂起请求并取消在途工具、持续巡检任务重启恢复及数据库迁移失败恢复。2026-09-30 的 `desktop_host_rpc_executes_server_command_once_and_persists_unknown_outcome` 通过桌面 Host RPC 分发和真实 `AppState` SSH 客户端连到临时 russh SSH 服务，验证未授权不发命令、获授权执行并记预算、重复请求不二次执行、丢失退出状态后标记未知且阻止逻辑重放。`crates/ssh/tests/sftp.rs` 新增服务层上传用例：`TransferManager` 经测试适配器调用生产 `RusshBackend` 的流式 SFTP/硬链接发布，核验发布字节、目标竞争不覆盖及 staging 清理。既有 SSH/SFTP 回环还覆盖固定主机密钥认证、exec stdout/stderr/非零退出码/输出上限/超时/取消、目录列表、限长与完整读取、独占写入/备份/删除，以及 `RemoteFileService` 的 revision 守卫与故障注入。SFTP 不提供 CAS，最后一次内容复核后仍有竞争窗口。可分别运行 `cargo test -p yukinal-ssh --test command` 和 `cargo test -p yukinal-ssh --test sftp`。2026-09-30 本机前置条件只读核验：当前进程未配置 live-test opt-in、OpenAI key/base/model、Anthropic/Gemini key；Windows OpenSSH client 存在，但未找到 sshd.exe 或 sshd 服务；本轮未发起任何外部 Provider/SSH 调用 | 用可重置服务器、真实 Provider 和 MCP 端点跑新目标 A/O/F 路径及错误矩阵；回环测试走真实 SSH/SFTP wire protocol，但不证明 OpenSSH/其他真实服务器兼容性、真实 Provider/AgentLoop/sidecar stdio 往返、真实目标机行为、Tauri transfer adapter、完整 UI 工作流或实际目标机故障恢复 |
| 发布可信度 | 产物当前未签名 | 确定证书、渠道、升级与回退方案后再公开分发 |

远端 CI 快照（2026-09-30 06:06 UTC）：PR #3 当前提交 `af4ab99` 的三平台 `pnpm check`、依赖 advisory audit 和 Windows installer lifecycle 均通过；完整三平台发布包 run [36674149301](https://github.com/Bad0RANG3/Yukinal/actions/runs/36674149301) 以 success 完成。该 run 在 Ubuntu 22.04 实际安装 `.deb`、完成 bundled Agent 握手并 purge；在 macOS runner 挂载 DMG、复制应用并完成 bundled Agent 握手；在 Windows runner 完成 NSIS uninstall policy、MSI 安装/启动/退出/卸载、NSIS 安装/更新/启动/卸载及数据保留验证。三个 runner 均成功校验 release manifest 并上传产物：[Linux](https://github.com/Bad0RANG3/Yukinal/actions/runs/36674149301/artifacts/11080610014)、[macOS](https://github.com/Bad0RANG3/Yukinal/actions/runs/36674149301/artifacts/11079725899)、[Windows](https://github.com/Bad0RANG3/Yukinal/actions/runs/36674149301/artifacts/11080177012)。PR #3 的三平台 `pnpm check` 与依赖审计见 [36674148457](https://github.com/Bad0RANG3/Yukinal/actions/runs/36674148457)，Windows 安装器门禁见 [36674148471](https://github.com/Bad0RANG3/Yukinal/actions/runs/36674148471)。Linux/macOS 的 Agent 冒烟使用安装包内 Node runtime，但 GitHub runner 自身预装 Node，不能替代完全无系统 Node 的干净环境验收。之前 package run `36661983739` 暴露 `pnpm release:manifest -- --verify` 多传裸 `--` 的问题，已修复并由后续成功 run 覆盖。工作流使用 Node 24 兼容的固定版 Actions，并以只读权限运行固定版 `cargo-audit` CLI；最近一次 main 历史检查 [36443924584](https://github.com/Bad0RANG3/Yukinal/actions/runs/36443924584) 曾被旧 RustSec Action 的 check-run 权限拒绝。涉及的官方 Action 版本为 [checkout v7.0.1](https://github.com/actions/checkout/releases/tag/v7.0.1)、[setup-node v7.0.0](https://github.com/actions/setup-node/releases/tag/v7.0.0)、[pnpm/action-setup v6.0.9](https://github.com/pnpm/action-setup/releases/tag/v6.0.9)、[upload-artifact v7.0.0](https://github.com/actions/upload-artifact/releases/tag/v7.0.0)。

本机发行文件复核结果：NSIS 与 MSI 的 Authenticode 均为 `NotSigned`；随包 Windows `node.exe` 为 24.21.0，Authenticode 状态 `Valid`、签名者 `OpenJS Foundation`。运行时签名与安装包签名是不同证据，不能把前者当作安装包可信发布的替代。

卸载风险记录（2026-09-29 起）：一次 Tauri 2.12 NSIS 本机卸载曾删除 `%LOCALAPPDATA%\dev.yukinal.workspace`，确认页和复选框状态没有留档，原触发状态仍无法追溯。Tauri 生成的 `Section Uninstall` 原生支持勾选“Delete the application data”后删除 `%APPDATA%\${BUNDLEID}` 与 `%LOCALAPPDATA%\${BUNDLEID}`；这与 [产品规则](./product.md) 冲突，因此当前 NSIS `NSIS_HOOK_PREUNINSTALL` 会在交互勾选时显示数据保留说明，并在继续卸载前无条件清零 `DeleteAppDataCheckboxState`。静默卸载不弹说明、仍然保留数据；Tauri 的 `/UPDATE` guard 也禁删。隔离 UI fixture 直接操作生成的 native checkbox，验证默认未选中时保留、勾选时显示提示且仍保留、更新模式下仍保留；另用测试专用 `/TESTCHECKED` 预置已勾选状态，以 `/S` 静默卸载并验证正常退出和数据保留；所有 canary 位于 `target/release/nsis-checkbox-ui-smoke/<run-id>/`，不触碰默认用户数据路径。真实 checkbox UI、NSIS `/UPDATE` 就地更新和安装/启动/卸载保留 canary 均已由 Windows package run `36667041840` 实测通过。

这次真实复验还暴露并修复了 smoke 自身的安全缺陷：脚本原先给 NSIS `/D=` 和 `_?=` 路径参数加引号，导致测试安装器忽略隔离路径并使用默认目录。按 [NSIS 官方命令行说明](https://nsis.sourceforge.io/Docs/Chapter3.html)，两种参数都必须置于末尾且不能带引号。脚本现改用未加引号的参数、预检默认安装目录是否已占用，并在安装后核对注册表路径确实指向本次临时目录；卸载后只允许剩余本次运行的 `uninstall.exe`，待进程退出后才精确清理它。新的回归测试先红后绿，完整本机复验通过。

Rust 依赖边界复核（2026-09-30）：`cargo tree -i glib@0.18.5 --target x86_64-unknown-linux-gnu` 将 `glib 0.18.5` 追溯到 Tauri 的 GTK 0.18 / WebKitGTK 栈；同一查询对 Windows 目标没有依赖输出，仓库源码搜索也没有直接调用 `glib::VariantStrIter`。`cargo tree -i proc-macro-error --target all` 显示 `proc-macro-error 1.0.4` 仅由 `glib-macros 0.18.5` 和 `gtk3-macros 0.18.2` 引入，均位于这条 Linux GTK/Tauri 图形依赖链；`--target x86_64-pc-windows-msvc` 无该依赖输出。RustSec 将 `glib` 修复版本标为 [`glib >= 0.20.0`](https://rustsec.org/advisories/RUSTSEC-2024-0429.html)，而当前 Tauri runtime 仍声明 GTK 0.18（[上游清单](https://github.com/tauri-apps/tauri/blob/dev/crates/tauri-runtime-wry/Cargo.toml)）；对应 GTK4 / WebKitGTK 6 迁移仍是[开放的 Tauri 3.0 PR](https://github.com/tauri-apps/tauri/pull/14684)。因此当前保留并披露这两条 Linux 传递依赖告警，不对单个 crate 作不兼容覆盖，也不将其加入忽略列表；上游图形依赖栈发布兼容迁移后重新审计。

此前 5 条 `unic-*` 未维护告警均来自 `urlpattern 0.3.0 -> tauri-utils 2.9.3`；现已锁定 Tauri `2.12.0` / `tauri-utils 2.10.0`，该版本改用 `urlpattern 0.6`，2026-09-29 与 2026-09-30 两次本机 `cargo audit` 均确认这些告警已消失（[Tauri 发行记录](https://github.com/tauri-apps/tauri/releases/tag/tauri-v2.12.0)）。因此项目 MSRV 已提高至 Rust 1.90；本轮以精确的 `1.90.0` 工具链通过 `fmt`、`check`、Clippy 和 workspace 全量测试，默认工具链下 `pnpm check` 也全绿；GitHub 的 Linux 全门禁现固定使用 Rust 1.90.0，其他平台继续测试 stable。这不影响终端用户安装包无需 Node.js 的承诺，也不会消除 Linux GTK 链中的 `glib` / `proc-macro-error` 两条告警。

固定运行时为 Node.js 24.21.0；截至 2026-09-29，Node.js 官方将 24.x 列为 LTS，支持计划延续至 2028 年 4 月底。六个平台归档 hash 以[官方 24.21.0 SHASUMS](https://nodejs.org/dist/v24.21.0/SHASUMS256.txt) 为准；每次 `pnpm package` 会执行 `node scripts/check-node-runtime-pins.mjs` 核对整组 pin，而不只核对当前构建平台。[官方归档页](https://nodejs.org/en/download/archive/v24.21.0)列出各平台二进制和签名校验清单。

### 本机 Windows 构建记录

2026-09-30 载荷复验：对 SHA-256 为 `56646e0c9157cb4e37bb034df54b46f69d6254357763448a1c148629ec397e96` 的 MSI 管理员映像目录 `target/release/msi-current-package-smoke-20260929/PFiles/Yukinal/`，重跑 `scripts/smoke-installed-windows.ps1` 的正常关窗和 `-ForceTerminateAfterReady` 两种模式。两次均确认受限 PATH 找不到 Node，sidecar 使用的 `node.exe` 与映像文件哈希相同，Agent 握手成功，主进程退出后 sidecar 随之退出。日志位于 `target/release/installer-smoke/package-19c8a6ff56d44e43a509e21b41ec2e7a.*.log` 和 `target/release/installer-smoke/package-6d8f55b403b54369b30ed3df564653d4.*.log`。这是管理员映像载荷验证，不是 MSI 安装/卸载生命周期，也不证明整台操作系统未安装 Node。

本节按时间保留验收证据；安装包哈希、ProductCode 和 `-PlanOnly` 输出都只对应其标注的快照。阅读当前结论时，以日期最新的工作树复核为准；历史安装器通过记录只适用于对应的旧哈希。

以下是本轮脏工作区生成的本地验收产物，不是签名版或可公开发布的 release。构建操作系统为 Windows x64；打包脚本先通过严格模式的 sidecar 冒烟，再由 Tauri 生成两个安装器。使用 7-Zip 检查 NSIS 内容，并读取 MSI 的 `File` 表，确认都带有 `runtime/node.exe`（93,580,104 字节）、`runtime/LICENSE`（160,555 字节）及 Agent bundle。

此前版本的 NSIS 曾以 per-user 模式在本机真实安装，按测试专用 `YUKINAL_DATA_DIR`、不含 Node 的子进程 `PATH`，通过 `pwsh -NoProfile -File scripts/smoke-installed-windows.ps1` 验证 sidecar 的实际运行文件与安装内容哈希相同、Agent 握手成功、正常关窗后应用及 sidecar 退出；该循环重复 7 次，7 次均通过。在当时重建的 NSIS 隔离目录 `target/release/nsis-post-wnaf-20260929/` 完成静默 per-user 安装和启动/关闭冒烟，再运行该目录自己的 `uninstall.exe /S`；确认 PATH 隔离有效、随包运行时哈希一致、Agent 握手成功、应用与 sidecar 正常退出、安装目录和卸载注册消失，卸载前已存在的 `%LOCALAPPDATA%\dev.yukinal.workspace` 用户数据目录仍在。第一次未收紧握手条件的手工尝试曾有一次 20 秒关闭等待超时；按就绪信号运行后没有复现，故记为一次未复现异常，不据此改代码。以上 NSIS 安装生命周期记录针对 Tauri 2.12 重建前的包，不冒充当前安装包的验收。当前机器装有 Node.js；这里只能证明应用子进程 PATH 不提供 Node 时会选用随包运行时，尚不等于完全未安装 Node 的干净系统验收。

之前构建的 MSI 曾通过 `msiexec /a` 提取管理员映像至 `target/release/msi-smoke-post-wnaf-20260929/`，使用 `-InstallDir` 运行 PATH 隔离、sidecar SHA-256、Agent 握手及进程退出检查；正常关窗 1 次、握手后强制终止父进程 3 次均通过。这验证的是 MSI 文件载荷行为，不是 MSI 安装生命周期。当前 Tauri 2.12 MSI 先后提取至忽略目录 `target/release/msi-tauri-2.12-smoke-20260929/` 和本轮重建产物目录 `target/release/msi-current-package-smoke-20260929/PFiles/Yukinal/`。针对本轮清单中 SHA-256 为 `56646e0c9157cb4e37bb034df54b46f69d6254357763448a1c148629ec397e96` 的 MSI 载荷，使用 `scripts/smoke-installed-windows.ps1` 分别完成 1 次正常关窗、1 次握手后强制终止父进程测试；两次均确认受限 PATH 找不到 Node、应用实际使用的随包 `node.exe` 哈希匹配、Agent 握手成功且 sidecar 退出。该验证仍不等价于实际 MSI 安装/卸载、升级或回滚；本机没有注册或安装 MSI。

为真实 MSI 生命周期新增了 `scripts/smoke-msi-install-windows.ps1`。它会从 MSI 读取 ProductCode、版本和安装路径；非管理员、检测到 Node.js、已有 Yukinal 安装、数据目录已存在或安装目录占用时会在改动系统前拒绝运行。仅在一次性 Windows x64 管理员测试机（无 Node.js、无现存 Yukinal、干净用户配置文件）运行 `pwsh -NoProfile -File scripts/smoke-msi-install-windows.ps1`；脚本安装这一个 MSI、调用上面的进程级启动/关闭冒烟、检查卸载前创建的数据保护标记仍保留，再只卸载本次 MSI ProductCode 并清理该标记，日志留在 `target/release/msi-install-smoke/`。`-PlanOnly` 只读解析包并报告主机阻碍。Windows 打包 workflow 现也会在 GitHub 一次性管理员 runner 上运行同一生命周期脚本；GitHub 将标准 `windows-latest` 描述为每个 job 新建的 Windows VM，并以管理员权限运行（[runner 文档](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)），当前 Windows 2025 镜像也预装 Node.js（[镜像软件清单](https://github.com/actions/runner-images/blob/main/images/windows/Windows2025-Readme.md)）。CI 因此明确使用 `-AllowInstalledNodeForPathIsolationSmoke`，而应用启动冒烟仍把 PATH 限制为 Windows 系统目录，所以可验收真实 MSI 安装/卸载和“应用进程 PATH 无 Node”行为，但不能冒充“系统未安装 Node”的验收，且首次远端通过记录尚未取得。2026-09-30 对最终 MSI `F57F401147D1FD2D8AB6917D2C0BB0235A8CB2D72309D2EFEFCC0F99839C1E03`（ProductCode `{3BA5349F-35D5-43CF-A008-9A70F6015E2F}`）执行 `-PlanOnly`；它是 per-machine（`ALLUSERS=1`），预检因当前 PowerShell 非管理员、主机存在 Node.js、Yukinal 用户数据根目录已存在而安全拒绝。预检未发现已有 MSI 安装，`C:\Program Files\Yukinal` 不存在；未进行安装或修改系统。当前工作机没有 WSL 发行版、Windows Sandbox 可执行文件或 Docker/Podman；虽有 Hyper-V PowerShell 模块，但当前账户无权运行 `Get-VM`/`Get-VMHost`，因此无法确认或使用可回滚的 Windows 虚拟机，也没有尝试实际 MSI 安装。

NSIS 当前构建的真实生命周期脚本为 `scripts/smoke-nsis-install-windows.ps1`。它只在两个 Yukinal 数据目录、产品/卸载注册、测试安装目录和默认 `%LOCALAPPDATA%\Yukinal` 安装目录均未占用时运行；静默安装到 `target/release/nsis-install-smoke/<run-id>/`，核对产品注册指向该临时目录，调用 `scripts/smoke-installed-windows.ps1` 验证 bundled Node、Agent 握手和关闭，再创建两处 canary 并静默卸载，断言数据未被删除或改写。执行 `-PlanOnly` 只报告环境阻碍。卸载后若仅剩当前 smoke 的 `uninstall.exe`，脚本会在确认进程已退出后精确清理该文件和空测试目录。图形确认页由隔离 fixture 操作真实 checkbox 和 Tauri 生成的回调，验证默认不勾选时保留、勾选时展示说明并保留、更新模式仍保留；该 fixture 不安装产品、不写注册表，也不触碰默认用户数据路径。打包 workflow 在安装器相关路径发生 PR 改动时运行 Windows 生命周期验收；release tag 或手动触发仍构建全平台安装包。

卸载生成脚本回归补强：构建后检查验证确认页回调在卸载区段之前捕获复选框状态、preservation hook 位于两条且仅两条应用数据递归删除语句之前、两条语句都位于 Tauri 同意与非更新 guard 内；同时检查 hook 会清零状态、只显示说明而不含递归删除，并拒绝缺失状态捕获和额外无条件数据删除。Windows UI smoke 从当前生成脚本提取 `un.ConfirmShow` / `un.ConfirmLeave` 和实际标签，通过 native control 实测默认未选中时保留、勾选后提示并保留，以及 `/UPDATE` 模式仍保留；静默 `/S` 模式通过测试专用 `/TESTCHECKED` 预置已勾选状态，验证正常退出和 canary 原值保留；canary 全部位于临时 fixture。生成器检查、原生图形卸载和实际 `/UPDATE` 文件恢复均已由 Windows package run `36667041840` 通过。

2026-09-30 较早一版 NSIS 包（SHA-256 `48267b073f369501a0e9e61ef1507f76cdb8a62d5d4c489a28257271acadbc86`）真实生命周期验收：完整 smoke 安装到 `target/release/nsis-install-smoke/4614077969f54f6fb0aa1fba03dd2a0e/`，应用进程级正常关闭 smoke 和卸载均返回 0，Roaming 与 LocalAppData 两枚 canary 原值保留。该记录保留作历史证据；本机当前 NSIS 产物的更新哈希及其复验见下方。

| 安装器（2026-09-30 当前工作树构建） | 大小 | SHA-256 |
| --- | ---: | --- |
| `Yukinal_1.0.0_x64-setup.exe` | 31.2 MiB | `DF6E1369E2F405BCFE35F20088F64476F6A3EBB8C022F1CE888A725674DB4B0D` |
| `Yukinal_1.0.0_x64_en-US.msi` | 44.5 MiB | `F57F401147D1FD2D8AB6917D2C0BB0235A8CB2D72309D2EFEFCC0F99839C1E03` |

当前 NSIS 哈希通过生成脚本检查及四路径隔离回归测试（默认未选中、勾选、`/UPDATE`、静默已勾选状态），并已完成该哈希对应的隔离安装、启动、正常退出、静默卸载和数据保留验收。当前 MSI 哈希对应的管理员映像载荷正常关闭和强制终止测试通过，但 MSI 当前哈希尚未完成真实安装/卸载生命周期。

2026-09-30 当前 NSIS 产物真实生命周期验收：SHA-256 为 `DF6E1369E2F405BCFE35F20088F64476F6A3EBB8C022F1CE888A725674DB4B0D`。在临时移走并随后恢复真实 LocalAppData 数据目录、导出并恢复原 HKCU 产品注册值后，`-PlanOnly` 无阻碍；完整 smoke 安装到 `target/release/nsis-install-smoke/21bfcb80347946d888e663f22809032d/`。安装后的 bundled `node.exe` 哈希匹配、Agent 握手成功、应用正常关闭且 sidecar 随后退出；安装器与卸载器均返回 0，Roaming 和 LocalAppData canary 均保留，卸载注册和测试安装已清理。测试主机安装有 Node.js，但应用进程使用受限 PATH，未找到系统 Node。原用户目录 159 个文件（12,034,934 字节）的逐文件 SHA-256 摘要测试前后相同；`HKCU\Software\Yukinal contributors\Yukinal` 默认 REG_SZ 值及唯一值/无子键结构均原样恢复。日志根目录：`target/release/nsis-install-smoke/21bfcb80347946d888e663f22809032d/`。

产物位于 `target/release/bundle/`，本地构建日志与产物本身不纳入版本控制；重新构建会生成不同哈希。MSI 管理员映像位于 `target/release/msi-smoke/`、`target/release/msi-smoke-current/`、`target/release/msi-smoke-latest/`、`target/release/msi-smoke-final-current/` 和 `target/release/msi-current-package-smoke-20260929/`，隔离数据和验收日志位于 `target/release/installer-smoke/`，均不纳入版本控制。

2026-09-29 主机预检历史快照（已由下方 2026-09-30 当前工作树复核更新）：当时 MSI SHA-256 为 `56646E0C9157CB4E37BB034DF54B46F69D6254357763448A1C148629EC397E96`，ProductCode 为 `{17ED4DC3-438C-4685-9953-8D4692C8FDB9}`。当时 `-PlanOnly` 确认其为 per-machine 包；因 PowerShell 非管理员且用户数据根目录存在而拒绝安装，没有写入 Program Files 或 MSI 卸载注册。NSIS 同样因 LocalAppData 根目录与历史 `HKCU\Software\Yukinal contributors\Yukinal` 安装路径值而拒绝。该 LocalAppData 根目录是在本轮最初的载荷冒烟期间创建的：数据库虽已重定向，WebView2 仍创建了默认 `EBWebView` 配置目录；元数据仅见该目录（159 个文件、约 11.5 MiB），后续冒烟未改变时间戳。进程级脚本现将 `WEBVIEW2_USER_DATA_FOLDER` 指向每次独立的 `target/release/installer-smoke/<run-id>/webview2` 并验证路径确实创建；当时 MSI 载荷的正常关闭与父进程强杀 smoke 通过。

2026-09-30 本机 Windows 包快照：`pnpm run package` 曾在 Windows x64 成功完成 release 编译、NSIS/MSI 打包、生成脚本检查和 manifest 写入；源码 revision 为 `27c4f7aaefdbbdde356e0350860e8f451a112f3a`。最终 NSIS SHA-256 为 `df6e1369e2f405bcfe35f20088f64476f6a3ebb8c022f1ce888a725674db4b0d`，MSI SHA-256 为 `f57f401147d1fd2d8ab6917d2c0bb0235a8cb2d72309d2efefcc0f99839c1e03`，两者 Authenticode 均为 `NotSigned`。本机 UI、checkbox、NSIS 生命周期和 MSI 管理员映像载荷检查详见以上记录；本机 `-PlanOnly` 因真实用户数据、注册值、非管理员 shell 与已安装 Node 安全拒绝系统级安装。后续 GitHub Windows package run `36664162122` 已另行通过对应源代码构建的 MSI/NSIS 实际安装与卸载步骤；run `36666059734` 将验证新增 NSIS `/UPDATE` 就地更新场景。远端 CI 对应的包哈希与本机旧快照不同，应以各自 manifest 为准。


## 开发和本地验证

开发和构建需要 Node.js ≥ 24、pnpm 11.8.0、Rust 1.90+（含 rustfmt、Clippy）及当前平台所需的 Tauri 2 系统依赖。最终用户使用打包后的应用不需要另装 Node.js。仓库根目录执行：

```bash
pnpm install --frozen-lockfile
pnpm check
pnpm --filter @yukinal/desktop tauri dev
```

只开发界面时可运行 `pnpm desktop:dev`；浏览器预览没有 SQLite、SSH、PTY、系统凭据库或原生 Agent 能力。构建当前平台安装包使用 `pnpm package`；它会先对六种受支持平台逐一核对 Node.js 官方 SHASUMS pin，再下载并校验当前平台归档，产物位于 `target/release/bundle/`。本地构建成功只证明该平台产物生成，不代替安装后的验收。

成功运行 `pnpm package` 会在 `target/release/bundle/` 同步生成 `SHA256SUMS.txt` 与 `release-manifest.json`，记录可分发安装文件的 SHA-256、字节数、应用/随包 Node.js 版本、构建平台、源码修订和工作区是否有未提交改动（含未忽略的新文件）。单独重建/替换产物后可运行 `pnpm release:manifest` 更新清单；交付或上传前用 `pnpm release:manifest -- --verify` 复核哈希、文件大小、清单覆盖范围和路径安全。`--no-bundle` 编译检查不刷新旧安装包的清单。CI 的平台安装包作业会复核这两份文件再与安装包一起上传；测试也会检查 CI 上传规则覆盖清单支持的全部安装包后缀。清单未签名，不能证明发布者身份，也不替代安装验收或代码签名。

## 发布验收

### 产品路径

以[目标架构与施工交接](./architecture-and-build-plan.md#验收条件发布必须逐项通过)的 A1–A4、O1–O2、F1–F4、R1 为发布必过清单，分别验证 AI 目标执行与工具生态、运维状态可视化、双向文件传输及安装后运行。每个编号单独记录环境、操作、实际结果和证据位置；缺一项或仅有模拟/回环结果时继续标为未完成。旧版“只读诊断、受控变更、文件恢复”是此前阶段的验收口径，保留其回归价值，但不再代替新产品目标。

业务验收需要一台可重置的真实 SSH 服务器、一个实际可调用的 Provider、一个已配置的 MCP 端点，以及 Windows、macOS、Linux 的真实桌面安装环境。至少完成一条从用户目标到远端命令/配置、验证和活动记录的完整链路，并在各平台完成系统文件管理器拖入、远端拖出、双向下载/上传、预览、冲突与取消。记录命令退出码和远端事实，不以模型的口头结论代替验证。失败矩阵包括权限拒绝、目标变化、断连、超时、非零退出、MCP 不可用、传输中断和文件冲突。

### 安装包与现有自动回归

每个平台至少保留一条从干净安装环境执行的记录，包含提交、版本、操作系统、产物名称与 SHA-256、安装方式、随包 Node 版本、启动/退出/重开/卸载结果和脱敏失败日志位置。确认主机未安装 Node.js 时 sidecar 仍能启动，并检查应用退出后 sidecar、MCP 子进程和 PTY 是否正确收尾。读取安装器清单或本地构建成功不算完成安装验收。Windows 可用 `pwsh -NoProfile -File scripts/smoke-installed-windows.ps1` 对 NSIS 安装版做进程级复验，也可用 `-InstallDir <目录>` 检查 MSI 提取载荷；真实 MSI 生命周期应在一次性管理员测试机运行 `pwsh -NoProfile -File scripts/smoke-msi-install-windows.ps1`。这些脚本仍需配合干净系统、重启/升级及手动 GUI 验收。

已有 Agent fixture `pnpm --filter @yukinal/agent exec tsx --test src/runtime/investigation-acceptance.test.ts` 使用真实 AgentLoop、权限审批和 Host RPC client，但服务端和远端文件是状态化内存 fixture。native Host RPC + SQLite 测试 `cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml --lib evidence_record_host_rpc_survives_host_restart_and_remains_fetchable` 覆盖 evidence 跨 `AppState` 重启。回环 SSH/SFTP 测试验证协议操作；这些都不证明真实 Provider、OpenSSH、安装版 UI 或大文件传输。新接口需分别补契约、宿主、sidecar、真实远端及 GUI 层的可观察验收。

真实 Provider 的可选门禁为 `pnpm --filter @yukinal/agent test:live`，用 `YUKINAL_LIVE_PROVIDER_TESTS=1` 显式启用，并以 `YUKINAL_LIVE_PROVIDERS` 选定实际要测的 Provider。真实 Provider 和 MCP 测试需记录实现、版本、认证方式和通过的输入组合；没有跑到的组合继续标为未验证。

发布记录不得包含真实凭据、私钥、完整远端正文或未脱敏的模型上下文。签名、公证和自动更新尚未配置；向外提供安装包时必须明确说明。
