# R2-C：构建 / 打包 / 发布链路 深度审查

## 1. 审查范围、方法与总体结论

**审查范围**（只读）：`scripts/package.mjs`、`scripts/lib/release-artifacts.mjs` 及其 `release-artifacts.test.mjs`、`scripts/build-python-agent.mjs`、`scripts/build-ssh-service.mjs`、`scripts/collect-python-licenses.py`、`scripts/smoke-python-agent.mjs`、`scripts/smoke-python-dev-agent.mjs`、`scripts/check.mjs`、`scripts/clean.mjs`、`scripts/start-desktop-preview.ps1`、`agent/packaging/entrypoint.py`、`agent/pyproject.toml`、`package.json`、`.github/workflows/check.yml`、`.gitignore`、`NOTICE`、`docs/release.md`、`docs/cpp-desktop-handoff.md`、`native/CMakeLists.txt`（仅依赖版本与选项）、`native/desktop_host/src/main.cpp`（仅 UI 解析与退出码）。**未修改任何被审查文件**；本次唯一写入物是本报告。

**方法**：先完整阅读 `scripts/lib/release-artifacts.mjs`（269 行）与 `scripts/package.mjs`（366 行）建立完整性契约模型，再对每一处结论**实测**——包括在 `target/`（已被 `.gitignore:13` 忽略）下复制真实载荷、注入额外文件、遗留陈旧载荷目录、建立真实目录联接（`mklink /J`）、伪造清单身份字段、并逐字节篡改二进制。所有探针脚本与日志都在 `target/review-logs/`，仓库根目录未新增任何文件（`git status --porcelain` 只有 task-5/FIX-2 的 5 个文档改动与 `docs/review/`、`ssh_agent_development_brief.md` 两个既有未跟踪项）。

**总体结论**：打包链路的**失败模式设计是合格的**——缺产物 fail-closed 且不产出部分载荷（`package.mjs:154-159`、`:187-192`、`:214-219`），三种模式在缺少清单时全部非零退出（`:326`、`:348`），单字节篡改、缺文件、校验和与清单不一致、路径越界都能被检出（实测见 第 2 节 与 第 6 节）。**但完整性契约的边界远窄于注释和文档的宣称**：`verifyReleaseManifest` 只复核「清单里列出的路径」，对**新增文件、整块陈旧载荷目录、联接指向的载荷外字节、以及清单里的版本/启动元数据**一律放行；而 `--manifest` 会把磁盘上的一切（包括被偷偷加进来的文件）重新「祝福」进契约。更严重的是，`pnpm package` 当前**根本不产出 UI 资源**，因此它的产物无法作为一个桌面应用启动——而没有任何 gate、smoke 或 verify 会指出这一点。

发现计数：**Critical 0 / High 3 / Medium 7 / Low 4（共 14）**。

**为什么没有 Critical**：当前链路是**未发布状态**——安装器、签名、公证、三平台真机验收都未实现（`docs/release.md:15`、`docs/cpp-desktop-handoff.md:83`），没有真实用户会拿到这个载荷；且没有发现「可被远程利用的提权/数据破坏」类缺陷。但 H-1 一旦有人按文档执行 `pnpm package` 并分发，就会产生「看起来验证通过、实际无法运行且缺 UI」的产物，因此按真实风险排序时它居首。

---

## 2. 七个必答问题的逐条判定

| # | 问题 | 判定 | 关键证据 |
| --- | --- | --- | --- |
| Q1 | `verifyReleaseManifest` 的完整性边界；`isSafeRelativePath` 对 Windows 保留名/UNC/ADS | **部分可信**：能挡住「已列载荷被改动/删除/尺寸变化/校验和不一致/路径越界」；**挡不住新增文件、陈旧同级载荷目录、联接逃逸、身份字段伪造、POSIX 权限位** | 第 3 节 H-2、M-2、M-4、M-7 |
| Q2 | 缺宿主/Agent/SSH 产物时的行为；三模式状态一致性 | **可信（fail-closed）**：缺任一产物即退出 1 且**不创建输出目录**；三模式缺清单即退出 1 | 第 2.2 节 |
| Q3 | `build-python-agent.mjs` 的 PyInstaller 调用、输出清理、许可证收集、smoke 充分性 | **无注入风险**（参数全为字面量）；**输出清理不完整**（`agent/runtime` 根下的陈旧文件不会删）；**许可证收集读的是构建 venv 而非冻结载荷**；smoke 只覆盖 version/imports/握手/ping | 第 3 节 M-5 |
| Q4 | 载荷污染（`.private/`、brief、测试 fixture、`.git`）；`listPayloadFiles` 名单策略 | **无敏感泄漏**：`.git`/`.private/`/brief/`node_modules` 均为 0；**但上游 vendored `tests/` 目录会随包发布**（18 个文件） | 第 3 节 L-1 |
| Q5 | NOTICE 与 `collect-python-licenses.py` 一致性 | **Python 侧无归属缺口**（21 个随包发行版全部有声明，0 个「发了未声明」）；control-plane 依赖不进载荷故不在范围内；**NOTICE 本身有两处事实错误**（Node runtime、字体清单），声明文件另有 64 条过度声明 | 第 3 节 M-3、M-5 |
| Q6 | CI 与本地 `pnpm check` 的差异；action 固定 SHA 可验证性 | **两侧覆盖面不同，且 CI 完全不覆盖打包**；控制面测试在 CI 永远 skip；**7 处 `uses:` 全部为 40 位十六进制 + 版本注释**（可验证），但无自动新鲜度检查 | 第 3 节 M-6 |
| Q7 | 实际复现 | **全部真实执行**：`pnpm package` / `--verify` / `release:manifest` 各退出 0；篡改后退出 1；详见 第 6 节 | 第 6 节 |

### 2.1 Q1：`verifyReleaseManifest` 到底能挡住什么

**能挡住**（`scripts/lib/release-artifacts.mjs:184-268`）：

- `schemaVersion` 不匹配（`:189-193`）；`payloadDirectory` 越界或含 `/`（`:194-196`）；空载荷（`:200`）
- 清单路径不安全、逃出 `payloadDirectory`、重复条目、sha256 格式非法（`:204-219`）
- `SHA256SUMS.txt` 不可读（`:226-228`），与清单**双向**不一致（`:230-238`）
- 条目在磁盘缺失 / 不是普通文件 / 尺寸不符 / sha256 不符（`:241-261`）

实测：单字节篡改 `yukinal-desktop-host.exe` → 退出 1 并指出具体 sha256 差异（第 6 节 R6）；`SHA256SUMS.txt` 缺条目 → 退出 1。

**挡不住**（全部实测，见 第 6 节）：

1. **新增文件**——无论载荷根目录还是任意子目录，`verify` 均退出 0。原因：`verifyReleaseManifest` 从不调用 `listPayloadFiles`，它只遍历 `manifest.payloads`（`:202`、`:241`）。
2. **陈旧/外来同级载荷目录**——`yukinal-1.0.0-old/TROJAN.exe` 与当前载荷并列，`verify` 仍 `SUCCESS files=3`（`target/review-logs/probe2-boundary.log:28-31`）。
3. **联接（junction）指向载荷外字节**——真实 `mklink /J` 替换 `ssh-service/` 且把真实字节移出载荷后，`verify` 报 `Verified 662 file(s)` 退出 0（第 3 节 M-2）。
4. **自洽重签**——载荷 + 清单 + SHA256SUMS 三者一起重写后 `verify` 退出 0。这不是缺陷本身（文档已承认无签名，`docs/cpp-desktop-handoff.md:83`），但注释没有说清「自洽的伪造与合法载荷不可区分」。
5. **清单身份/启动字段**——`version`、`agentVersion`、`platform`、`arch`、`sourceRevision`、`dirty`、`builtAt`、`launch.*`、`notes` 全部可伪造且 `verify` 退出 0（`target/review-logs/probe4-metadata.log`）。`verifyReleaseManifest(root, manifest)` 的签名里没有「期望版本」参数，因此**结构上不可能**校验这些字段。
6. **权限位/符号链接**——清单条目只有 `path`/`size`/`sha256` 三个键（`target/review-logs/probe2-boundary.log:40-42`），`verify` 只判 `stats.isFile()`（`:249`）。win32 上 POSIX 模式位无意义（标注为**未执行**，见 第 7 节）。

**关于注释宣称的强度**：`release-artifacts.mjs:8-9` 写「a payload directory either matches its manifest **exactly**」，`package.mjs:9-10` 写 verify「fails when a byte changed or **a file was added** or removed」。**「removed」为真，「added」为假**——这是本次审查中最直接的「注释强于实现」的一处。

**关于 `isSafeRelativePath`（`:34-44`）**：UNC（`\\s\share`、`//s/share`）、`C:` 盘符、绝对路径、`..`、`.`、空段、`\`、`\0`、超长（>4096）**全部正确拒绝**。但它**接受** Windows 保留设备名与别名：`CON`、`NUL`、`COM1`、`LPT1`、`aux`、`payload/CON`、`payload/NOTICE.`（尾点别名）、`payload/NOTICE `（尾空格别名）、`payload/NOTICE:ads`（ADS 流）。实测这些名字在今天是**偶然 fail-closed** 的：`stat` 对 `payload/NUL` 等返回 ENOENT → 记为 `missing on disk`（`probe2-boundary.log:20-26`）。但这依赖 Windows 的路径语义而非显式的契约检查，一旦清单来自 POSIX 侧生成、或在 `payloadDirectory` 处出现真实的 `CON` 文件，语义会变得不可预测。仓库内部已有正确先例：`native/desktop_host/src/transfer_types.cpp:497-515` 对传输名**显式**拒绝保留设备名、尾点、尾空格。

**修复建议**（按优先级）：

```js
// scripts/lib/release-artifacts.mjs:34-44 —— 与 transfer_types.cpp:497-515 对齐
const WINDOWS_RESERVED = new Set([
  "con","prn","aux","nul",
  ...Array.from({length:9},(_,i)=>`com${i+1}`),
  ...Array.from({length:9},(_,i)=>`lpt${i+1}`),
]);
// 段级：拒绝 尾点/尾空格、':'(ADS)、保留名(取 '.' 前的 stem 并 casefold 后比较)
```

并在 `verifyReleaseManifest` 中新增「无多余文件」检查：

```js
// 在 :261 之后
const onDisk = new Set(await listPayloadFiles(root, { skip: [CHECKSUMS_FILE, RELEASE_MANIFEST_FILE] }));
for (const path of onDisk) if (!expected.has(path)) problems.push(`${path}: on disk but not in the manifest`);
```

（注意：该检查会与 `--manifest` 的当前语义冲突，两者必须一起改，见 第 3 节 H-3。）

### 2.2 Q2：`pnpm package` 在缺产物时的行为与三模式一致性

**实测**（第 6 节 R7/R8）：

- 缺宿主产物（`--host-exe does/not/exist.exe`）→ `Required packaging input is missing: …` + 指引，**退出 1，且输出目录未被创建**（`$base/missing` 不存在）。因此「不会产出部分载荷」的宣称成立。
- 同样的 fail-fast 也覆盖冻结 Agent（`:187-192`）与 SSH 服务（`:214-219`），并且都提供可执行的指引文本。
- `--verify` 在无清单时 → `No manifest to verify: <path>`，退出 1（`:348`）；`--manifest` 同理 → `No manifest to refresh:`，退出 1（`:326`）。
- `--out`/`--host-exe` 等缺值 → `--out requires a value`，退出 1（`:103`）；未知参数 → 退出 1 并打印 USAGE（`:116`）。

**三模式状态一致性——发现一处不一致**：`stage`（`:272-321`）从源码重新推导 `version`（`packages/shared/src/version.ts:16` → `1.0.0`）与 `agentVersion`（`agent/pyproject.toml:3` → `0.1.0`），而 `refreshManifest`（`:323-343`）把 `existing.version`、`existing.agentVersion`、`existing.platform`、`existing.arch`、`existing.launch`、`existing.notes` **原样复制**，只重算 `payloads`、`sourceRevision`、`dirty`、`builtAt`。实测：把清单的 `version` 改成 `0.0.0-forged`、`agentVersion` 改成 `9.9.9-forged`、`launch.hostExecutable` 改成 `elsewhere/not-the-host.exe`，再跑 `--manifest`，三项**全部保留**，随后 `--verify` 仍报 `Verified 662 file(s)` 退出 0（`target/review-logs/probe5-refresh.log`）。即：**`--manifest` 既不能纠正也不能发现错误的版本/启动信息，它只让磁盘现状变成「已验证」**。修复方向：`refreshManifest` 应从源码重新推导 `version`/`agentVersion` 并断言与 `existing` 一致（不一致即失败）。

### 2.3 Q3：冻结 Agent 的构建、清理、许可证与 smoke

- **PyInstaller 调用无注入面**：`build-python-agent.mjs:35-56` 的 22 个参数全部是固定字面量，唯一外部输入是 `YUKINAL_PYTHON` 指定的**可执行文件路径**（`:16-18`），经 `spawnSync(python, args)` 以数组传参，无 shell 解释。`--paths agent/src`、`--collect-all <pkg>` 等都不来自用户。
- **输出目录清理不完整（实测源码级确认）**：脚本自身**没有**任何 `rm`/清理逻辑（仅 `--clean`/`--noconfirm` 传给 PyInstaller）。上游 `PyInstaller/building/build_main.py:1160-1174` 表明 `--clean` 只清 `cachedir` 与 `workpath`；`COLLECT.assemble` 调用的 `_make_clean_directory`（`PyInstaller/building/utils.py:386-397`）只清 `distpath/yukinal-agent` 这一个子目录。因此 `agent/runtime/` **根下**的陈旧文件不会被删除——本机就存在一个陈旧的 `agent/runtime/THIRD_PARTY_LICENSES.txt`（418306 B，2026-10-15 09:27:55），而 `package.mjs:289-292` 会**无条件**把它复制进载荷。
- **许可证收集读错了对象**：`collect-python-licenses.py:8,29` 遍历 `importlib.metadata.distributions()`，即**构建 venv**（实测 85 个发行版），而不是冻结载荷里实际包含的 21 个 `*.dist-info`。后果是**过度声明**而非漏声明：实测「随包但未声明 = 0」，「声明但未随包 = 64」，其中包括**已被删除的 Node agent** 的残留元数据 `yukinal-agent-node 0.1.0`（仍在 `agent/.venv/Lib/site-packages/yukinal-agent-node-0.1.0.dist-info`），以及 `pip`、`pyinstaller`、`paramiko`、`setuptools` 等构建期依赖。这类过度声明会把已删除组件写进随包的法律文本，既有误导性也让 NOTICE 无法作为「实际随包清单」使用。
- **smoke 强度**：`smoke-python-agent.mjs:24,34-38,52-55` 只验证 `--version`（`0.1.0`）、`--check-imports`（`packaged imports ok`）、JSON-RPC `initialize`（protocolVersion 1.0）与 `system.ping`。`entrypoint.py:10-28` 与之一致。**未覆盖**：真实 provider 调用、工具注册表、SQLite 迁移、与 C++ SSH 服务的实际联通、UI 启动。作为「冻结产物能跑起来」的门禁这是**合理的最小集**，但不足以支撑「可分发布」的结论。
- **一个必须记录的事实**：`agent/pyproject.toml` 的 `package = ["pyinstaller>=6.22,<7"]` 是构建冻结载荷的必需 extra，而 **CI 的 pip 安装命令（`check.yml:56`）不包含它**，因此 CI 环境里 `pnpm build:python-agent` 不可执行。

### 2.4 Q4：载荷污染

对干净载荷（未篡改、662 条条目）做模式匹配（`target/review-logs/probe9b-contamination.log`，另见 R5 之后 663 条的 `probe9-contamination.log`）：

| 模式 | 命中 |
| --- | --- |
| `.git/` | **0** |
| `.private/` | **0** |
| `ssh_agent_development_brief.md` | **0** |
| `node_modules/` | **0** |
| `conftest.py` | **0** |
| `*.log` / `.env` | **0** |
| `test_*.py` | 9 |
| `/tests/` 目录 | 18 |
| `index.html`（UI 产物） | **0** |
| `/assets/` | **0** |
| Node runtime | **0** |
| `Cargo.toml` / `*.rs` | **0** |

**判定：无敏感信息泄漏**。`listPayloadFiles` 的策略是「含入一切普通文件、只在根目录跳过自身两个输出」（`:63-83`，尤其 `:72` 的 `relative === ""` 守卫），这意味着**任何**进入载荷目录的东西都会被声明——它不是一个 allow-list。唯一实际后果是 `--collect-all` 把上游 vendored 测试套件带了进来：`python-agent/_internal/jsonschema/tests/**`（13 文件 185.6 KB）与 `python-agent/_internal/aiosqlite/tests/**`（5 文件 27.4 KB）。这不是安全问题（无秘密、无本仓库 fixture），但属于体积与攻击面的无谓扩张，且 `jsonschema/tests/fuzz_validate.py` 之类的测试工具会一并交付。

### 2.5 Q5：NOTICE 与许可证收集的一致性

**Python 侧：无归属缺口。** 归一化后对比 `agent/runtime/THIRD_PARTY_LICENSES.txt`（85 个 `## Name version` 段）与冻结载荷的 21 个 `*.dist-info`：

- 随包但未声明：**0**
- 声明但未随包：64（过度声明；含已删除的 `yukinal-agent-node`）
- 7 个随包包的段落只有元数据没有全文（`sqlite_vec`、`urllib3`、`langgraph_checkpoint*`、`opentelemetry_api`、`langchain_openai`、`langchain_google_genai`）——逐个核对其 wheel **本身不带任何 license 文件**，段落已记录 `License: MIT` / `Apache-2.0`，因此**不构成归属缺口**（`target/review-logs/probe7-gaps.log`：「real attribution gaps: 0」）。

**关于 `services/control-plane/pyproject.toml` 的抽查结论：它不在打包链路的覆盖范围内，因此不构成缺口。** 该项目的依赖（`fastapi>=0.115,<1`、`uvicorn>=0.30,<1`、`cryptography>=42,<52`、`websockets>=12,<19`、`sqlalchemy[asyncio]>=2.0,<3`、`pydantic-settings>=2.4,<3`、`alembic>=1.13,<2`，见 `:8-17`）**没有任何一个**进入冻结载荷（实测在 662 条载荷里搜索 `uvicorn`/`fastapi`/`sqlalchemy`/`alembic`/`asyncpg`/`starlette`/`pydantic_settings` 命中均为 **0**，`pydantic_settings` 也不在其中；载荷里的 `cryptography` 来自 Agent 自身的 `langchain-*` 依赖链，与 control plane 无关）。它是独立的服务端组件，不被 `package.mjs` 暂存，也不需要出现在 `THIRD_PARTY_LICENSES.txt` 中。**真正需要关注的不是「漏登记了 control plane 的依赖」，而是 `THIRD_PARTY_LICENSES.txt` 会登记根本不随包的构建期依赖（64 条过度声明，含 `pip`/`setuptools`/`pyinstaller`/`paramiko` 与已删除的 `yukinal-agent-node`）**——详见 M-5。顺带记录一个数字上的巧合风险：`uvicorn` 在声明文件里有 1 个 `## uvicorn …` 段（来自构建 venv 的传递依赖），若只看「声明文件里有、载荷里没有」很容易误判为缺口；按上面的口径它属于「过度声明」，与实际随包内容无关。

**NOTICE 本身有两处事实错误**（且 NOTICE 被逐字节复制进载荷，实测 `staged` 与仓库 `NOTICE` 的 SHA-256 相同，均为 `96228CB3…`）：

1. `NOTICE:28-32`：「The packaged desktop application includes the official Node.js v24.21.0 runtime… checked against the SHA-256 value recorded in `scripts/node-runtime.json`」。**`scripts/node-runtime.json` 不存在**（`git ls-files '*node-runtime*'` 为空），打包链路也**从不暂存任何 Node runtime**（载荷内 `/runtime/` 命中 0，`package.mjs:283-296` 无对应代码）。这是 Rust/Tauri 时代的遗留声明。
2. `NOTICE:9-12`：只声明 `JetBrains Mono` 与 Nerd Font 变体，但 `apps/desktop/src/assets/fonts/` 现在还有 `Orbitron-Variable.ttf` 与 `Orbitron-OFL.txt`（OFL，带自己的许可证文本）。该目录当前也不在载荷里（无 UI），但 NOTICE 作为仓库级声明已经与源码不符。

其余声明经核对**属实**：`NOTICE:17-23` 的固定依赖版本与 `native/CMakeLists.txt:55-56`（OpenSSL 3.5.9）、`:91`（nlohmann/json v3.11.3）、`:121`（libssh2 `f216bbe3…`）、`:134`（cpp-httplib v0.18.3）一致；`NOTICE:25-26` 的 `$RESOURCE/ssh-service/OPENSSL-LICENSE.txt` 与 `package.mjs:223-230,294` 一致。

### 2.6 Q6：CI 与本地 `pnpm check` 的差异

`check.mjs:38-75` 定义 12 个步骤，**全部** `required: true`（`:39,40,41,44,47,52,61,69,71,72,73,74`），CI 在 `check.yml:89-90` 只执行 `pnpm install --frozen-lockfile` + `pnpm check`。差异：

| 维度 | 本地 | CI |
| --- | --- | --- |
| CTest 构建目录 | `native/build-nmake`（`check.mjs:22`） | `native/build-check`（`check.yml:30`） |
| OpenSSH 容器测试 | OFF（本机 cache） | ubuntu **ON**（`check.yml:23`），需 docker + `ssh-keygen`/`ssh-keyscan` |
| 控制面测试 | **执行**（`services/control-plane/.venv` 存在） | **永远 skip**——CI 无任何创建该 venv 的步骤（`git grep control-plane .github/workflows/check.yml` 为空），`check.mjs:62-63` 走 skip 分支 |
| `agent` extras | 本机含全量 | `agent[agent,models,test]`（`check.yml:56`），**无 `package`** |
| 打包链路 | 可手动跑 | **完全不跑**：无 `pnpm package`、无 `--verify`、无 `build:python-agent`（`check.yml` 中除注释外无相关调用） |
| 触发范围 | — | `check.yml:17` 的 `if` 排除 tag push，注释明确「Release packaging is not implemented yet」 |

**本地绿 / CI 红的方向**：容器集成测试（`openssh_container_integration`，`native/CMakeLists.txt:302-312`，180 s 超时 + 需构建 Docker 镜像，且有网络依赖）只可能在 CI 上跑并失败；本地从零配置 `-DYUKINAL_FETCH_DEPS=ON` 的路径在 CI 上才被真正执行。
**CI 绿 / 本地红的方向**：控制面测试（CI 跳过、本地执行）与依赖完整度差异——本地未来可能因缺少某个 extra 而在 CI 通过。
**最关键的缺口**：`release artifact tests`（`check.mjs:44`）虽然被 CI 覆盖（11/11 通过），但**没有任何 CI 步骤会把模块用于真实载荷**——`pnpm package` 与 `--verify` 从未在 CI 里对真实产物执行过，因此 H-1（无 UI）和 H-2（新增文件不报）不可能被 CI 捕获。

**固定 SHA 可验证性**：`.github/workflows/check.yml` 共 7 处 `uses:`，全部为 40 位十六进制 + 版本注释（程序化核对 `target/review-logs/pins.mjs`）：

- `actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1`（`:32`、`:99`）
- `pnpm/action-setup@008330803749db0355799c700092d9a85fd074e9 # v6.0.9`（`:34`、`:104`）
- `actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0`（`:38`、`:107`）
- `actions/setup-python@83679a892e2d95755f2dac6acb0bfd1e9ac5d548 # v6.1.0`（`:43`）

格式上**完全合规**（无 tag/mutable ref），但**没有任何自动化的新鲜度/一致性检查**（例如 Dependabot 或定期对比），注释里的版本号也未被机器校验，存在注释与 SHA 漂移的空间。（本次会话无法联网访问 `github.com`/`api.github.com`——解析到非公网 IP 被拒——因此**未能**逐一点开注释所对的 release 标签核实「SHA ↔ v 号」的映射，此点标注为**待证实**。）

### 2.7 Q7：实际复现

所有命令**真实执行**，产物在 `target/review-logs/`（`target/` 已被 `.gitignore:13` 忽略，仓库根目录无新增文件）。完整批次：`target/review-logs/repro-batch.log`、`repro-r6.log`、`probe3-junction.log`、`probe4-metadata.log`、`probe5-refresh.log`、`probe6-licenses.log`、`probe7-gaps.log`、`probe8-junction.log`、`probe9-contamination.log`、`probe2-boundary.log`、`probe-integrity.log`。汇总见 第 6 节。

---

## 3. 发现清单（按「能挡住真实风险」排序）

### Critical

无。

---

### High

#### H-1 `pnpm package` 产出的载荷无法作为桌面应用启动：它根本不含 UI 资源

**位置**：`scripts/package.mjs:283-296`（暂存清单）、`:19-31`（文档化布局）、`:257-264`（`launch` 段）；`native/desktop_host/src/main.cpp:233-246,365-376`。

**证据**：`stage()` 只复制三件产物加三个文本文件——宿主（`:287`）、`agent/runtime` → `python-agent/`（`:288`）、`agent/runtime/THIRD_PARTY_LICENSES.txt`（`:289-292`）、SSH 服务（`:293`）、`OPENSSL-LICENSE.txt`（`:294`）、`NOTICE`（`:295`）、`LICENSE`（`:296`）。**没有任何一行引用 `apps/desktop/dist`**（`git grep 'desktop/dist' -- scripts/` 无命中），而该目录在仓库中存在（49 个文件，8,270,340 B）。

宿主的 UI 解析是**强制二选一**：

```cpp
// native/desktop_host/src/main.cpp:233-246
std::optional<std::string> resolve_ui_url(const Options& options, UiServer& ui_server) {
  if (options.dev_url.has_value() == options.ui_directory.has_value()) return std::nullopt;
  ...
  const auto index = root / "index.html";          // :242
  if (!std::filesystem::is_regular_file(index, error) || error) return std::nullopt;  // :243

// native/desktop_host/src/main.cpp:375-376
  const auto url = resolve_ui_url(*options, ui_server);
  if (!url) return 2;                               // 静默拒绝
```

**实测**（三次独立运行，互相印证）：

```
$ <payload>/yukinal-desktop-host.exe
exit=2  stdout=0B stderr=0B                          # 载荷内 0 个 index.html

$ <payload>/yukinal-desktop-host.exe --ui-dir <payload root>
exit=2                                                # 只传载荷里唯一的那个目录也不行

$ <payload>/yukinal-desktop-host.exe --ui-dir apps/desktop/dist --data-dir <tmp>
still running after 6s  => 被接受（随后被手动终止）    # 证明缺的正是这一份 UI 产物
```

`release-manifest.json` 的 `launch` 段（`package.mjs:257-264`）只列 `hostExecutable`、`agentEntry`、`sshServiceBinary`、`environmentVariables`——**没有 `uiDirectory`/`assets` 字段**，所以即使有人想按清单启动也不知道该指向哪里。`package.mjs:317-320` 的结束提示只提醒设置 `YUKINAL_AGENT_ENTRY` 与 `YUKINAL_SSH_SERVICE_BINARY`，同样不提 UI。

**为什么这是 High 而不是 Critical**：当前没有安装器与分发（`docs/release.md:15`），没人会拿到它；且 `--dev-url` 开发路径不受影响。

**影响**：文档把该产物称为「可分发布局」（`docs/cpp-desktop-handoff.md:69-83`、`docs/release.md:13`）。任何人按文档执行 `pnpm package`，会得到一个 `verify` 报「662 files 全部一致」、`smoke` 全绿、`--self-check` 退出 0 的目录，然后启动宿主得到**空输出 + 退出码 2**。没有任何 gate 会提示原因。这是「门禁全绿但产物不可用」的典型形态，也是本次审查最值得先修的一条。

**修复建议**：
1. `stage()` 增加 UI 产物输入，与另外三件一致地 fail-closed：默认 `apps/desktop/dist`（应要求 `index.html` 存在），支持 `--ui-dir` / `YUKINAL_DESKTOP_UI_DIR` 覆盖；复制到 `<payloadDirectory>/ui/` 或 `<payloadDirectory>/resources/ui/`。
2. `launchSection` 增加 `uiDirectory: "<payloadDirectory>/ui"`，并让 `package.mjs:317-320` 的输出提示包含 `--ui-dir`。
3. 让 `--verify` 至少断言 `launch` 指向的路径确实存在于载荷中（见 H-3 的元数据校验）。
4. 增加一个真正启动宿主的 smoke（`--ui-dir <staged ui>` + `--self-check` 已存在；再加一个「UI 目录可解析」的断言：`host --ui-dir <staged> --data-dir <tmp>` 在 N 秒内不退出即通过——本次审查用的就是这个判据）。

---

#### H-2 `verifyReleaseManifest` 只复核「清单里列出的路径」，新增文件与整块外来载荷目录完全不可见；注释与文档对此的宣称过强

**位置**：`scripts/lib/release-artifacts.mjs:184-268`（从不调用 `listPayloadFiles`）、`:8-9`（「matches its manifest exactly」）；`scripts/package.mjs:9-10`（「a file was added or removed」）。

**证据**（实测，`target/review-logs/repro-batch.log`）：

```
R3  在载荷根加入未声明的 BACKDOOR.exe
    -> Verified 662 file(s), 89935048 byte(s)   exit=0    # 未检出
R3  在任意子目录加入未声明文件
    -> [UNDETECTED] P1b（probe-integrity.log:2）
R4  并列一个整块陈旧载荷目录 yukinal-1.0.0-OLD/TROJAN.exe
    -> Verified 662 file(s)                     exit=0    # 未检出
```

机制很直接：`verifyReleaseManifest` 构造 `expected` 映射后（`:202-221`）只对 `expected` 迭代（`:230-238`、`:241-261`），**唯一**的「多余项」检测是「`SHA256SUMS.txt` 里有但清单里没有」（`:236-238`）——它比对的是**两个自身产物**，不接触文件系统。而 `listPayloadFiles`（`:63-83`）只在 `stage`/`--manifest` 时被调用（`package.mjs:235`）。

**影响**：
1. 供应链/构建污染无法被发现：在暂存与验证之间被任何步骤写入载荷的文件（后门 DLL、被替换的配置文件、`.env` 残留）不会让验证失败。
2. 交付目录里的陈旧载荷（上一版本的目录、`yukinal-1.0.0-old/`）不会被发现，而用户/安装器按目录名选择时可能选错。
3. `verify` 的返回 `{files, bytes}` 只统计清单条目（`:240-268`），因此一个膨胀的目录会报出与干净载荷**完全相同**的总数——观察者失去唯一可用于交叉核对的信号。
4. 注释宣称「a payload directory either matches its manifest **exactly**」（`:8-9`）与「fails when a byte changed or **a file was added** or removed」（`package.mjs:9-10`）：**「added」这一半是假的**，「removed」为真。

**关于 `release-artifacts.test.mjs` 的 11 个用例强度判定**（`scripts/lib/release-artifacts.test.mjs:74,83,90,107,118,159,167,177,184,211,227`，实测 11/11 通过）：

| 用例 | 覆盖 | 强度评价 |
| --- | --- | --- |
| `:74` `isSafeRelativePath` | `""`、`/abs`、`C:\`、`a/../b`、`../b`、`a//b`、`a/./b`、`a\b`、`a\0b` | **强**（但**未覆盖**保留设备名/ADS/尾点尾空格） |
| `:83` `sha256File` | 单文件摘要 | 强 |
| `:90` `listPayloadFiles` | 递归、排序、正斜杠、只在根跳过自身输出（`:97-98` 显式断言嵌套同名文件**应**被列出） | 强，且把「只跳根」这个设计写成了断言 |
| `:107` 校验和渲染/往返 | sha256sum 格式 + 解析报错 | 强 |
| `:118` `buildReleaseManifest` | 排序、去重、越界、非法 sha、多段 `payloadDirectory`、空 version | 强 |
| `:159` 接受匹配载荷 | 总数正确 | 基线 |
| `:167` 拒绝被篡改 | 单文件改写 → `size|sha256` | 强 |
| `:177` 拒绝缺文件 | `missing on disk` | 强 |
| `:184` 校验和不一致/不完整 | 空文件、单条不一致、清单外多一条 | 强 |
| `:211` 拒绝路径越界 | — | 强 |
| `:227` 验证后文档路径存在 | 三件产物路径 | 弱（几乎恒真，只证明 fixture 自身） |

**判定：用例强度与注释声称的「exactly」不一致。** 11 个用例中**没有一条**覆盖：新增/未列出文件、陈旧同级目录、联接/符号链接、权限位、`verify` 的返回计数是否会暴露多余文件、以及清单身份字段的可伪造性。用例质量本身很高（边界清晰、善用 `assert.rejects` 的正则），**缺口在覆盖面而非写法**。因此「11 个用例」不能作为「完整性契约已被充分测试」的证据。

**修复建议**：
1. 在 `verifyReleaseManifest` 末尾增加「无多余文件」与「无重解析点」检查（代码片段见 第 2.1 节）。
2. 把 `return { files: expected.size, bytes }` 改为同时返回 `scanned`（磁盘实际枚举数）并在两者不等时报错。
3. 补 4 个用例：新增文件（根/子目录各一）、陈旧同级目录、联接/符号链接指向载荷外、清单身份字段与源码不一致。
4. 更正 `release-artifacts.mjs:8-9` 与 `package.mjs:9-10` 的措辞，使其与实现的真实边界一致。

---

#### H-3 `--manifest` 会把磁盘现状「祝福」进契约：偷偷加入的文件变为「已验证」，且身份/启动字段原样复制

**位置**：`scripts/package.mjs:323-343`（`refreshManifest`）、`:328-340`（字段来源）。

**证据**（实测，`target/review-logs/repro-batch.log` R5 与 `probe5-refresh.log`）：

```
# 1) 偷偷加一个文件
$ Set-Content .../yukinal-1.0.0-win32-x64/BACKDOOR.exe
# 2) 按文档重建清单（这一步是公开的、被推荐的例行操作）
$ node scripts/package.mjs --manifest --out <dir>
Refreshed 663 file(s)                                  exit=0
$ grep BACKDOOR SHA256SUMS.txt
7b1578a8f41aa2a023717a549f25829232b9d41a9b773c807cfe8e5e4d6c34c7  yukinal-1.0.0-win32-x64/BACKDOOR.exe
# 3) 现在它「已验证」
$ node scripts/package.mjs --verify --out <dir>
Verified 663 file(s), 89935080 byte(s)                 exit=0
```

第二组：伪造身份字段后 `--manifest` 全部保留（`probe5-refresh.log`）：

```
seeded forgery: version=0.0.0-forged  agentVersion=9.9.9-forged  launch.hostExecutable=elsewhere/not-the-host.exe
$ node scripts/package.mjs --manifest --out <dir>
after `--manifest`:
  version        : 0.0.0-forged        (source truth: 1.0.0)
  agentVersion   : 9.9.9-forged        (source truth: 0.1.0)
  sourceRevision : 736f06a…            (re-derived)
  dirty          : true                (re-derived)
  launch         : {"hostExecutable":"elsewhere/not-the-host.exe"}
$ node scripts/package.mjs --verify --out <dir>
Verified 662 file(s)                                   exit=0
```

**影响**：`--manifest` 的定位是「在已暂存的载荷上重建清单与校验和」（`package.mjs:71-72`、`docs/cpp-desktop-handoff.md:77`），但它的实现是**无条件的 `collectPayloads` 重新枚举 + 原样复制身份字段**。这意味着：
- 任何在 `stage` 之后写入载荷的字节都会被合法地纳入「完整性契约」，验证随即通过——H-2 的盲区被 `--manifest` 从「检测不到」升级为「主动认可」。
- 清单里的 `version`/`agentVersion`/`platform`/`arch`/`launch` 与源码**没有绑定**，因此 manifest 不能被当作「这个载荷是什么」的可信陈述。它与 `src/version.ts`、`agent/pyproject.toml` 之间没有断言。

**修复建议**：
1. `refreshManifest` 从源码重新推导 `version`/`agentVersion`（复用 `readApplicationVersion`/`readAgentVersion`），与 `existing` 不符即 `fail()`。
2. 对 `payloadDirectory` 做同样的交叉检查（应为 `yukinal-<version>-<platform>-<arch>`）。
3. 把 `--manifest` 的语义收紧为「仅在载荷内容与 `existing.payloads` 完全一致时刷新 `sourceRevision`/`dirty`/`builtAt`」——即它应拒绝新增/删除文件的载荷，而不是吸收它。若确实需要「吸收」场景，另设显式的 `--adopt` 开关并要求更高的确认。
4. 让 `--verify` 校验 `launch.*` 指向的路径存在于载荷中，且 `payloadDirectory` 与 `version`/platform/arch 自洽。

---

### Medium

#### M-1 陈旧构建产物被静默接受：清单把整份载荷归因于 `HEAD`，但三件产物中两件早于 `HEAD`

**位置**：`scripts/package.mjs:161-181`（宿主查找）、`:183-196`、`:198-221`（均只做 `existsSync`，**无新鲜度检查**）；`:304-306`（`sourceRevision` = `git rev-parse HEAD`）。

**证据**（`target/review-logs/bundle-final/release-manifest.json` 与文件 mtime）：

| 暂存输入 | 大小 | 构建时间 | 与 `HEAD` 的关系 |
| --- | --- | --- | --- |
| `native/build-nmake/yukinal-desktop-host.exe` | 8,259,584 | 2026-10-17 **12:04:16** | 之后有 **9 个提交**改过 `native/desktop_host` |
| `native/build-nmake/yukinal-ssh-service.exe` | 5,656,576 | 2026-10-17 23:31:31 | 之后 `native/src` 无提交 |
| `agent/runtime/yukinal-agent/yukinal-agent.exe` | 22,654,058 | 2026-10-**15** 09:27:54 | 之后有 **29 个提交**改过 `agent/src` |

清单却写 `sourceRevision: 736f06a58d07e595fa79ac620d8ce116779bfe4c`（`HEAD`，提交于 2026-10-17 17:06:46），`dirty: true`。**`dirty: true` 只说明工作区脏，不说明二进制陈旧**——`dirty` 来自 `git status --porcelain`（`:145-148`），与产物构建时间无关。因此清单把一份由「10-15 的 Agent + 12:04 的宿主」组成的载荷标注为「`736f06a` 构建」。被漏掉的修复包括 `693ea8f`（`native/desktop_host/src/transfer_types.cpp:497-515` 现在在**所有**平台上拒绝保留设备名，而 12:04 的二进制里该逻辑仍被 `#ifdef _WIN32` 包着）与 `1bc0563`（拖拽暂存目录引用计数修复）。

**影响**：这是「打包正确性」最容易被忽视的一环——`verify` 会完美地证明「这份陈旧载荷没有被改动」。任何以 manifest 为发布依据的流程都会把旧二进制当成新提交的产物。

**修复建议**：在 `resolveHostExecutable`/`resolveAgentRuntime`/`resolveSshServiceExecutable` 里记录并输出每个输入的 `mtime` 与 `sha256`；与 `git log -1 --format=%cI -- <该输入对应的源码目录>` 比较，产物早于最后一次相关提交时**默认失败**，提供 `--allow-stale-inputs` 显式豁免（并把豁免写进 `manifest.notes`）。同时在 `launch`/新增的 `provenance` 段里逐件记录输入的 sha256，而不是只记 `HEAD`。

#### M-2 走查与校验对重解析点（junction/symlink）语义不一致：校验会跟随联接去验证载荷**之外**的字节

**位置**：`scripts/lib/release-artifacts.mjs:73`（`entry.isDirectory()`）、`:244`（`stat`）、`:256`（`createReadStream`）。

**证据**（真实 `mklink /J`，`target/review-logs/probe8-junction.log` 与 `probe3-junction.log`）：

```
Dirent ssh-service: isDirectory=false isFile=false isSymbolicLink=true
stat  isDirectory=true  isFile=false
lstat isDirectory=false isFile=false isSymbolicLink=true
descending through the junction reveals: hidden.dat

# 把 ssh-service/ 换成指向载荷外目录的联接（真实字节搬到载荷外）
listPayloadFiles(root)      = 662 entries（联接的 2 个条目已从走查结果消失）
manifest 条目仍在联接路径下 = 2
verifyReleaseManifest       = OK files=662 bytes=89935048   exit=0
```

机制：Node 把 junction 报告为 `isSymbolicLink=true`/`isDirectory=false`，因此 `listPayloadFiles` 的 `:73` 不递归（整棵子树从「磁盘真相」里消失）；而 `verifyReleaseManifest` 用 `stat`（`:244`）与 `createReadStream`（`:256`），二者**跟随**重解析点，于是它老老实实地校验了位于载荷目录之外的字节并宣布通过。走查与校验对同一棵树得出不同的集合，且没有任何一处检查重解析点。

**影响**：一个指向载荷外的联接会让「已暂存载荷」的字节实际来自仓库其它位置或另一块磁盘；验证通过但交付内容不确定。附带效应：走查与校验的集合不一致本身就该是一个错误信号，而当前它被静默吞掉。

**修复建议**：在 `listPayloadFiles` 与 `verifyReleaseManifest` 中统一使用 `lstat` 并拒绝任何 `isSymbolicLink()` 条目（或显式地把重解析点当作不安全项上报，例如 `payload path is a reparse point: <path>`）；两者对「目录」的判定必须使用同一语义。POSIX 符号链接路径在本次 win32 环境**未能实测**（标注待证实），但同一段代码路径适用。

#### M-3 `NOTICE` 有两处与当前源码不符的事实声明，而它被逐字节复制进载荷

见 第 2.5 节。要点：`NOTICE:28-32` 声称随包分发官方 Node.js v24.21.0 runtime 并以 `scripts/node-runtime.json` 校验完整性——**该文件不存在，打包链路也从不暂存任何 runtime**；`NOTICE:9-12` 的字体清单遗漏 `Orbitron-Variable.ttf` / `Orbitron-OFL.txt`。实测仓库 `NOTICE` 与载荷内 `NOTICE` 的 SHA-256 相同（`96228CB3…`），所以错误声明会随产物交付。

**修复建议**：删除 `NOTICE:28-32` 整段（Rust/Tauri 遗留），或改写为「当前不随包分发 Node runtime」；在 `NOTICE:9-12` 中补上 Orbitron。更根本的建议：让 `package.mjs` 在暂存 `NOTICE` 时校验其声明的文件确实存在（例如解析 `$RESOURCE/...` 与 `scripts/*.json` 引用），使 NOTICE 与载荷不会漂移。

#### M-4 `isSafeRelativePath` 接受 Windows 保留设备名、尾点/尾空格别名与 ADS 流；当前仅靠 `stat` 偶然 fail-closed

见 第 2.1 节 末段。实测接受 `CON`、`NUL`、`COM1`、`LPT1`、`aux`、`payload/CON`、`payload/NOTICE.`、`payload/NOTICE `、`payload/NOTICE:ads`（`probe2-boundary.log:1-12`）；这些名字在磁盘上因 Windows 语义而 `stat` 失败，于是被记为 `missing on disk` 而 fail-closed（`:21-26`）——**行为正确但理由不是契约**。仓库内部已有正确先例 `native/desktop_host/src/transfer_types.cpp:497-515`。

**修复建议**：在路径检查里显式拒绝保留设备名（含 stem 归一化与 `:`）、尾点、尾空格；补对应测试用例（现有用例 `:74` 未覆盖）。

#### M-5 许可证收集读的是构建 venv 而非冻结载荷；`agent/runtime` 根下的陈旧文件不会被清理但会被打包

见 第 2.3 节。两个具体问题：

1. `collect-python-licenses.py:8,29` 枚举构建 venv（85 个发行版），而载荷只含 21 个 → 64 条过度声明，包括**已删除组件**的残留元数据 `yukinal-agent-node 0.1.0`（仍在 `agent/.venv/Lib/site-packages/yukinal-agent-node-0.1.0.dist-info`）。反向缺口为 **0**（`probe6-licenses.log`、`probe7-gaps.log`），所以这不是法律风险，而是「随包声明不可作为实际清单使用」的准确性问题。
2. `build-python-agent.mjs` 无任何 `rm`；`--clean` 只清 `workpath`/cache（`PyInstaller/building/build_main.py:1160-1174`），`_make_clean_directory` 只清 `distpath/yukinal-agent`（`PyInstaller/building/utils.py:386-397`）。因此 `agent/runtime/` 根下的陈旧 `THIRD_PARTY_LICENSES.txt`（418306 B，2026-10-15）会被 `package.mjs:289-292` 无条件复制进载荷。**如果 PyInstaller 构建失败而许可证步骤未执行，载荷会带上一次构建的许可证文本。**

**修复建议**：`collect-python-licenses.py` 改为对**冻结载荷目录**（`--payload agent/runtime/yukinal-agent`）枚举 `*.dist-info`，或在打包时对载荷做一次「发行版名/版本集合」与声明段的双向比对；`build-python-agent.mjs` 在调用 PyInstaller 前显式清空 `agent/runtime/yukinal-agent` 与 `agent/runtime/THIRD_PARTY_LICENSES.txt`。

#### M-6 CI 对打包链路零覆盖，且控制面测试在 CI 永远被跳过

见 第 2.6 节。核心：`check.yml` 从不执行 `pnpm package`/`--verify`/`build:python-agent`（也因 `check.yml:56` 缺 `package` extra 而不可执行）；`check.mjs:62-63` 的控制面步骤在 CI 无 venv → 永远 skip；`check.yml:17` 排除 tag push。因此 H-1/H-2 这类「只有对真实产物运行才会暴露」的问题不可能被 CI 发现，而 11 个单测全绿会给出「打包已被覆盖」的错觉。

**修复建议**：新增一个（非矩阵、单平台即可的）打包 job：`pnpm build:python-agent`（需 `agent[...,package]`）→ 构建宿主与 SSH 服务 → `pnpm package` → `pnpm package -- --verify` → 断言载荷内存在 `index.html` → 启动宿主 smoke。这会把 H-1 变成 CI 红灯。

#### M-7 清单不记录权限/模式位，POSIX 上的权限与符号链接变化不可见（win32 未实测）

**证据**：清单条目键仅 `["path","size","sha256"]`（`probe2-boundary.log:41`）；`verifyReleaseManifest` 只判 `stats.isFile()`、`stats.size`、sha256（`:249-259`）。在 POSIX 上：可执行位被清除（产物无法启动）、条目被替换为指向相同字节的符号链接（内容校验通过但语义改变）、或目录权限变化，均不会导致 `verify` 失败。**本机为 win32，POSIX 模式位与符号链接两条探针标记为 `NOT-EXECUTED`**（`probe-integrity.log:3-4`），结论为**基于代码的推断，待 POSIX 复现确认**。

**修复建议**：在清单条目中加入 `mode`（POSIX 下 `stats.mode & 0o777`，win32 下省略或记录 `null`）并在校验时比较；对 `lstat().isSymbolicLink()` 一律拒绝（与 M-2 合并处理）。

---

### Low

#### L-1 上游 vendored 测试套件随包发布（`--collect-all` 副作用）

`python-agent/_internal/jsonschema/tests/**`（13 文件 185.6 KB）与 `python-agent/_internal/aiosqlite/tests/**`（5 文件 27.4 KB）会被 `--collect-all` 带入（`build-python-agent.mjs:43-51`），合计 18 个文件、213 KB。**不是秘密泄漏**（`.git`/`.private/`/brief/`node_modules`/`conftest.py` 均为 0），但包含 `jsonschema/tests/fuzz_validate.py` 这类测试工具。**修复建议**：在 PyInstaller 调用中加入 `--exclude-module jsonschema.tests --exclude-module aiosqlite.tests`（或 `--collect-all` 换为显式 `--collect-data`），并在 `package.mjs` 增加一条「载荷内不得出现 `/tests/` 或 `test_*.py`」的断言（当前无任何此类检查）。

#### L-2 `verify` 的返回值只统计清单条目，膨胀的载荷报出相同总数

`verifyReleaseManifest` 返回 `{ files: expected.size, bytes }`（`:240-268`），`package.mjs:356-358` 原样打印。因此一个多了 1 个文件、多了整块陈旧目录的载荷会打印与干净载荷**完全相同**的 `Verified 662 file(s), 89935048 byte(s)`（实测 R3/R4 输出完全相同）。**修复建议**：返回值加入 `scanned`（磁盘实际枚举数）与 `extra`（未列出文件列表），并在非零时失败。

#### L-3 `scripts/clean.mjs` 不清理 `target/` 与 `agent/runtime`

`clean.mjs` 只对 `packages/*/dist`、`apps/*/dist` 做 `rm -rf`，**不触碰** `target/`（含 `target/release/bundle`、`target/pyinstaller`）与 `agent/runtime/`。结合 M-2/L-2，陈旧载荷目录会长期留在打包默认输出根下并且对 `verify` 不可见。**修复建议**：`clean.mjs` 增加对 `target/release`、`target/pyinstaller`、`agent/runtime/yukinal-agent` 的清理（保留 `.gitkeep`），或在 `package.mjs` 的 stage 阶段清理输出根下的其它 `yukinal-*-*-*` 目录并给出警告。

#### L-4 `docs/release.md:7` 的「十个目标」已过期

`docs/release.md:7` 称「最近一次 CTest 的**十个目标**全部通过」，当前 `ctest -N` 为 **15** 个目标。（`docs/release.md` 不在本次写入范围内——task-5/FIX-2 的写入范围是 `CONTRIBUTING.md`/`README*.md`/`docs/acceptance-handoff.md`/`docs/cpp-desktop-handoff.md`，本报告仅记录未修改。）

---

## 4. 已排查但未发现问题的区域

1. **缺少任一暂存输入时确实 fail-closed 且不产出部分载荷**——含输出目录是否被创建（实测未创建），指引文本可执行。`package.mjs:154-159,187-192,214-219`。
2. **三模式在缺清单时全部非零退出**，错误信息含具体路径。`:326,348`。
3. **参数解析无注入面**：PyInstaller 全部参数为字面量（`build-python-agent.mjs:35-56`）；`package.mjs` 的参数只影响路径，且 `--out` 值以 `--` 开头时会被拒绝（`:103`）。
4. **载荷无敏感泄漏**：`.git`、`.private` 目录、仓库根那份外部输入 brief、`node_modules`、`conftest.py`、`.env`、`*.log` 的命中均为 0（`probe9b-contamination.log`）。
5. **`.gitignore` 覆盖充分**：`target/`（`:13`）、`*.log`（`:47`）、`.private/`（`:55`）、`agent/runtime/*` + `!agent/runtime/.gitkeep`（`:64-65`）。本次所有探针与日志都落在 `target/review-logs/`，`git status` 未出现任何新条目。
6. **单字节篡改、缺文件、校验和双向不一致、路径越界全部被检出**（R6、`:177`、`:184`、`:211` 用例均通过）。
7. **`release-artifacts.test.mjs` 的 11 个用例全部通过且写法规范**（`node --test` → `# tests 11 / # pass 11 / # fail 0`），缺口在覆盖面而非质量。
8. **NOTICE 的依赖版本声明与 `native/CMakeLists.txt` 逐条一致**（OpenSSL 3.5.9 / nlohmann-json v3.11.3 / libssh2 `f216bbe3…` / cpp-httplib v0.18.3），`OPENSSL-LICENSE.txt` 的暂存路径正确。
9. **随包 Python 发行版的许可证声明无缺口**：21 个 `*.dist-info` 全部有声明段；7 个「只有元数据无全文」的包其 wheel 本身不含许可证文件，段落已记录 SPDX 表达式。
10. **CI 的 7 处 `uses:` 全部为 40 位十六进制 + 版本注释**，无 mutable ref。
11. **`check.mjs:44` 确实把 `release-artifacts.test.mjs` 作为 `required: true` 的步骤**，因此 H-2 的测试缺口是「覆盖不足」而非「未接入门禁」。
12. **`probe-integrity.log:7-25` 的 `GAP isSafeRelativePath(undefined)` 是探针自身期望表的错误**（拒绝 `undefined` 是正确行为），不是源码缺陷；此处记录以免后续读者误读该日志。
13. **`services/control-plane/pyproject.toml` 的依赖不需要出现在随包声明里**：该服务的 7 个依赖（`fastapi`/`uvicorn`/`cryptography`/`websockets`/`sqlalchemy`/`pydantic-settings`/`alembic`）在 662 条载荷里命中为 0，`package.mjs` 也从不暂存 control plane，因此 Q5 的抽查口径下它不是缺口（只有 `python-agent/**` 里的 21 个发行版才是随包内容）。

---

## 5. 建议新增的测试

按价值排序（前 4 条直接对应 H-1/H-2/H-3）：

1. **`verifyReleaseManifest` 拒绝新增文件**：载荷加入未列出文件（根目录一处、子目录一处），断言抛出且错误信息包含该路径。同时反向断言 `listPayloadFiles` 能被 `verify` 使用（即实现改为枚举磁盘）。
2. **`verifyReleaseManifest` 拒绝重解析点**：在支持的环境建立目录联接/符号链接指向载荷外，断言抛出。（win32 可用 `mklink /J`；POSIX 用 `symlink`。）
3. **`refreshManifest` 拒绝与源码不一致的身份字段**：伪造 `version`/`agentVersion` 后运行 `--manifest`，断言非零退出；并断言 `payloadDirectory` 与 `version`/platform/arch 自洽。
4. **打包产物可启动**：`host --ui-dir <staged ui> --data-dir <tmp>` 在 N 秒内不退出；`host`（无参数）退出码为 2 且输出为空这一现有行为也应被固化为断言，以防将来「静默拒绝」被改成「崩溃」。
5. **载荷成员断言**：载荷内不得出现 `/tests/`、`test_*.py`、`.env`、`*.log`、`node_modules`（当前无任何此类检查）。
6. **`isSafeRelativePath` 的 Windows 边界用例**：`CON`/`NUL`/`COM1`/`aux`/`NOTICE.`/`NOTICE `/`NOTICE:ads` 全部应为 `false`。
7. **陈旧输入检测**：产物 `mtime` 早于最后一次相关提交时 `package.mjs` 应失败（可注入假 mtime 测）。
8. **许可证声明 ↔ 载荷双向比对**：对 `agent/runtime/yukinal-agent` 的 `*.dist-info` 集合与 `THIRD_PARTY_LICENSES.txt` 的 `## Name` 集合做双向差集断言为空（当前为「单向 + 64 条过度声明」）。

---

## 6. 复现命令与产物

全部在 `D:\yukinal`、分支 `codex/ui-layout-direction`、`HEAD=736f06a` 上执行；产物全部位于 `target/review-logs/`（`.gitignore:13` 忽略）。

**A. 三条主命令（真实执行，全部退出 0）**

```bash
node scripts/package.mjs --out target/review-logs/bundle-final
#   Staging yukinal-1.0.0-win32-x64 / Staged 662 file(s) / Wrote …SHA256SUMS.txt and …release-manifest.json
node scripts/package.mjs --verify --out target/review-logs/bundle-final
#   Verified 662 file(s), 89935048 byte(s) against …release-manifest.json
node scripts/package.mjs --manifest --out target/review-logs/bundle-final
#   Refreshed 662 file(s) in …release-manifest.json

pnpm package            # → Staged 662 file(s) into target/release/bundle/…            exit=0
pnpm package -- --verify# → Verified 662 file(s), 89935048 byte(s)                     exit=0
pnpm release:manifest   # → Refreshed 662 file(s)                                      exit=0
```

**B. 载荷不可启动（H-1）**

```powershell
target\review-logs\bundle-final\yukinal-1.0.0-win32-x64\yukinal-desktop-host.exe
#   exit=2  stdout=0B  stderr=0B          ← 静默拒绝（main.cpp:234,376）
#   载荷内 index.html 数量 = 0
…yukinal-desktop-host.exe --ui-dir <payload root>          # exit=2
…yukinal-desktop-host.exe --ui-dir apps\desktop\dist --data-dir <tmp>
#   still running after 6s → UI 目录被接受；证明缺的正是这份资源
```

**C. 完整性边界（H-2 / H-3）：`target/review-logs/repro-batch.log`、`repro-r6.log`**

```powershell
pwsh -NoLogo -NoProfile -File target\review-logs\repro-batch.ps1   # R1–R9
pwsh -NoLogo -NoProfile -File target\review-logs\repro-r6.ps1      # 单字节篡改
```

| 步骤 | 操作 | `--verify` 结果 |
| --- | --- | --- |
| R1/R2 | 全新暂存 + 验证 | `662 files, 89935048 bytes` exit=0 |
| R3 | 载荷根加入 `BACKDOOR.exe` | `662 files, 89935048 bytes` **exit=0（未检出）** |
| R4 | 并列 `yukinal-1.0.0-OLD/TROJAN.exe` | `662 files, 89935048 bytes` **exit=0（未检出）** |
| R5 | `--manifest` 后重新验证 | `663 files, 89935080 bytes` exit=0（**后门已被祝福**） |
| R6 | 篡改 `yukinal-desktop-host.exe` 第 100 字节 | 报 sha256 不符，**exit=1（检出）** |
| R7 | `--host-exe does/not/exist.exe` | 退出 1，**输出目录未创建** |
| R8 | 无清单时 `--verify` / `--manifest` | 各退出 1 |
| R9 | 载荷内 0 个 `index.html`；宿主退出 2 | — |

**D. 联接跟随（M-2）**：`target/review-logs/probe3-junction.log`、`probe8-junction.log`

```powershell
node target\review-logs\probe8-junction-classify.mjs   # Dirent/stat/lstat 三态语义
node target\review-logs\probe3-junction.mjs            # 走查 662 vs 校验 OK 662，且 2 条目标准确落到载荷外
```

**E. 清单元数据可伪造（H-3）**：`target/review-logs/probe4-metadata.log`、`probe5-refresh.log`

```powershell
node target\review-logs\probe4-manifest-metadata.mjs            # 10 个字段全部 UNDETECTED
node target\review-logs\probe5-manifest-refresh.mjs <bundle-dir> # version/agentVersion/launch 原样保留，verify 仍 exit=0
```

**F. 许可证一致性（Q5）**：`target/review-logs/probe6-licenses.log`、`probe7-gaps.log`

```powershell
node target\review-logs\probe6-licenses.mjs     # 声明 85 / 随包 21 / 发了未声明 0 / 声明未发 64
node target\review-logs\probe7-license-gaps.mjs # real attribution gaps: 0
```

**G. 载荷污染（Q4）**：`target/review-logs/probe9b-contamination.log`（干净载荷，662 条）、`probe9-contamination.log`（R5 之后，663 条）

```powershell
node target\review-logs\probe9b-contamination-clean.mjs target/review-logs/bundle-final/SHA256SUMS.txt
node target\review-logs\probe9-contamination.mjs
#   .git/.private/brief/node_modules/conftest.py/.env/*.log = 0；tests/ = 18；test_*.py = 9
```

**H. 固定 SHA 与门禁（Q6）**：`target/review-logs/pins.mjs`

```powershell
node target\review-logs\pins.mjs                       # 7/7 为 40-hex + 版本注释
node --test scripts/lib/release-artifacts.test.mjs     # # tests 11 / # pass 11 / # fail 0
node scripts/package.mjs --help                        # 模式与选项文本
node scripts/smoke-python-agent.mjs --executable <payload>/python-agent/yukinal-agent.exe
#   PASS packaged Python Agent 0.1.0, provider/MCP imports, and JSON-RPC stdio
```

---

## 7. 附：本机环境限制与可复核性说明

1. **WebView2 SDK 版本与本机构建树陈旧（非源码缺陷）**：`native/CMakeLists.txt:358` 固定 `WEBVIEW_MSWEBVIEW2_VERSION = "1.0.2903.40"`，但本机已下载的 `native/build-vs/_deps/microsoft_web_webview2-src/Microsoft.Web.WebView2.nuspec` 是 `1.0.1150.38`，且本地头文件中 `ICoreWebView2Settings9` 出现 **0** 次——而 `native/desktop_host/src/windows_window_frame.cpp:296-302` 正是用它来实现非客户区拖拽。因此完整的 `cmake --build native/build-nmake` 在本机**无法完成**。**这不影响本次审查**：`pnpm package` 复用的是已存在的 `native/build-nmake/yukinal-desktop-host.exe` 与 `yukinal-ssh-service.exe`（`package.mjs:58-64` 的 `NATIVE_BUILD_DIRECTORIES[0]`），三件产物齐全，因此 stage/verify/manifest 都真实执行成功。这也正是 M-1 的现实背景：能打包，是因为旧二进制还在。
2. **POSIX 专属探针未执行**：符号链接指向载荷外、`chmod` 清除可执行位两项在 win32 上无法得到有意义结果，标记为 `NOT-EXECUTED`（`probe-integrity.log:3-4`）。M-7 的结论是**基于代码的推断**，需要在 Linux/macOS 上复现确认；M-2 的联接版本已在 win32 实测（junction 与 symlink 在 Node 的 `Dirent`/`stat`/`lstat` 语义上同类）。
3. **无法联网核实 action 注释**：本会话对 `github.com` / `api.github.com` 的请求被拒（解析到非公网 IP），因此「SHA ↔ 版本注释」的映射**未能独立核实**，仅确认了 SHA 的格式合规（40 位十六进制）。
4. **`pnpm release:manifest` 的副作用（预期，已确认无害）**：它会重写 `target/release/bundle/release-manifest.json` 的 `sourceRevision`/`builtAt`。该文件位于被忽略的 `target/`（`.gitignore:13`），不进入版本控制，也不影响工作区。本次执行后 `git status --porcelain` 仅剩 task-5/FIX-2 的 5 个文档改动与 `docs/review/`、`ssh_agent_development_brief.md` 两个既有未跟踪项。
5. **本报告的写入范围**：仅 `docs/review/r2-release.md`。未修改 `scripts/**`、`native/**`、`NOTICE`、`docs/release.md` 或任何跟踪文件；`docs/release.md:7` 与 `NOTICE` 的问题以「发现 + 建议」形式记录，留给对应写入范围的负责人处理。
