# 打包与分发

这一节回答三件事：怎么产出安装包、安装包里到底有什么、以及哪些部分是**故意没做或还没验证**的。把一个没跑过的步骤写成「已支持」，下一个人会在发布当天才发现。

**现状。** `apps/desktop/src-tauri/tauri.conf.json` 的 `bundle.active` 是 `true`，并在 `bundle.resources` 里把 agent 放到 `<resource_dir>/agent/index.js`；agent 由 esbuild 打成单文件，不再靠 `node_modules` 解析 `zod`、`@yukinal/shared`、`@yukinal/provider-sdk`；`scripts/check-packaging.mjs` 在每次门禁里把「配置 ↔ Rust 解析器 ↔ 实际产物」三者钉在一起；`scripts/smoke-packaged-agent.mjs` 把同一个文件放到「没有 `node_modules` 的目录」里再跑一遍协议冒烟。

**在 Windows 上，`pnpm package` 已经真的跑通过**，产出两份未签名安装程序：

```text
target/release/bundle/nsis/Yukinal_1.0.0_x64-setup.exe     （NSIS 安装程序）
target/release/bundle/msi/Yukinal_1.0.0_x64_en-US.msi      （WiX 安装包）
```

这条路径以前一次都没有跑完过，原因不在打包器而在配置：`build.beforeBuildCommand` 里的 `pnpm build:libs` 假定当前目录是仓库根，而 Tauri 执行它时的当前目录是 `apps/desktop`（`beforeBuildCommand` 的运行目录是应用目录，即 `src-tauri` 的上一级），那里没有这个脚本，于是 `tauri build` 在编译任何 Rust 之前就失败。现在它写作 `pnpm -w run build:libs`：`--filter` 与 `-w` 都能从子目录解析到 workspace 根，因此这条命令在哪个目录下执行都成立。这个顺序本身仍由门禁守着。

**本机产出安装包。** 前置条件：Node.js `>= 24`、pnpm `11`（`packageManager` 固定 `11.8.0`）、Rust stable（含 `rustfmt`、`clippy`）；Linux 还需 Tauri 系统依赖，与 `check.yml` 安装的是同一串：`libwebkit2gtk-4.1-dev librsvg2-dev patchelf build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev`；Windows 首次打包需要联网——Tauri CLI 会自行下载 **WiX 3 与 NSIS**（默认放进全局工具缓存，已下载过一次之后离线也能打包）。

```bash
pnpm install --frozen-lockfile
pnpm check                 # 门禁
pnpm run package           # 契约库 -> agent 单文件 -> 打包契约 -> tauri build
```

多余参数会透传给 `tauri build`——只编译不产出安装程序用 `pnpm run package -- --no-bundle`。它**先**跑契约库、agent 单文件与打包契约，**再**调用打包器，因为 `tauri build` 会先把整个 Rust workspace 按 release 编译一遍才去看资源文件（`profile.release` 里 `lto = "thin"`、`codegen-units = 1`），早几秒钟失败比十分钟后失败好得多。**不做跨平台交叉打包**：要哪个平台就在那个平台上跑，macOS 的 `.app`/`.dmg` 只能在 macOS 上打。打包器下载工具链那一步可能失败在网络上，和代码无关。

产物落在 cargo workspace 的 `target/release/bundle/` 下，`pnpm run package` 结束时会把它实际生成的文件连大小一起列出来。目录与平台的对应关系是固定的：Windows 是 `.../msi`、`.../nsis`，macOS 是 `.../macos`、`.../dmg`，Linux 是 `.../deb`、`.../rpm`、`.../appimage`。

## 安装包里有什么

- 桌面程序本身：Rust 宿主、前端静态资源（由 Tauri 内嵌进可执行文件）、图标和元数据。
- Agent sidecar：`<resource_dir>/agent/index.js` 加上一份一行的 `agent/package.json`（**两份资源文件**）。bundle 本身**是一个文件**，esbuild 把 `zod`、`@yukinal/shared`、`@yukinal/provider-sdk` 以及 agent 自己的全部源码内联进去（当前约 900 KiB），`node:` 开头的内置模块是唯一保留的外部件。
- `<resource_dir>` 由 Tauri 决定：macOS 是 `Yukinal.app/Contents/Resources`，Windows 是可执行文件旁边的 `resources` 目录，Linux 是随包安装的资源目录。
- **为什么必须是一个文件**：安装后的应用没有 `node_modules`，任何一个没被内联的裸模块名都会让 `node <resource>/agent/index.js` 直接以 `ERR_MODULE_NOT_FOUND` 退出。`scripts/smoke-packaged-agent.mjs` 针对的就是这件事。
- **那份一行的 `package.json` 不是装饰**：bundle 是 ESM，而安装后的 `<resource_dir>/agent/` 里没有相邻的 `package.json` 时，Node 靠**语法探测**判断模块系统。实测（Node 26.5.1、空目录）确实能启动，但同一文件加上 `--no-experimental-detect-module` 就会失败；`NODE_OPTIONS` 会被子进程继承，所以用户环境里只要存在那个开关，应用就会以一句 Node 解析错误启动失败，而我们的代码里没有任何地方能解释这件事。显式声明 `{"type": "module"}` 把加载方式变成权威来源。代价是这份文件**只能**有 `type` 一个键——多出 `name`/`exports` 会让 Node 把 `agent/` 当成一个包并按包规则解析——`scripts/check-packaging.mjs` 因此断言 `agent/` 下恰好这两个目的地并检查那份 JSON 的内容。
- **安装包里没有 Node.js**：不捆绑、不内嵌、不下载、不 vendor 任何运行时，用户机器上的 `node` 就是运行时（理由与残留风险见 [ADR 0013](./adr.md#adr-0013安装包分发随包分发-agent-bundle但使用用户自己的-node)）。安装包里同样没有 `node_modules`、pnpm、源码、测试、构建脚本与任何开发期工具。
- **历史教训**：`apps/agent` 曾经允许 tsc 输出到 `dist`，跑一次 `tsc -p tsconfig.json` 就会把逐文件产物覆盖 esbuild 的单文件 bundle，而「类型检查通过」这句话本身不会提示任何异常，真正变红的是几步之后的打包契约。这条路径**真的发生过两次**（第二次是有人绕过 `package.json` 的脚本直接调用 tsc，所以修在脚本上不够）。现在 `noEmit` 写在 `apps/agent/tsconfig.json` 里，`outDir`/`rootDir` 一并去掉——构建产物只有一个来源，无论 tsc 怎么被调用。

## 用户需要准备什么

| 需要 | 从哪来 | 缺了会怎样 |
| --- | --- | --- |
| Node.js >= 24（与根 `package.json` 的 `engines.node`、CI 的 `node-version`、esbuild 的 `--target=node24` 是**同一个数**） | 用户自己装，`node`（Windows 上是 `node.exe`）出现在 `PATH` 上 | 启动前执行有 5 秒上限的 `node --version`；缺失、过旧或输出不可解析时给出 `nodejs.org` 与可选的 `YUKINAL_NODE` 路径 |
| WebView2 运行时（Windows） | Windows 10/11 一般自带；安装程序默认 `downloadBootstrapper`，即安装时联网下载引导程序 | 窗口起不来 |
| webkit2gtk-4.1 / GTK3（Linux） | 见下面的已知缺口 | 窗口起不来 |

**缺 Node 或缺 bundle 时应用怎么报。** Rust 侧的解析顺序（`crates/core/src/sidecar/config.rs`）是：`YUKINAL_AGENT_COMMAND`（可配 `YUKINAL_AGENT_ARGS`，分号分隔）→ `YUKINAL_AGENT_ENTRY`（可配 `YUKINAL_NODE`）→ `<resource_dir>/agent/index.js` → 开发兜底（从当前工作目录向上找 `apps/agent/dist/index.js`）。安装包路径排在开发兜底之前是有意的：安装后的应用没有仓库树可向上走，开发运行没有 staged 资源，两种顺序各自在真正重要的场景里给出正确答案。四条都不成立时错误消息会点名构建步骤：

```text
no agent bundle to launch (searched <它找过的每一个路径>); run `pnpm --filter @yukinal/agent build`
```

`searched` 里包含打包路径，所以用户和排查的人都能看见它到底去哪儿找过 —— 有一条测试专门钉住这一点。`node` 本身完全不存在时，启动错误会直接点名最低版本、`nodejs.org` 与 `YUKINAL_NODE`。仍然**不做** `node --version` 预检，这是 ADR 0013 的知情决定（每次启动都多起一个进程，而且仍抓不到“装了但太旧”）；因此已安装的错误版本可能表现为 stderr 上的一行解析错误加退出码。

## 运行时会用到的路径与开关

| 什么 | 在哪 |
| --- | --- |
| agent bundle | `<resource_dir>/agent/index.js` |
| agent 数据目录 | 桌面启动时把 Tauri 的 `app_data_dir()` 通过 `YUKINAL_DATA_DIR` 给 sidecar，除非环境里已设同名变量。identifier 是 `dev.yukinal.workspace`，于是 Windows 是 `%APPDATA%\dev.yukinal.workspace`，macOS 是 `~/Library/Application Support/dev.yukinal.workspace`，Linux 是 `~/.local/share/dev.yukinal.workspace` |
| 覆盖启动方式（排查用） | `YUKINAL_AGENT_COMMAND`、`YUKINAL_AGENT_ENTRY`、`YUKINAL_NODE`、`YUKINAL_AGENT_TIMEOUT_SECS`（缺省 10 秒）、`YUKINAL_LOG_LEVEL` |
| sidecar 日志 | stdout 只承载协议帧，日志一律走 stderr |

**为什么配置长这样。** `tauri.conf.json` 是严格 JSON，写不了注释，所以理由记在这里：

- **`bundle.resources` 用映射而不是列表**：目标路径是契约（Rust 侧 `packaged_entry()` 解析的就是 `<resources>/agent/index.js`），映射把目标写死，源路径相对 `src-tauri`，所以是 `../../agent/dist/index.js`；列表形式保留源目录结构，装出来会是 `resources/agent/dist/index.js`。
- **`bundle.targets` 显式列出三个平台的产物**（`nsis`/`msi`、`app`/`dmg`、`deb`/`rpm`/`appimage`）：tauri-bundler 会按当前宿主过滤这张表，所以同一份配置在三个平台都对；不写 `"all"` 是因为「某个平台默认多了或少了一种产物」应该是一次看得见的改动。
- **`bundle.icon` 必须列全五项**：打包器没有图标会直接拒绝运行（空数组时 MSI 找不到 `.ico` 会失败，macOS 生成不了 app icon）。Tauri 2 配置 schema 里 `bundle.icon` 的默认值是**空数组**，那五项来自 CLI 内嵌的项目模板而非打包时默认值，所以必须显式写出。图标已随仓库提交（`apps/desktop/src-tauri/icons/`，17 个文件），`scripts/check-packaging.mjs` 每次门禁都复查这五项在列表里而且文件真的存在。
- **`build.beforeBuildCommand` 的顺序是契约库 → agent 单文件 → 前端**：原来只构建前端，`tauri build` 会先编译十分钟 Rust 然后才发现要打包的资源文件根本不存在；而「构建成功、但装进去的 agent 是上一次的」是更糟的静默错误。
- **故意没有签名配置**：没有 `bundle.signingIdentity`、没有 Windows 证书指纹、没有公证、没有 `createUpdaterArtifacts`。本仓库没有证书，写一个不能工作的签名配置只会让构建失败。
- **依赖记录位置**：esbuild 声明在 `apps/agent/package.json` 的 `devDependencies`，因为跑它的脚本就在那个 workspace 里；生命周期脚本白名单在 `pnpm-workspace.yaml` 的 `allowBuilds` 里。

**标记（图标）的来源与重新生成**（只在标记改变时才需要）：源图是 `apps/desktop/design/app-icon.png`（1024×1024），生成脚本是 `scripts/generate-app-icon.ps1`，用 PowerShell 的 `System.Drawing` 画出来，因此**只能在 Windows 上重新生成**（并在仓库根目录下运行）。由源图生成整套图标：

```bash
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/generate-app-icon.ps1
pnpm --filter @yukinal/desktop icon     # 即 tauri icon design/app-icon.png
```

实测：用当前源图重新生成一遍，除 `icon.icns` 之外的 16 个文件与仓库里提交的逐字节相同；`icon.icns` 同一源图连续生成两次的 SHA-256 也不同（文件大小一样），这是 `tauri icon` 组装 ICNS 容器的方式导致的，**不要**为了「顺手重新生成一次」而提交新的 `icon.icns`。

**CI。** `.github/workflows/check.yml` —— 每次 push 到 `main` 与每个 PR，三个操作系统各跑一次 `pnpm check`，**不打包**。`.github/workflows/package.yml` —— 只在 `workflow_dispatch` 和 `v*` 标签上触发；Windows、macOS、Ubuntu 22.04 各跑 `pnpm check` 再跑 `pnpm run package`，把 `target/release/bundle/**` 当作 artifact 上传。打包之所以是单独作业：① 打包不是编译检查，它会下载平台工具链、按 release profile 重编译整个 Rust workspace，每个平台几分钟，失败原因常常是「下载镜像慢」而不是代码错了，而「一条会因为网络超时而红的门禁，最后会被人忽略」；② 打包作业仍然**先跑同一条 `pnpm check`**，而不是复制一份更松的步骤清单——测试不过的树打出来的安装包，比没有安装包更糟；③ 用 `ubuntu-22.04` 而不是门禁用的 `ubuntu-latest`，因为 `.deb`/AppImage 的 glibc 下限等于构建机的 glibc。

## 发布流程

发布以 `main` 上的一次 release commit 和对应 `vX.Y.Z` 标签为准。步骤：

1. 确认工作区只包含本次发布需要的改动，并完成全部审查。
2. 更新 `packages/shared/src/version.ts`、六个 `package.json`、`Cargo.toml`、`tauri.conf.json` 和版本化 IPC fixture；运行 `pnpm --filter @yukinal/shared test`，让版本门禁先变绿。
3. 把 `docs/changelog.md` 的未发布内容整理成带日期的版本小节，并同步 README 的发布状态与本文档中的产物名称。
4. 在目标平台运行 `pnpm check` 和 `pnpm package`；至少要确认包内的 agent bundle、资源映射和版本号来自同一棵树。
5. 提交为 `release: prepare vX.Y.Z`，创建 annotated tag：`git tag -a vX.Y.Z -m "Yukinal vX.Y.Z"`。
6. 推送提交与标签；`.github/workflows/package.yml` 会在 `v*` 标签上重新运行门禁，并上传三个平台的安装包 artifact。
7. 发布负责人下载 artifact、核对来源和校验值，再按 GitHub Release 说明附上平台限制；未签名产物不得被描述成已签名发布。

## 未签名与未验证的部分

- 所有产物**未签名**：没有 macOS 签名身份与公证，没有 Windows 代码签名证书。macOS 上首次打开需要右键「打开」，Windows 上 SmartScreen 会提示「未知发布者」。没有更新器，升级靠重新下载安装包。
- **已核对**：`tauri.conf.json` 通过 Tauri CLI 自带的 `config.schema.json` 校验；`bundle.icon` 列的图标都在；`tauri icon` 能重现提交的图标（`icon.icns` 例外见上）；agent 单文件能在没有 `node_modules` 的目录里按协议应答；门禁每次复查资源映射、目标平台、图标与构建顺序；Windows 上的 `pnpm package` 已完整跑通并产出上述两份安装程序。
- **没跑过**：**装完之后应用能否真的拉起 sidecar** —— 安装包产出过，但没有真的安装并启动验证过。macOS 的 `.app`/`.dmg`、Linux 的 `.deb`/`.rpm`/`.AppImage` 连构建都没有在本仓库执行过（本机是 Windows）。
- **`.deb` / `.rpm` 已声明依赖基线，但没有在干净发行版中安装验证。** Debian 配置声明 `libwebkit2gtk-4.1-0 | libwebkit2gtk-4.0-37` 与 `libgtk-3-0 | libgtk-3-0t64`；RPM 配置声明 `webkit2gtk4.1` 与 `gtk3`。打包契约会拒绝空的依赖表，但跨发行版包名和版本仍只能在对应环境安装后确认。
