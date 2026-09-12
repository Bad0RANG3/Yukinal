# 打包与发布

本文件回答三件事：怎么产出安装包、安装包里到底有什么、以及哪些部分是**故意没做或还没验证**的。最后一点和前面两点同样重要 —— 把一个没跑过的步骤写成"已支持"，下一个人会在发布当天才发现。

## 现状

- `apps/desktop/src-tauri/tauri.conf.json` 的 `bundle.active` 已经是 `true`，并且在 `bundle.resources` 里把 agent 放到 `<resource_dir>/agent/index.js`。
- agent 由 esbuild 打成**单文件** `apps/agent/dist/index.js`，不再靠 `node_modules` 解析 `zod`、`@yukinal/shared`、`@yukinal/provider-sdk`。
- `scripts/check-packaging.mjs` 在每次 `pnpm check` 里把「配置 ↔ Rust 解析器 ↔ 实际产物」三者钉在一起，`scripts/smoke-packaged-agent.mjs` 把同一个文件放到"没有 `node_modules` 的目录"里再跑一遍协议冒烟。
- **本仓库至今没有产出过安装包**：`tauri build` 需要一次完整的 Rust release 编译加平台打包工具链，这条路径第一次执行者是 `.github/workflows/package.yml`（或你本机的 `pnpm run package`）。下面每一条命令都可以照抄运行，但"第一个安装包"来自那一次运行。

本文是操作说明；背后的取舍（为什么不分发 Node、为什么不做签名与自动更新、有哪些残留风险）记在 [ADR 0013](adr/0013-installer-distribution.md) 里。

## 本机产出安装包

前置条件（和跑门禁相同）：

- Node.js >= 24（根 `package.json` 的 `engines`），pnpm 11（`packageManager` 固定 `11.8.0`）。
- Rust stable（`rust-toolchain.toml` 要求 `rustfmt`、`clippy`）。
- Linux 还需要 Tauri 的系统依赖，与 `check.yml` 安装的是同一串：
  `libwebkit2gtk-4.1-dev librsvg2-dev patchelf build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev`。
- Windows 首次打包需要联网：Tauri CLI 会自行下载 WiX 3 与 NSIS，默认放进全局工具缓存目录（想让它们进 `target/` 就用 `bundle.useLocalToolsDir`）。

三条命令：

```bash
pnpm install --frozen-lockfile
pnpm check                 # 门禁
pnpm run package           # 契约库 -> agent 单文件 -> 打包契约 -> tauri build
```

几点说明：

- `pnpm run package` 的多余参数会透传给 `tauri build`。例如只编译、不产出安装程序：`pnpm run package -- --no-bundle`；跳过签名相关步骤：`pnpm run package -- --no-sign`（本仓库本来就没有可用的签名配置）。
- 它**先**跑契约库、agent 单文件与打包契约，**再**调用打包器：`tauri build` 会先把整个 Rust workspace 按 release 编译一遍才去看资源文件，配置写错或 agent 打不成单文件的话，早几秒钟失败比十分钟后失败好得多。
- 不做跨平台交叉打包。要哪个平台的安装包，就在那个平台上跑（macOS 的 `.app`/`.dmg` 只能在 macOS 上打）。
- 打包器要下载工具链的那一步可能失败在网络上，和代码无关。

产物在 cargo workspace 的 `target/release/bundle/` 下，`pnpm run package` 结束时会把它实际生成的文件连大小一起列出来（文件名带版本号与架构，不要照抄本文档里的示例名）。目录与平台对应关系是固定的：

| 平台 | 目录 | 产物 |
| --- | --- | --- |
| Windows | `target/release/bundle/msi`、`target/release/bundle/nsis` | WiX 的 `.msi`、NSIS 的 `-setup.exe` |
| macOS | `target/release/bundle/macos`、`target/release/bundle/dmg` | `Yukinal.app`、`.dmg` |
| Linux | `target/release/bundle/deb`、`.../rpm`、`.../appimage` | `.deb`、`.rpm`、`.AppImage` |

## 安装包里有什么

- 桌面程序本身：Rust 宿主、前端静态资源（由 Tauri 内嵌进可执行文件）、图标和元数据。
- Agent sidecar：`<resource_dir>/agent/index.js`，**一个文件**。esbuild 把 `zod`、`@yukinal/shared`、`@yukinal/provider-sdk` 以及 agent 自己的全部源码内联进去（当前约 850 KiB）。`node:` 开头的内置模块是唯一保留的外部件。
- `<resource_dir>` 由 Tauri 决定：macOS 是 `Yukinal.app/Contents/Resources`，Windows 是可执行文件旁边的 `resources` 目录，Linux 是随包安装的资源目录。

为什么必须是一个文件：安装后的应用没有 `node_modules`，任何一个没被内联的裸模块名都会让 `node <resource>/agent/index.js` 直接以 `ERR_MODULE_NOT_FOUND` 退出。`scripts/smoke-packaged-agent.mjs` 针对的就是这件事 —— 它把 bundle 复制到一个空目录（没有 `node_modules`，连 `package.json` 都没有），按 `<resources>/agent/index.js` 的形状跑同一套协议断言。

关于它是怎么被当作 ESM 加载的：bundle 是 ESM（`--format=esm`），所以安装后的 `<resource_dir>/agent/` 里需要一份声明，**现在是两份资源文件**：bundle 本身，以及一个一行的 `agent/package.json`（内容只有 `{"type": "module"}`，源文件是 `apps/desktop/src-tauri/resources/agent-package.json`）。

最初这里**没有**第二份，靠的是 Node 的**语法探测**（syntax detection）：它在 `.js` 文件里看到 `import`/`export` 就按 ESM 执行，Node 22.7 起默认开启，Node 24 的文档也写明默认开启。实测（Node 26.5.1、空目录、无 `package.json`）确实能启动；但同一个文件加上 `--no-experimental-detect-module` 就会失败，报的是 Node 自己那句 `Make sure to set "type": "module" in the nearest package.json file or use the .mjs extension`。

改成显式声明，是因为「依赖一个默认开启的开关」在这里可以被环境推翻：`NODE_OPTIONS` 会被子进程继承，所以用户环境里只要存在 `--no-experimental-detect-module`，安装后的应用就会以一句 Node 的解析错误启动失败，而我们的代码里没有任何地方能解释这件事。`package.json` 的 `type` 是加载方式的权威来源，把它放在文件旁边，这个前提就不存在了。

代价与边界：多一个资源文件；那份文件**只能**有 `type` 一个键（多出 `name`/`exports` 会让 Node 把 `agent/` 当成一个包并按包规则解析，而这里要的只是「这个目录里的 `.js` 是 ESM」）；`scripts/check-packaging.mjs` 因此断言 `agent/` 下**恰好**这两个目的地并检查那份 JSON 的内容 —— 它是一份契约，不是随手放的配置文件。

（`apps/agent` 的项目曾经把 `outDir` 指向 `dist` 且允许 emit：跑一次 `tsc -p tsconfig.json` 就会把逐文件的 tsc 产物写进 `dist`、覆盖掉 esbuild 的单文件 bundle，而「类型检查通过」这句话本身不会提示任何异常 —— 真正变红的是几步之后的打包契约。这条路径真的发生过两次：第二次是有人绕过 `package.json` 的脚本直接调用 tsc，所以修在脚本上是不够的。现在 `noEmit` 写在 `apps/agent/tsconfig.json` 里，`outDir`/`rootDir` 也一并去掉 —— 构建产物只有一个来源，无论 tsc 怎么被调用。）

## 安装包里没有 Node.js

这是已经定好的决定，不再讨论：**不捆绑、不内嵌、不下载、不 vendor 任何 Node 运行时**。用户机器上的 `node` 就是运行时。代价写在下面两节里：安装包小，但用户在装完之前必须先有 Node；缺 Node 的报错目前还只是"启动失败"，见"缺 Node 或缺 bundle 时应用怎么报"。

安装包里同样没有：`node_modules`、pnpm、源码、测试、构建脚本、任何开发期工具。agent 的源码只以 bundle 形式存在于 `agent/index.js` 中。

## 用户需要准备什么

| 需要 | 从哪来 | 缺了会怎样 |
| --- | --- | --- |
| Node.js >= 24（与根 `package.json` 的 `engines.node`、CI 的 `node-version`、esbuild 的 `--target=node24` 是同一个数） | 用户自己装，`node`（Windows 上是 `node.exe`）出现在 `PATH` 上 | 见下一节：目前表现为"启动 sidecar 失败" |
| WebView2 运行时（Windows） | Windows 10/11 一般自带；安装程序默认 `downloadBootstrapper`，即安装时联网下载引导程序 | 窗口起不来 |
| webkit2gtk-4.1 / GTK3（Linux） | 见下面的已知缺口 | 窗口起不来 |

已知缺口（已核对，尚未修）：`.deb` 的 `Depends:` 完全来自 `bundle.linux.deb.depends`，而那份配置现在是空的 —— 打包器不会自动补 `libwebkit2gtk-4.1-0` / `libgtk-3-0`，`.rpm` 同理（`bundle.linux.rpm.depends`）。所以最小化安装的系统上，包装得进去、装得上，但启动会因为缺库失败。修法是给这两个键填上发行版对应的包名，但包名属于各发行版的事实，必须在那台机器上装一次验证过再写进配置 —— 写错了会让安装直接失败，比现在这种"装得上但起不来"更难查。

## 缺 Node 或缺 bundle 时应用怎么报

Rust 侧的解析顺序（`crates/core/src/sidecar/config.rs` 的 `from_env_with_resources`）：

1. `YUKINAL_AGENT_COMMAND`（可配 `YUKINAL_AGENT_ARGS`，分号分隔）；
2. `YUKINAL_AGENT_ENTRY`（可配 `YUKINAL_NODE`）；
3. `<resource_dir>/agent/index.js`；
4. 开发兜底：从当前工作目录向上找 `apps/agent/dist/index.js`。

四条都不成立时，错误消息是：

```text
no agent bundle to launch (searched <它找过的每一个路径>); run `pnpm --filter @yukinal/agent build`
```

`searched` 里包含打包路径，所以用户（和排查的人）能看见它到底去哪儿找过；`crates/core/src/sidecar/config.rs` 里有一条测试专门钉住"打包路径必须出现在这条消息里"。

**没做到的**：`node` 本身不存在（不在 `PATH` 上）或版本过旧时，**没有**版本探测。这不完全是缺口 —— [ADR 0013](adr/0013-installer-distribution.md) 明确决定**不做** `node --version` 预检（每次启动多起一个进程，而且仍然抓不到"装了但太旧"：那种情况表现为 stderr 上的解析错误加退出码），但要求"缺少 Node"是一条**可执行的错误**，消息里要说清三件事：这是一个不随附 Node 运行时的构建、需要的版本下限、以及两条出路（装 Node，或把 `YUKINAL_NODE` 钉到一个绝对路径）。当前 `crates/core/src/sidecar/mod.rs` 里这条路径还只是 `SidecarError::Launch` 包着系统错误，形如 `failed to launch agent sidecar: <系统错误>`；按 ADR 0013 第 6 条，消息本身要带上上面那三件事。**下限只有一个数：`>= 24`** —— `engines.node`、CI 钉的 `node-version`、esbuild 的 `--target=node24` 写的是同一个数，`scripts/check-packaging.mjs` 会在门禁里断言 `--target` 与 `engines.node` 一致（不一致就红），但它本身仍然是约定，不进运行时校验。

## 运行时会用到的路径

| 什么 | 在哪 |
| --- | --- |
| agent bundle | `<resource_dir>/agent/index.js`（见"安装包里有什么"） |
| agent 数据目录 | 桌面启动时把 Tauri 的 `app_data_dir()`（即 `data_dir()/<identifier>`）通过 `YUKINAL_DATA_DIR` 给 sidecar，除非环境里已经设了同名变量（`apps/desktop/src-tauri/src/lib.rs`、`apps/desktop/src-tauri/src/commands/mod.rs`）。identifier 是 `dev.yukinal.workspace`，于是 Windows 是 `%APPDATA%\dev.yukinal.workspace`，macOS 是 `~/Library/Application Support/dev.yukinal.workspace`，Linux 是 `~/.local/share/dev.yukinal.workspace` |
| 覆盖启动方式（排查用） | `YUKINAL_AGENT_COMMAND`、`YUKINAL_AGENT_ENTRY`、`YUKINAL_NODE`、`YUKINAL_AGENT_TIMEOUT_SECS`、`YUKINAL_LOG_LEVEL` |
| 日志 | sidecar 的 stdout 只承载协议帧，日志一律走 stderr（`apps/agent/src/config.ts` 顶部那条规则） |

## 为什么配置长这样

`tauri.conf.json` 是严格 JSON，写不了注释，所以理由记在这里。

- **`bundle.resources` 用映射而不是列表**：目标路径是契约（Rust 侧 `packaged_entry()` 解析的就是 `<resources>/agent/index.js`），映射把目标写死，源路径相对 `src-tauri`，所以是 `../../agent/dist/index.js`。列表形式做不到这件事 —— 它保留源目录结构，装出来会是 `resources/agent/dist/index.js`。目标里的 `agent/` 目录不需要预先存在：打包器复制资源时会自己创建父目录（`copy_file` 里的 `create_dir_all`）。
- **`bundle.targets` 显式列出三个平台的产物**（`nsis`/`msi`、`app`/`dmg`、`deb`/`rpm`/`appimage`）。tauri-bundler 的 `Settings::package_types` 会按当前宿主过滤这张表，所以同一份配置在三个平台都对；不写 `"all"` 是因为"某个平台默认多了/少了一种产物"应该是一次看得见的改动。
- **`bundle.icon` 必须列全**：打包器没有图标会直接拒绝运行（空数组时 MSI 找不到 `.ico` 会失败，macOS 生成不了 app icon）。图标已随仓库提交（`apps/desktop/src-tauri/icons/`），配置里列的这五项就是 CLI 项目模板里的标准五项，CI 不需要重新生成任何图标。
- **`build.beforeBuildCommand` 的顺序是契约库 → agent 单文件 → 前端**。原来只构建前端：`tauri build` 会先编译十分钟 Rust，然后才发现要打包的资源文件根本不存在；而"构建成功、但装进去的 agent 是上一次的"是更糟的静默错误。
- **故意没有签名配置**：没有 `bundle.signingIdentity`、没有 Windows 证书指纹、没有公证、没有 `createUpdaterArtifacts`。本仓库没有证书，写一个不能工作的签名配置只会让构建失败。不签名的本机构建照常可用。
- **依赖记录位置**：esbuild 声明在 `apps/agent/package.json` 的 `devDependencies`（`^0.28.2`），因为跑它的脚本就在那个 workspace 里；根 `package.json` 只放根脚本自己的依赖，不重复声明同一版本。生命周期脚本白名单在 `pnpm-workspace.yaml` 的 `allowBuilds` 里，esbuild 已经在其中。

## 图标

打包**只引用**已经提交的图标，不生成也不改写任何图标文件：`bundle.icon` 列的就是 `apps/desktop/src-tauri/icons/` 里那五项 —— `icons/32x32.png`、`icons/128x128.png`、`icons/128x128@2x.png`、`icons/icon.icns`、`icons/icon.ico`，路径相对 `src-tauri/`，五个文件都已随仓库提交（该目录一共 17 个文件，另外 12 个是 `64x64.png`、`icon.png`、`StoreLogo.png` 与九个 `Square*Logo.png`）。

这一条必须显式写出来，不能省：Tauri 2 配置 schema 里 `bundle.icon` 的默认值是**空数组**，那五项来自 CLI 内嵌的项目模板（`tauri init` 生成的那份 `tauri.conf.json`），不是打包时的默认值。而且缺了对应格式的图标是有代价的：Windows 的 MSI 打包器找不到 `.ico` 会直接失败（`tauri-bundler` 的 `windows/msi` 里 `Couldn't find a .ico icon`），macOS 也要用这套图标生成 app icon。`scripts/check-packaging.mjs` 每次门禁都会复查"这五项在 `bundle.icon` 里、而且文件真的存在"。

标记本身的来源与重新生成（**只在标记改变时才需要**，日常打包不需要跑）：

- 源图是 `apps/desktop/design/app-icon.png`（1024×1024；深色圆角底 + 倾斜的环 + 一个节点和一个核心）。生成它的脚本是 `scripts/generate-app-icon.ps1`，用 PowerShell 的 `System.Drawing` 画出来，因此**只能在 Windows 上重新生成**（并且要在仓库根目录下运行，输出路径是相对根目录的）。图标已经提交进仓库，其他平台的贡献者不需要重新生成它们。
- 由源图生成整套图标（`apps/desktop/package.json` 里的 `icon` 脚本就是第二条命令）：

  ```bash
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts/generate-app-icon.ps1
  pnpm --filter @yukinal/desktop icon     # 即 tauri icon design/app-icon.png
  ```

- 实测：用当前源图重新生成一遍，`32x32.png`、`64x64.png`、`128x128.png`、`128x128@2x.png`、`icon.png`、`icon.ico`、`StoreLogo.png` 与九个 `Square*Logo.png` 共 16 个文件与仓库里提交的**逐字节相同**。
- 唯一的例外是 `icon.icns`：同一个源图连续生成两次，它的 SHA-256 也不同（文件大小一样）。这是 `tauri icon` 组装 ICNS 容器的方式导致的，不是标记变了。所以**不要**为了"顺手重新生成一次"而提交新的 `icon.icns`，那会是一次没有意义的二进制 diff。

## CI

- `.github/workflows/check.yml`：每次 push 到 `main` 与每个 PR，三个操作系统各跑一次 `pnpm check`。不打包。
- `.github/workflows/package.yml`：只在 `workflow_dispatch` 和 `v*` 标签上触发；Windows、macOS、Ubuntu 22.04 各跑 `pnpm check` 再跑 `pnpm run package`，把 `target/release/bundle/**` 当作 artifact 上传。

为什么打包是单独的作业，而不是塞进那个三平台门禁里：

- 打包不是编译检查。`tauri build` 会先按 release profile 编译整个 Rust workspace（`profile.release` 里 `lto = "thin"`、`codegen-units = 1`），然后下载平台工具链 —— Windows 上的 WiX 3 与 NSIS、Linux 上的 AppImage/dpkg 工具。每个平台几分钟，失败原因还常常是"下载镜像慢"而不是代码错了。门禁要在每次 push 上保持快且确定；一条会因为网络超时而红的门禁，最后会被人忽略。
- 打包作业仍然**先跑同一条 `pnpm check`**，而不是复制一份更松的步骤清单：测试不过的树打出来的安装包，比没有安装包更糟。
- 用 `ubuntu-22.04` 而不是门禁用的 `ubuntu-latest`：`.deb`/AppImage 的 glibc 下限等于构建机的 glibc，22.04 打出来的包在更老的发行版上也装得上。

## 未签名、未经本仓库验证的部分

- 所有产物**未签名**：没有 macOS 签名身份与公证，没有 Windows 代码签名证书。macOS 上首次打开需要右键"打开"或 `xattr -dr com.apple.quarantine`，Windows 上 SmartScreen 会提示"未知发布者"。没有更新器，装完之后升级要靠重新下载安装包。
- 已核对的部分：`tauri.conf.json` 通过 Tauri CLI 2.11.4 自带的 `config.schema.json` 校验；`bundle.icon` 列的图标都在；`tauri icon` 能重现提交的图标（`icon.icns` 的可复现性例外见上）；agent 单文件能在没有 `node_modules` 的目录里按协议应答；`scripts/check-packaging.mjs` 会在每次门禁里复查资源映射、目标平台、图标与构建顺序。
- **没跑过**的部分：`tauri build` 本身。本仓库还没有产出过安装包，所以下面这些只能算"已经配好并复核过"，不是"验证过"：WiX/NSIS 是否真的能把资源放进 `resources/agent/index.js`、`.app` 与 `.dmg` 的安装、`.deb`/`.rpm`/`.AppImage` 的安装与依赖、以及安装之后应用能否真的拉起 sidecar。第一次 `package` 工作流（或你本机第一次 `pnpm run package`）就是这条路径的第一次真实检验。
