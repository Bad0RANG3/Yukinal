# Yukinal 审查后修复说明（2026-10-18）

配套文档：[三轮审查总报告](./review-report.md) ｜ [原始报告索引](./review/README.md)

本文记录**改了什么、为什么改、怎么验证的**。每个修复都有实测验收，且尽量附
**反向验证**（故意让门禁失败以证明它真的会失败），而不是只跑一次绿。

**18 项全部完成**（FIX-1 – FIX-18）。最后一项 FIX-11（桌面宿主接通 keyboard-interactive）
在本轮收尾阶段落地：`server.auth_challenge` 的**生产者**终于存在了。

## 目录

1. [修复总览](#1-修复总览)
2. [FIX-1 基线门禁失败](#2-fix-1-基线门禁失败)
3. [FIX-2 文档与仓库卫生](#3-fix-2-文档与仓库卫生)
4. [FIX-3 门禁与 CI 加固](#4-fix-3-门禁与-ci-加固)
5. [打包链路（FIX-4、FIX-12、FIX-13、FIX-14）](#5-打包链路fix-4fix-12fix-13fix-14)
6. [FIX-5 控制平面两条 P0](#6-fix-5-控制平面两条-p0)
7. [第二批修复（FIX-6 到 FIX-11）](#7-第二批修复fix-6-到-fix-11)
8. [写入面与并发纪律](#8-写入面与并发纪律)
9. [修复后门禁](#9-修复后门禁)
10. [未修项与遗留代价](#10-未修项与遗留代价)

---

## 1. 修复总览

| 修复 | 对应发现 | 改动文件数 | 核心验证 |
| --- | --- | --- | --- |
| **FIX-1** | R1 基线门禁失败（CTest 2 项） | 0（源码无需改） | 重建后 `15/15`，`git diff` 为空 |
| **FIX-2** | 文档与仓库现状不一致（4 处） | 6（含删除孤儿目录） | 三条快 gate green |
| **FIX-3** | python H-1、python H-2、`desktop_host` 静默 skip | 3 | Agent `100 → 153`；控制平面从「静默绿」变硬失败；CTest `15 → 13` |
| **FIX-4** | release H-1、release H-2、release M-2、M-4 | 5 | 载荷 `662 → 711` 含 `ui/index.html`；注入 `BACKDOOR.exe` 后 `--verify` 非零退出 |
| **FIX-5** | control-plane P0-3、P0-4（`expire`） | 6 | 控制平面 `129 → 139`；修复前源码还原后同一套测试 `FAILED (failures=7)` |
| **FIX-12** | release M-5（通知按构建 venv 枚举，85 → 21 个发行版） | 2 | 幻影包残留 0；空载荷 / 仅 exe 载荷硬失败；通知路径不再烧入绝对路径 |
| **FIX-13** | release M-1（旧清单把整份载荷归因于 `HEAD`） | 3 | 陈旧产物 exit 1 **且不创建输出目录**；provenance 被 `--verify` 与 `--manifest` 双向校验；测试 `18 → 24` |
| **FIX-14** | release L-3（`clean` 不覆盖 `target/` 与冻结 Agent） | 1 | 恰好 7 条命中、0 条被跟踪文件、`.gitkeep` 幸存 |
| **FIX-6** | control-plane P0-1 / P0-2 / P0-4 剩余 / nonce 上限 | 13（含 1 个新迁移） | 测试 `139 → 148`；跨租户同签名 `true → 401`；禁用租户泄漏 `6 → 0`（24/24 路由）；nonce 落库 `200 行 → 128 行`、100 000 字符 → 0 行；逐项还原 5/2/7/2 红 |
| **FIX-8** | native `exit_code` 语义（H-1）+ SIGPIPE（H-2） | 6 | CTest 5/5；修复前二进制上分别复现**假成功**、**假失败**、状态被抹成 `null`；SIGPIPE 只交付源码级护栏（本机无 Linux） |
| **FIX-10** | 契约诚实化（13 条 verbatim 响应 / 9 个零生产者通道 / Rust-Tauri 因果依据） | 57 | shared `135 → 142`、desktop `323 → 325`；容错构造恒等化后 shared 4 红 / desktop 1 红；13 条转发 pre-fix 13/13 拒 → post-fix 13/13 收 |
| **FIX-7** | Agent 四条「永久停在运行中」（H1/H2/H2b/H3）+ H4 的 sync 放大后果 + `policyId` 边界 | 2 | Agent `153 → 164`（skip 数不增）；**Lead 独立复现**：仅替换 HEAD 的 `sidecar.py` 后 11 条新用例 `FAILED (failures=8, errors=1)` |
| **FIX-18** | 给 FIX-7 的两个安全网补真读者（只动测试） | 1 | Agent `164 → 167`；AG-1 变异（删去重）**1 条红**（此前 0）、AG-3（记账前移）**3 条红**（此前 0）、AG-4 只红 1 条（证明钉的是另一根梁）；`sidecar.py` 哈希与 FIX-7 交付时逐字节一致 |
| **FIX-15** | 桌面策略选择器诚实化（`policyId` 不再进请求） | 3 | desktop **325** 只增不减；`requestPolicyId` 恒返回 `undefined`，选择区禁用并附说明；断言「选任何内建策略都不出现该字段」 |
| **FIX-9** | 桌面宿主：未知 `approvalId` 显式报错、`decision` 白名单、`mcp_server_list` 真状态、`consume` 有界 | 8 | 6 个 desktop ctest target **6/6 Passed**；四轮「修复前失败」对照（第 4 轮是**挂死 600 s 被 kill**）；反面护栏「sidecar 自有审批仍转发、`approvals.log` 恰为 `["approve_once"]`」 |
| **FIX-16** | 给 SIGPIPE 屏蔽补第二个读者（源码级护栏） | 1 | `yukinal_ssh_tests` 63 → **64**；**5 个变异全部变红**（删守卫 2 红、挪位置 2 红、`SIG_DFL` 1 红、错文件 1 红、相对路径 4 红）；Linux 行为级仍未闭环，如实标注 |
| **FIX-17** | nonce 上限：把「声称的界」变成真的界 | 2 | 6 组 barrier 矩阵 **overshoot = 0**（此前 188−128=60），每组恰好接受 `headroom` 个；控制平面 148 → **149**；README 与 docstring 降级为「未认证垃圾写入的界，非速率限制」 |

| **FIX-11** | 桌面宿主接通 keyboard-interactive（`auth_prompt` → `server.auth_challenge` → `server_auth_respond` 回写 PTY） | 4 | `native/**` 一侧 +543/−24 行；CTest **13/13**（`desktop_host_database_tests` 9.04 s、`sidecar_tests` 1.74 s）；两个变异（去掉条数校验 / 挑战不复用一次）各自让对应断言变红 |
专门说明：**这批修复不是一次性完成的**。FIX-1 – FIX-5 之后，FIX-12 – FIX-14 由一轮对抗式证伪
改写过（见 5.7），FIX-6 / FIX-8 / FIX-10 又各自由独立审查者做「失败面已转移」的差异回归
（见第 7 节）。本文把「被自己的验证推翻过的部分」单列，而不是只留最终结论。

---

## 2. FIX-1 基线门禁失败

**现象**：`node scripts/check.mjs` 在第 6 步「configured native CTest suite」失败（exit 8）——
`caption_hit_test_tests` 报 `***Not Run`（找不到可执行文件），`desktop_host_sidecar_tests` 报
`***Failed`（`native/desktop_host/tests/test_sidecar.cpp:254` 的「missing-provider」断言）。

**根因**：**陈旧构建树，不是源码缺陷。** 测试期望与实现行为一致，只是 `native/build-nmake` 里
两个目标没有被重建。

**修复**：重建这两个目标。

```
$ cmake --build native/build-nmake --target yukinal_caption_hit_test_tests --config Release
$ cmake --build native/build-nmake --target yukinal_desktop_host_sidecar_tests --config Release
$ ctest --test-dir native/build-nmake -C Release      →  15/15 passed
$ git diff                                            →  空（未改源码）
```

**结论**：这条能作为「**门禁红不等于源码坏**」的实例——先看构建树新鲜度，再怀疑代码。

---

## 3. FIX-2 文档与仓库卫生

### 3.1 仓库不再依赖未跟踪的任务输入文件

`ssh_agent_development_brief.md` 是外部任务输入（含私有计划），从未被跟踪，但
`CONTRIBUTING.md:3` 曾引用它。`check-docs-links.mjs` 会扫描「未跟踪但未被忽略」的 markdown，
所以这个文件一旦被删除或被 `.gitignore` 忽略，`CONTRIBUTING.md` 立刻变成死链。

**决策**：不把它纳入仓库（它是外部输入），改为**让仓库不再依赖它的存在**。
`CONTRIBUTING.md` 改引仓库内文档（`docs/rewrite-cpp-python.md` 与 `docs/cpp-desktop-handoff.md`），
其它文档保留「任务输入」说明但明确指出**该文件不在仓库内**，且构建与门禁都不依赖它。

### 3.2 删除孤儿构建产物目录

`apps/agent/` 是 legacy Node agent 删除后留下的**孤儿产物目录**（0 个被跟踪文件，约 1.12 MB 的
`dist/` + `node_modules/`，全部被 `.gitignore` 覆盖）。`docs/changelog.md` 已声称它被删除，
而磁盘上仍在，会误导贡献者。**已删除**；`apps/` 现在只含 `desktop`。

### 3.3 如实描述门禁覆盖范围

`CONTRIBUTING.md` 与 `README.md` / `README.zh-CN.md` 原先声称 `pnpm check` 覆盖「类型检查」等，
但**从未提及它不包含任何 lint / 格式化 / 静态分析**（既无 C++ 的 clang-format / clang-tidy / cppcheck，
也无 JS/Python 的 eslint / ruff / mypy）。现在三份文档都显式写明这一点，并补齐了实际存在的步骤
（命令运行器测试、桌面前端打包等）。

### 3.4 CTest 目标数与测试数的过期声明

多次修复共同造成的过期数字已按**保留历史值 + 追加带日期的复核说明**处理，而不是直接覆盖：

- `docs/acceptance-handoff.md` 的 CTest 清单改为真实的 **13** 个目标（按 `ctest -N` 顺序），
  并附修正 blockquote 解释「原先写 10 个、并把两个重复注册当独立目标」；
- 测试数 `132 → 153`、目标数 `15 → 13`（`:28`、`:139`、`:258`、`:332` 等处）；
- `docs/release.md:7` 的「十个目标」→ **13**，并把「控制平面步骤只在 venv 存在时执行」改为
  新的两级探针语义 + Agent 全量入闸；
- `native/README.md` 的「six registered CTest targets」保留为 2026-10-15 的历史记录，
  追加 2026-10-18 的更正说明（当前 13 个，另外两个模块并入 `python_agent_tests`）。

---

## 4. FIX-3 门禁与 CI 加固

这是三条系统性断层里第一条（**门禁与实际执行之间的落差**）的直接修复。

### 4.1 Agent 全量测试入闸

**修复前**：CTest 里唯一注册 Agent 测试的目标用 glob `test_agent_*.py`，只跑到 **100** 个测试。
`test_network_snapshot.py` / `test_provider_store.py` / `test_reasoning_effort.py` /
`test_system_snapshot.py` 四个模块（**18 个测试**）在 `native/CMakeLists.txt`、`scripts/check.mjs`、
`.github/workflows/check.yml`、`package.json` 中命中数**均为 0** —— 任何门禁都不执行它们。

**修复**：`native/CMakeLists.txt` 的 glob 改为 `test_*.py`，并**删除两个重复注册**
（`python_client_tests` 与 `service_http_integration`，它们的 glob 已被 `test_*.py` 覆盖）。
同时给该目标注入 `YUKINAL_SSH_SERVICE_BINARY`，使 `test_service_integration` 的 4 个用例
**真正执行**而不再静默 skip。

三段证据说明这不是「把 skip 换个地方」：

```
旧 glob  test_agent_*.py                     → Ran 100 tests            OK
仅换 glob，不注入二进制                        → Ran 153 tests            OK (skipped=4)   ← 4 个仍 skip
改 glob + 注入 YUKINAL_SSH_SERVICE_BINARY     → Ran 153 tests            OK               ← 0 skip
```

CTest 目标数 `15 → 13`，但这是**去重 + 净增 18 个覆盖**：原 15 个里有 2 个是同一批测试的重复计数，
同时有 18 个测试此前完全无门禁。另外新增一个 `required: true` 且**没有 `skip` 字段**的门禁步骤
「Python Agent tests (full unittest discovery)」，探测链为
`agent/.venv/Scripts/python.exe` → `agent/.venv/bin/python` → `YUKINAL_AGENT_PYTHON` → `python`，
探测失败即 `exit 2` 并打印修复命令。

### 4.2 控制平面从「静默绿」改为硬失败

**修复前**：`scripts/check.mjs` 里控制平面步骤同时带 `required: true` 与 `skip: !controlPlanePython`，
而 `skip` 分支先于 `required` 生效（skip 时直接 `continue`，`failures` 不自增、退出码 0）——
**「静默绿」**。

**修复**：两级探针。core = `fastapi, sqlalchemy, alembic, pydantic_settings`；full = core + `httpx, aiosqlite`。
只有「所有候选解释器都缺 core」时才允许 skip；以下三种情况一律 `exit 2` 并打印
`python -m pip install -e "services/control-plane[test]"`：

- 显式指定的 `YUKINAL_CONTROL_PLANE_PYTHON` 存在但不可用；
- core 在但缺 test extra；
- 设置了 `YUKINAL_CONTROL_PLANE_REQUIRED=1`。

CI 同步补齐：新增 `python -m pip install -e "services/control-plane[test]"`（不装 `postgres`，
CI 只跑 SQLite 路径），并给 `pnpm check` 那一步加 `env: YUKINAL_CONTROL_PLANE_REQUIRED: "1"`。

### 4.3 反向验证（证明它真的会红）

| 场景 | 期望 | 实测 |
| --- | --- | --- |
| 坏 `YUKINAL_CONTROL_PLANE_PYTHON` + `REQUIRED=1` | 红 | `✗ ... is set but cannot run \`import fastapi, ...\``，exit 2 |
| 所有候选都无 core + `REQUIRED=1` | 红 | exit 2 |
| core 在但缺 test extra | 红 | exit 2，提示装 test extra |
| Agent 无可用解释器 | 红 | exit 2，**绝不静默 skip** |
| 唯一允许 skip 的情形（处处都没装） | 绿 | exit 0 |

`exit 2` 专用于「环境无法按配置运行门禁」；`exit 1` 仍保留给「步骤跑了但失败」。

### 4.4 一处未改的既有语义（刻意保留）

`YUKINAL_SSH_CMAKE_BUILD_DIR` 指向不存在的目录时，CTest 步骤**仍会跳过并报绿**。
这一条**没有改**，因为它本来就只该在构建存在时跑；而且实测表明它**不再连累**其它步骤：

```
── configured native CTest suite: skipped (no configured CMake test build (see README.md))
── control plane tests (SQLite)                   Ran 129 tests OK   ✓
── Python Agent tests (full unittest discovery)   Ran 153 tests      ✓
check: green   exit=0
```

即「构建目录不可用 → 诚实 skip；控制平面与 Agent 照常执行」。这是**已知且接受的边界**，
不是漏洞——它无法掩盖控制平面或 Agent 的失败。

### 4.5 一条需要留意的操作细节

为了让修改后的测试注册生效，必须重新生成 CMake 配置（`cmake -S native -B native/build-nmake`）。
本次重新生成**没有改变任何交付相关配置**，可用两条独立证据核对：

```
CMAKE_BUILD_TYPE:STRING=                      (空，与重新生成前一致)
YUKINAL_BUILD_TESTS:BOOL=ON
YUKINAL_ENABLE_AGENT_TESTS:BOOL=ON
YUKINAL_ENABLE_SSH_MOCK_TEST:BOOL=ON
YUKINAL_BUNDLE_WINDOWS_OPENSSL:BOOL=ON
Python3_EXECUTABLE:UNINITIALIZED=D:/yukinal/agent/.venv/Scripts/python.exe
CMAKE_CXX_COMPILER:STRING=.../Community/VC/Tools/MSVC/14.44.35207/bin/Hostx64/x64/cl.exe

# 生成时间之后被重建的 *.exe/*.obj/*.lib 数量 = 0
```

即配置与编译器逐字不变，且 `ctest 13/13` 跑的是**同一批未变的二进制**。

---

## 5. 打包链路（FIX-4、FIX-12、FIX-13、FIX-14）

### 5.1 FIX-4 H-1：载荷根本不含 UI，产出的「可发布包」无法启动

**修复前**：`stage()` 只暂存宿主 / 冻结 Agent / SSH 服务 + 三个文本文件，**没有任何一行引用
`apps/desktop/dist`**。而宿主的 UI 解析强制二选一（`--dev-url` / `--ui-dir`），
`--ui-dir` 还要求 `<dir>/index.html` 可读，否则 `return 2` 且**零输出**。所以载荷能「校验全绿」
却永远起不来。

**修复**：新增 `resolveUiDirectory()`，默认 `apps/desktop/dist`，支持 `--ui-dir` 与
`YUKINAL_DESKTOP_UI_DIR` 覆盖，缺 `index.html` 即 fail-closed；复制到 `<payloadRoot>/ui/`。
`launchSection()` 增加 `uiDirectory` 字段，启动提示改为 `--ui-dir <payload>/ui`。
另外对 staging 源统一拒绝符号链接 / NTFS 联接（`cp` 会保留链接而不是解析它）。

**实测**：

```
$ node scripts/package.mjs --out target/lead-verify-bundle
Staged 711 file(s)                     ← 原 662，+49 UI 文件
Launch it with:
  .../yukinal-desktop-host.exe --ui-dir .../yukinal-1.0.0-win32-x64/ui
with YUKINAL_AGENT_ENTRY ... The host exits 2 (silently) when --ui-dir is missing
  or its index.html is unreadable.

$ node scripts/package.mjs --verify --out target/lead-verify-bundle
Verified 711 file(s), 98205388 byte(s), 711 scanned     exit=0
```

宿主启动验证：用 `--ui-dir <payload>/ui` 启动后 **8 秒仍存活**；负对照（同一 exe 不给 `--ui-dir`）
得到 `exit 2` / `0 B` 输出 —— 证明缺的正是这一份 UI 产物。

### 5.2 H-2：`--verify` 看不见「清单之外的文件」

**修复前**：`verifyReleaseManifest` **从不调用** `listPayloadFiles`，只迭代清单里的条目；
唯一的「多余项」检查是「`SHA256SUMS.txt` 有但清单没有」，它不接触文件系统。因此新增文件、
整块陈旧同级载荷目录、联接逃逸**全部放行**。

**修复**：反向枚举磁盘求差集，非空即失败并列出路径（最多列 10 条）；返回值增加 `scanned`，
`package.mjs` 同步输出 `files / bytes / scanned`，两者不等即可看出异常。

**实测**（Lead 独立复跑，全部对抗性构造）：

```
A) 载荷根放一个未声明文件
   - 1 file(s) on disk are not in the manifest: .../SNEAKED.txt          exit=1
B) 删除后恢复
   Verified 711 file(s), 98205388 byte(s), 711 scanned                   exit=0
C) 篡改 ui/index.html 一个字节
   - .../ui/index.html: sha256 8e0560fa... disagrees with the manifest    exit=1
D) 还原后再次校验
   Verified 711 file(s), 98205388 byte(s), 711 scanned                   exit=0
E) 载荷内放一个指向载荷外的 NTFS 联接
   - 1 file(s) on disk are not in the manifest: outside/leak.dat          exit=1
   - payload path is a reparse point: .../ui-link
F) 删除联接后
   Verified 711 file(s), 98205388 byte(s), 711 scanned                   exit=0
```

### 5.3 M-2：重解析点语义统一

`listPayloadFiles` 与 `verifyReleaseManifest` 统一用 `lstat` / dirent 语义判定，遇到
`isSymbolicLink()`（在 Windows 上对符号链接与联接都为真）一律作为**不安全项**失败，绝不跟随。
这条修复的边界由 Lead 用真实联接独立验证过：

```
$ cmd /c mklink /J <probe>\payload-link <probe>\outside
dirent isSymbolicLink = True   isDirectory = False
lstat  isSymbolicLink = True   isDirectory = False
stat   isDirectory    = True   ← 只有 stat 会跟随
```

即旧代码的错误形态是「**静默丢弃**」（`isFile()` 与 `isDirectory()` 都为假），而不是「跟随」。

**一个自我降级/纠正**：task-9（R2 发布审查）曾说 `isSafeRelativePath` 接受 Windows 保留名
「靠 `stat` 偶然 fail-closed」，这个措辞**给了错误的安心感**——`NOTICE.` / `NOTICE ` 在 Win32 上
会被规范化成 `NOTICE`，`stat` 其实是**成功**的。现在这些在**路径层**就被拒绝。

### 5.4 M-4：`isSafeRelativePath` 收紧

拒绝 Windows 保留设备名（`CON`/`PRN`/`AUX`/`NUL`/`COM1-9`/`LPT1-9`，大小写不敏感、
含 `payload/CON` 形式）、尾点（`NOTICE.`）、尾空格（`NOTICE `）、含 `:` 的 ADS（`NOTICE:ads`）、
UNC 与驱动器路径、控制字符。**误报验证**：对当前 711 条真实清单路径 + 149 个目录分量 +
launch 字段 = **861 个实例**，以及磁盘上直接枚举出的 **859 条路径**，逐个过新规则：
**拒绝数 0、独立规则命中 0**。

### 5.5 测试

`scripts/lib/release-artifacts.test.mjs` **11 → 18 个用例**（无删除），新增：
载荷根新增文件被拒 / 子目录新增文件被拒 / 超出 10 条时只列 10 条 /
`isSafeRelativePath` 拒绝保留名·尾点·尾空格·ADS·UNC（含 4 个合法反例 `console.txt`、`com10`、
`nulify`、`NOTICE.md`）/ `listPayloadFiles` 拒绝符号链接条目 / launch 声明的 UI 目录不含
`index.html` 被拒 / 清单条目本身是重解析点被拒。

**一处质量改进**：重解析点用例初版在 win32 上因 `EPERM` 而被 `t.skip()`。作者没有接受这个跳过，
而是探测出 **`junction` 类型不需要提权**，加了 `junction → dir → file` 的回退链，
使该用例真正跑起来（`skipped: 1 → 0`）。

### 5.6 H-3 明确**未修**（设计如此）

`--manifest` 依然会把手动加入的**载荷文件**「祝福」成已验证（实测 `BACKDOOR.exe` → `Refreshed 712` →
`Verified 712` exit 0）。这是 `--manifest` 的固有语义——重建清单时就该反映磁盘现状——
防线应放在「谁能跑 `--manifest`」，而不是让 `--manifest` 拒绝磁盘现状。
（FIX-13 之后有一个例外：**`inputs` 段**不再享有这个自由，见 5.7。）

### 5.7 FIX-12/13/14：打包链路的第二轮（许可证按载荷枚举 / 输入新鲜度 / clean 范围）

这三条来自 R2 发布审查，与 FIX-4 同属交付链路：

| 修复 | 内容 | 核心验证 |
| --- | --- | --- |
| **FIX-12** | `collect-python-licenses.py` 重写：改为从**冻结载荷**的 `*.dist-info` 枚举（不再读构建 venv），路径相对载荷根，收录规则改为「文件名前缀 **或** `License-File` 点名」；`build-python-agent.mjs` 在 PyInstaller 前清掉旧 onedir 与旧通知 | 通知 `418306 B / 85 个发行版 → 46086 B / 21 个`，幻影包 `yukinal-agent-node` 残留 **0**，绝对路径烧入 **0 行** |
| **FIX-13** | 产物 mtime 与源码目录**最后一次提交**逐件比较，陈旧默认拒绝（`--allow-stale-inputs` 可豁免并留痕）；manifest 记录 4 条 build input 的 sha256/构建时间/修订/`dirty`；`inputs` 段被 `--verify` 与 `--manifest` 双向校验 | 陈旧产物 → exit 1 且**不创建输出目录**；篡改 provenance → `--verify` exit 1、`--manifest` 拒绝刷新；测试 `18 → 24` |
| **FIX-14** | `clean.mjs` 纳入 `target/`、冻结 Agent onedir 与通知；glob 显式 `cwd`、用 `join()` 而非 `new URL()` | 恰好 7 条命中、0 条被跟踪文件、`.gitkeep` 幸存 |

三条落地后由 `fixer-gate2` 做了一轮**对抗式证伪**（`target/verify-fix5/VERIFY-5-report.md`）。
它先证实了三条修复确实改变了失败模式，又拿出多类「不成立」的实证，其中多数被**当轮修掉**：

| VERIFY-5 的证伪 | 实证 | 本轮处置 |
| --- | --- | --- |
| `inputs` 从不被 `--verify` 回读：改 sha256、伪造 `sourceRevision`、删整段，**全部 exit 0** | 探针脚本 `tamper-round2.mjs` | **已修**：`inputConsistencyProblems()` 把 `inputs[].sha256` 与同路径的 `payloads[].sha256` 逐条对账，重复 role / 指向清单外路径也被拒；`--manifest` 刷新前跑同一检查，伪造段不能再被「刷新」洗白 |
| 删掉载荷内的 `THIRD_PARTY_LICENSES.txt` 后 `--verify` 仍 `Verified 710 file(s)` exit 0 | `final-checks.mjs` H1 | **已修**：`launch.agentEntry` 现在参与校验，并强制其同目录必须存在该通知 |
| `--agent-runtime <另一 onedir>` 时通知与载荷**静默脱钩**（通知字节与仓库内文件相同，只是描述另一棵树） | `alias-runtime-test.mjs` | **已修**：暂存前把通知的 `## 发行版 版本` 列表与将要暂存的 onedir 的 `.dist-info` 逐条对账，不匹配即 fail-closed 且**不创建输出目录** |
| `build-python-agent.mjs` 传**绝对** `--payload`，会把 `D:\yukinal\…` 烧进随包发布的通知（40 行差异） | 相对/绝对复算对比 | **已修**：收集器把路径解析为相对载荷根，两种调用方式输出**逐字节相同**（46086 B） |
| `cryptography 50.0.2` 声明的 `LICENSE.APACHE`/`LICENSE.BSD` 被后缀白名单丢弃，只运出 197 B 指针文件 | `omission-scan.py` | **已修**：收录规则改为「文件名前缀 **或** `License-File` 元数据点名」，后缀白名单删除；Apache-2.0 正文出现次数 `1 → 9` |
| 无 git 时陈旧门禁整体静默失效（4 条 input 全 `"unknown"`，仍 exit 0） | PATH 去掉 git | **已修（降级为显式告警）**：`sourceRevision === "unknown"` 的输入会打印「新鲜度未验证」警告。**未改成硬失败**——导出 tarball 等无 git 场景是合法的，把它变成失败会让正常打包不可用 |
| 对**未提交**的源码改动不可见：产物旧于工作区、却仍判「新鲜」 | `stale-probe3.mjs` edge A | **已修（记录而非拦截）**：每条 input 增加 `dirty` 字段，notes 里标出「source directory has uncommitted changes」。比较对象是最后一次提交，工作区编辑本就不该让打包失败，但必须可见 |
| `new URL(target, root)` 对含 `#`/`?`/`%` 的目录名会解析到**别的路径**（可能误删父目录） | URL 解析探针 | **已修**：`clean.mjs` 改用 `join(root, target)`（不再是 URL 拼接）。当前无此目录，属潜在项 |

**自身的记账**：`inputs` 段这一半最初只写不读——VERIFY-5 的原话是「provenance 是没人验的文档」。
被证伪后它才成为**被校验**的段，这也是本文为什么把它单列：一条修复是否成立，不能只看它是否
「成功执行」，还要看它承诺的不变式是否有**第二个读者**在强制。
`scripts/lib/release-artifacts.test.mjs` **18 → 24 个用例**（新增 provenance 对账、通知-载荷对账、
`readDistributionIdentity` 三例、`dirty` 归一化、通知缺失被拒；无删除）。

**仍未修/未验证**（诚实列出）：

- **无真实 PyInstaller 运行**：清理分支是在副本树里用 `PYTHONPATH` stub 验证的，不能据此声称
  「真实失败构建不留旧通知」。
- **载荷内许可证文本的完整性**没有独立校验：通知现在与**发行版集合**对账，但不与每个 `.dist-info`
  的实际许可证字节对账，超大文件省略与重复文本共享仍是**告知性**输出。
- `--manifest` 与 `--verify` 同时给出时「后写者胜」（HEAD 亦同，属存量语义）未改。
- `newestFileTime()` 对空目录返回 `NaN` → 判为新鲜：今天不可达（`resolveUiDirectory` 要求可读
  `index.html`），属潜在项。

---

## 6. FIX-5 控制平面两条 P0

### 6.1 A：`approvals/expire` 完全没有租户门禁

**修复前**：该路由的参数只有 `tenant_id` / `session` / `_actor`，**没有调用 `require_tenant`**；
服务层把 `tenant_id` 只当作可选 `WHERE` 过滤。实测后果：

- 对**不存在**的租户返回 `200 {"expired":0}`（同租户名下其它 6 条路由全是 404）；
- 对**已禁用**的租户会真的把 `approved` 改成 `expired` 并写入审计（`{"expired":1}`）——
  即 `disabled` 开关**挡不住这条写入路径**。

**修复**：在 sweep 之前加 `await service.require_tenant(session, tenant_id)`。

**实测**：两类租户都变成 `404 not_found`，且与 `GET /v1/tenants/X` 的响应体**逐字节相同**；
禁用租户那次的审批仍为 `approved`、审计行数不变（`4 → 4`）。没有改成「迷惑性一致响应」——
修的是门禁。

### 6.2 B：失败的请求不烧 nonce

**修复前**：`claim_nonce` 只在验签**通过之后**调用，且 nonce 插入发生在请求事务内，
路由抛错会让整个事务回滚。于是同一份签名字节在 300 s 窗口内可反复重放。

**选定的修法（含被实测掉的方案）**：先试「claim 后 `session.commit()`」，结果 nonce 行确实活下来了，
但请求自身的 `session.begin()` 上下文管理器随即失效
（`InvalidRequestError: Can't operate on closed transaction inside context manager`），
请求内后续任何写入都失败 —— **不可用**。最终采用**独立提交的短事务**：

```python
async with nonce_session_factory() as nonce_session:
    async with nonce_session.begin():
        nonce_session.add(models.NodeNonce(node_id=node_id, nonce=nonce, at=stamp))
        await nonce_session.flush()      # IntegrityError → 原来的 Unauthorized
```

并把 `claim_nonce` **前移到验签之前**，两处调用点（HTTP 与 WebSocket 握手）显式传入 session 工厂。
因此**两类重放都被覆盖**（不是备选方案只覆盖一类）：

```
P82  409 invalid_transition → 操作员把任务推进到 running → 同一字节同一 nonce 重放
     修复前: 200 succeeded        修复后: 401 unauthorized（任务停在 running）
P83  签名错误 401 → 同一 nonce 换正确签名
     修复前: 200                  修复后: 401
成功路径 200 / 重放 401 / nonce 行恰好 1     ← 未变（防止把重放防护改坏）
```

**刻意保留两个「不烧」**：缺请求头、未知或已禁用节点。此时**没有 `node_id` 可归属**，
硬写需要哨兵值，且会给未认证请求填表能力。

### 6.3 测试与差异回归

测试 **129 → 139**（+10，AST 独立重算 `23+20+38+21+10+27 = 139` ✓）。要求的 5 条覆盖：
不存在的租户 → 404；禁用租户 → 404 且**零副作用**（断言审批状态与审计行数不变）；
坏签名后同 nonce 重放被拒；成功后同 nonce 重放仍被拒（加强既有用例，补「恰好 1 行 nonce」）；
启用租户 `expire` 仍能扫过期审批。另 5 条钉边界：其余 9 条路由全 404 的 surface 测试、
验签前已占用、无头/未知节点零写入、WS 握手坏签名后同 nonce 仍 `4401`。

**差异回归（最关键的证据）**：把 `git show HEAD:` 的修复前 `app.py` / `service.py` / `channel.py`
还原到临时目录，跑**同一份套件** → `Ran 139, FAILED (failures=7)`，7 条**恰好**是本轮新增/加强的
用例；两条纯护栏（成功后重放、启用租户仍扫）前后都绿，正是它们的作用。

### 6.4 本次修复引入的新代价（单列，未修）

占用前移到验签之前之后，**任何知道 node id 的人**（node id 不是秘密，WS `ready` 会直接回显）
都可以用垃圾签名写 nonce 行：实测 200 次 → 200 行、7.0 ms/次；单个 100 000 字符的 nonce
**原样落库**（SQLite 不强制 `String(128)`，PostgreSQL 下会变成驱动层 500）。
因此「nonce 长度上限 + 频率限制」应从 P2 加固**升级为下一项**：修法是在 `service.py` 的
`claim_nonce` 前加显式长度检查，超长返回与验签失败**相同**的 `Unauthorized`。

### 6.5 R2 阶段的两处自我更正（记录在案）

审查者在定稿前主动更正了自己的结论，这类记录比结论本身更有价值：

1. **矩阵「禁用租户仍可用」列的计数对不上**：原因是矩阵把 3 条**全局路由**
   （`GET /v1/metrics` 等，路径不含 `{tenant_id}`）也写成了「是」。重跑穷举探针后发现
   **第一版探针本身有 5 个测量假象**（用占位 body 导致的 422 被误读为租户门禁生效），
   于是重写探针并加「启用租户 27/27 全 2xx」的对照组。最终数字仍是 **10 条**，但依据从静态推断
   变成了实测。
2. **「不存在的租户被确认可操作」的措辞站不住**：实测启用 / 已禁用 / 从未存在三种租户的
   `expire` 响应**逐字节相同**，所以它**不是存在性预言机**。准确且更严重的表述是
   「#26 是一个无租户门禁的状态变更操作」——对已禁用租户它会真的改数据，
   对不存在的租户只是空转 200。

---

## 7. 第二批修复（FIX-6 到 FIX-11）

第一批修的是「文档与门禁」和「交付链路」；第二批修的是**运行时缺陷**，每一条都曾经让某个
安全或可用性保证失效。第二批的 13 项（FIX-6 – FIX-11、FIX-15 – FIX-18）已全部完成，
下面按落地顺序记录；其中（FIX-9 / FIX-11）
的验收证据在各任务完成后补入本节。

### 7.1 FIX-6 控制平面 P0（P0-1 / P0-2 / P0-4 剩余 / nonce 上限）

**修复前**（`docs/review/r2-control-plane.md` 的实测）：

- **P0-1**：`canonical_request(timestamp, nonce, method, path, body)` **不含 node id**，而验签用的是
  `X-Yukinal-Node` 指向的那一行；`nodes.public_key` 也没有唯一约束。两者相加 ⇒ 同一把公钥登记进
  两个租户后，**只改一个未签名的请求头**就能跨租户读写（两次 201、`same_signature_reached_the_other_tenant: true`）。
- **P0-2**：派发门禁只在 `previous == "queued"` 时调用，`unknown → running`（恢复）绕过它。
- **P0-4 剩余**：FIX-5 只修了 `approvals/expire`；实测还有 6 条（HEAD 上 9 条）路由对禁用租户有效。
- **nonce 代价**（FIX-5 引入）：100 000 字符的 nonce 原样落库，200 次垃圾签名写 200 行。

**修复**：

| 项 | 修法 | 位置 |
| --- | --- | --- |
| P0-1a | `canonical_request` 把 **node id 作为第一行**；`sign_headers` 必须传 `node_id`；`authenticate_node` 用 `node.id` 重建规范串，WS 握手同样传入 | `nodeauth.py:127-140,165-198`；`service.py:277-279`；`channel.py:93-104` |
| P0-1b | `UniqueConstraint("public_key", name="uq_nodes_public_key")` + 新迁移；迁移**先查重复**，命中则列出 `SHA256:<16>` 指纹并 `RuntimeError`，**版本号留在原处**（不做自动挑选） | `models.py:67`；`migrations/versions/b7c41d0e9a2f_one_public_key_one_node.py:58-73` |
| P0-1c | `IntegrityError` → 409，且**不点名**持有该公钥的租户或节点 | `service.py:164-172` |
| P0-2 | 守卫由 `status == "running" and previous == "queued"` 收窄为 `status == "running"`；`attempts += 1` 语义不变 | `service.py:1140-1151` |
| P0-4 | 在共享解析器（`require_node` / `require_task`）里加 `require_tenant`，另对 `decide` / `revoke` 显式加门禁 | `service.py:194,1099,1117,1420` |
| nonce | `MAX_NONCE_CHARS = 128` 显式长度检查 + 每节点 `MAX_OUTSTANDING_NONCES_PER_NODE = 128` 未过期上限，**两种拒绝与验签失败逐字节相同** | `service.py:52,58-75,263,312-325` |

**验收（Lead 独立复跑）**：

```
$ services/control-plane/.venv/Scripts/python.exe -m unittest discover -s tests -t tests
Ran 148 tests in 21.288s
OK                                            ← 139 → 148，只增不减
$ git status --porcelain services/control-plane
（12 个 M + 1 个新增迁移，全部在写作用域内）
```

**差异回归**（审查者提交，含逐项还原）：整套还原 FIX-6 四项（保留 FIX-5）→ `FAILED (failures=11, errors=32)`、
**43 红**；**逐项**还原 → 项1 5 红 / 项2 恰好 2 红 / 项3 7 红 / 项4 2 红。
对照组的意义：`git show HEAD:` 全撤会得到 **48 红**（多出的 5 条是 FIX-5 的用例），
说明 **HEAD 不是本轮合格的对照树**——这本身就是一条记录，避免把「和 HEAD 比」当成万能差异证据。

**实测数字（前 → 后）**：跨租户同签名 `true` → `401`；100 000 字符 nonce `200` 且
`MAX(LENGTH(nonce)) = 100000` → `401` 且 **0 行落库**；200 次垃圾签名 **200 行 → 恰好 128 行**、
200 次全 401；200 次**合法**签名 200 接受 → 128 接受 / 72 拒绝（首次拒绝在索引 128，与上限吻合）；
禁用租户泄漏 **6（HEAD 9）→ 0**，且 24 条 `{tenant_id}` 路由与 app 自身路由表做**集合相等**后逐一驱动。

**这条是破坏性客户端契约变更**，需要单独说明：节点驱动必须把自己的 node id 签进规范串，否则升级后
一律 401。已确认仓库内没有受影响的消费方（`git grep` 在 `agent/ native/ packages/ apps/ scripts/`
下对 `X-Yukinal-*` / `sign_headers` / `canonical_request` **命中 0**），所以改动被限制在
`services/control-plane/` 内；版本号与外部驱动的升级说明由维护者决定，已写进控制平面 README 的
“Upgrading an existing deployment”。**未验证**：SQLite 通过不等于 PostgreSQL 通过（唯一约束的
并发插入路径未被本机单进程测试覆盖）。

### 7.2 FIX-8 native `exit_code` 语义与 SIGPIPE

**修复前**：exec 与终端两条路径都用 `libssh2_channel_get_exit_status(...) < 0` 判定「结果未知」。
但 libssh2 对接收到的是**32 位状态做无条件强转**，所以 `0x80000000` 会变成负数——守卫可达但语义错：

- 服务器**一直没发** exit-status 时，`get_exit_status` 返回 0，旧的 `< 0` 判定**不成立** ⇒
  报告**成功、退出码 0**（假成功）；
- 服务器发了 `0x80000000` 时，得到负数 ⇒ 报告 `result_unknown`（**假失败**）；
- 终端路径还有第三重：`closed` 帧原本用 `exit_code < 0 ? null : value`，把**已收到**的状态抹成 `null`。

**修复**：新增 header-only seam `native/src/exit_status.hpp`，用 pinned libssh2 的
`libssh2_channel_has_exit_status()`（唯一能区分「收到 0」与「没收到」的信号）判「是否收到」，
收到则**原样**取值（包括 0、包括高位强转后的负值，取舍写进注释与头文件契约）。
终端与 exec **两个调用点**都改用该 seam，并把终端的 `closed` 帧改为由「是否收到」驱动。
另在 `main.cpp` 加 `#ifndef _WIN32` 的 `std::signal(SIGPIPE, SIG_IGN)`（H-2：Linux 上 RST 会按默认
动作直接终止长生命周期的服务进程）。

**验收**（`native/build-nmake`，只构建 `yukinal_ssh_tests` 与 `yukinal-ssh-service`）：

```
1/5 yukinal_ssh_tests ............ Passed 0.64s   (63 test(s), 0 failure(s))
4/5 ssh_mock_integration ......... Passed 9.58s
5/5 agent_ssh_mock_integration ... Passed 12.48s
100% tests passed out of 5
```

**差异回归**（用 `git show HEAD` 的源码单独编译出的修复前二进制）：
未发 exit-status 的前缀版返回**成功 exit 0**（假成功）；`0x80000000` 的前缀版抛
`The SSH connection closed before an exit status was received`（假失败）、终端为
`{'exit_code': None, 'type': 'closed'}`（把已收到的状态抹成 `null`）；修复版分别得到
`result_unknown` 与 `-2147483648`。源码级对照也记录在案：HEAD 上 `has_exit_status(` 命中 0、
`interpret_exit_status(` 命中 0、`SIGPIPE` 命中 0。

**未验证（必须说清）**：SIGPIPE 交付的是**源码级护栏**（断言 `#include <csignal>`、唯一一处
`std::signal(SIGPIPE, SIG_IGN)`、位于 `#ifndef _WIN32` 内且晚于 `int main(`、早于 worker/HTTP 派发），
**不是行为测试**——本机是 Windows（无 SIGPIPE）且 WSL 未安装，无法在 Linux 上触发 RST。
另：`agent/src/yukinal_ssh/client.py` 的通用 `_execute_operation` 不限制 `exit_code` 范围，
所以高位强转后的负值会一路传到 Agent（`server_exec` 那条路径有 0..255 校验，属既有行为，未改动）。

### 7.3 FIX-10 契约诚实化（verbatim 响应闸门 / 零生产者通道 / Rust-Tauri 因果依据）

**修复前**：`packages/shared` 里 13 条**原样转发** sidecar 响应的命令，其 `response` 是
`z.strictObject`；sidecar 只要多一个字段，UI 就会抛错——契约比实现更严格，且这个风险是**静默**的
（实测 0 条会立刻抛，12 条「未来加字段即抛」）。同时 6 个可订阅通道加 3 个声明名在 C++/Python 里
**没有任何生产者**，而测试却断言「生产者已核对」；大量注释把**已删除的** `crates/` / `src-tauri`
当作当前的因果依据。

**修复**：三条路线里选了**「容错 + 漂移可观测」**，而不是「宿主按 schema 裁剪」：

1. 裁剪是错的层——`ipc_router.cpp` 把 sidecar 响应原样当帧转发，在那里按 TS 字段清单裁键，
   等于让 C++ 宿主维护一份它本不需要知道的清单，每加一个 sidecar 字段要改两个语言；
2. 新增 `SIDECAR_FORWARDED_COMMANDS`（13 条）**显式划定**容错范围，**其余响应仍是 `z.strictObject`**
   （`agent_status` / `server_connect` / host-key 这些宿主自组装的响应多一个字段仍然抛错）——
   不是全局放宽，而是「按谁写的 payload 分读法」；
3. 新增 `unknownResponseKeys()` 递归找出契约未声明的键，桌面侧每命令 `console.warn` 一次报出名字，
   并导出 `strictResponseSchema` 保留「修复前会怎样」这个可回答的问题。**没有用 `.passthrough()`**：
   `catchall` 更精确，且能同时表达 tolerate / strict 两种读法。

其余三项：**9 个零生产者事件名全部「保留 + 显式标注」**（补发是增量改动，删名会把将来的接线
变成契约破坏），测试从「生产者已核对」改成**三态分区 + 9 这个数字本身被断言**；
`tauriEventName` **全仓零残留**（改名 `eventChannelName`），把 Rust/Tauri 当**当前因果依据**的注释
全部改写为「两侧由 fixture 与实测生产者对齐」。用户可见文案（A7）一行未动。

**验收**：

| 范围 | 前 | 后 |
| --- | --- | --- |
| `packages/shared` | 135 | **142**（142 pass / 0 fail；Lead 独立复跑确认） |
| `apps/desktop` | 323 | **325**（324 pass + 1 skip） |

**差异回归**：把容错构造临时改成恒等 → shared **4 红**、desktop **1 红**，恢复全绿；
另用 zod 原语独立旁证 `strictObject` rejects/`unrecognized_keys` 而容错 accepts；
13 条转发行 **pre-fix rejected 13/13 → post-fix accepted 13/13**；
并有反证用例钉住「宿主自组装响应仍然严格」。

**顺带发现的两处**（不只是措辞）：`apps/desktop/src/lib/mcp.ts` 原文称宿主对 MCP stderr
「截断**并脱敏**」——实测宿主**只截断、不脱敏**（`desktop_mcp.cpp:455-457`），已改正；
`types/provider.ts` 称某配置由宿主「随 `agent.run.start` 下发」——实测它在 sidecar 进程内构造，已改正。

**未修（本轮最大的一条）**：`server.auth_challenge` 的**生产者**本身（C-1/C-2）没有补——
弹窗会一直等到超时也收不到挑战。补它需要改 `native/**`（不在写作用域），已单列为 **FIX-11**。
`terminal.output_lost` 的 `Lagged` 处理同样只做了标注。

### 7.4 FIX-7 Agent 四条「永久停在运行中」

**修复前**：UI 只有两条依赖**进程死亡**的兜底信号，所以「进程存活 + run 不再推进」= 永久挂起，
唯一恢复手段是重启 sidecar。四个机制共享一个特征：**写终态的代码不在任何一定会执行的路径上**。

| 机制 | 根因 | 修复 |
| --- | --- | --- |
| H1 取消落在协程首步之前 | `task.cancel()` 把一个尚未开始的协程标记为 cancelled，**协程体从不运行**，`try/finally` 全部不执行，`_completed_runs` / `admission.completed` 永不写入 | `create_task` 之后、注册 `_active_runs` 之前**同步**挂 `add_done_callback`（中间无 await，因此永不漏）。done-callback 是唯一对每个 run 都执行的钩子，负责兜底记账 |
| H2 前置区抛异常 | `_execute_session_run` 的前置区（读 context / memory / `record_event`）在 `try:` **之外**，唯一写终态账目的位置不执行 | 整个前缀纳入单一 `try`，`finally` 无条件 `_terminal_accounting`；`context_usage` 提升为安全默认值，失败分支不会再因读它二次抛错 |
| H2b 前置区的 `_emit` 抛错 | 同 H2（审计写入失败，如磁盘满 / sqlite 锁） | 同上；另外 `context.loaded` 原先跳过 `_emit` 直连 `record_event`，是本轮唯一让测试从 `completed` 变 `failed` 的真实吞事件点，一并修掉 |
| H3 终态 `_emit` 抛错 | `_emit` 先 `record_event` 再发通知，审计抛错则通知根本不发，外层 `except` 再 `_emit` 又抛一次 | `_emit` 拆成「审计失败只写 stderr」+「通知**无条件**投递」；`_emit_terminal` 保证终态**至多一次**，投递失败则留存，由 done-callback 重投 |

**顺带修的 H4 后果**（原判为「放大器」）：`delivery:"sync"` 的 `await task` 会把取消抛出
`handle_async`，而 `CancelledError` 是 `BaseException`，`except Exception` 接不住 —— 主机于是
**等满 15 分钟** `agent_run_start` 超时。现在 `_await_run_response` 区分「本 RPC 被取消」
（照常向上抛）与「run 被下层取消」（写出 response frame）。

**验收**（Lead 独立复跑）：

```
$ agent\.venv\Scripts\python.exe -m unittest discover -s agent/tests -p "test_*.py"
Ran 164 tests in 27.989s
OK (skipped=4)          ← 153 → 164（+11），skip 数不增
```

**差异回归**（Lead 独立复现，不采信写者自报）：把 `git show HEAD:agent/src/yukinal_agent/sidecar.py`
导出到 `target/lead-verify/src-old/`，以 `PYTHONPATH` 指过去、仅替换这一个模块后跑新增的 11 条：

```
Ran 11 tests in 0.398s
FAILED (failures=8, errors=1)   ← 11 条里 9 条在修复前变红
```

两条刻意在修复前即绿：`test_policy_id_is_rejected_before_any_run_state_exists`（钉住**本就正确**的
边界，不许回归）与 `test_stop_while_the_model_is_thinking_still_ends_cancelled`（钉住**取消仍然生效**，
防止用「吞掉取消」换 `finally` 执行）。每条新用例同时断言三件事：终态事件到达 notification sink、
磁盘/内存终态已写（`conversation_turns.status`、`_completed_runs`、`_active_runs`/`_run_sessions` 已清、
`admissions[..].completed`）、**恰好一条**终态。

**`policyId` 选 (b)：保持边界拒绝，由契约/UI 停止发送。** `PolicyEngine` / `PrincipalScope`
（`agent/src/yukinal_agent/policy.py`）是**死代码**，从未接入 `PythonSidecar`
（`ToolRegistry(authorization=)` 无调用者），`system.describe` 如实上报 `permissionPolicyIds: []`
（`sidecar.py:220`）。选「在 agent 侧造一个注册表」等于新造一套无法被独立证伪的策略语义，
所以维持 `-32602`，并把它变成**双端一致**的契约：见 **FIX-15**。

**未修（明确保留）**：

- **H4 的根因**（`delivery:"sync"` 这个放大器本身）没改投递模型；建议桌面一律用 `async`
  （`useAgentRun.ts:405,411` 已如此），sync 只作为兼容路径。
- **B7 死通道 `agent.thinking` / `agent.usage`**：**不是未修项，而是「已如实记录的设计缺口」**。
  实测：全仓库 `*.py` 搜 `agent\.thinking|agent\.usage` → **0 命中**（连测试里都没有），
  即 Python 侧确实从未发过这两个事件；`model_agent.py` 不抽取任何 token usage，
  所以补 producer **只能靠伪造数据**——那比通道为空更坏。删名同样不可取：补发事件是增量改动，
  删名会把将来的接线变成契约破坏。FIX-10 因此把它变成**诚实且可回归**的状态：
  `event-vocabulary.test.ts:145` 钉住 `NO_PRODUCER.length === 9`，`:146-158` 逐名校验这 9 个名字，
  改名或删名都会红、**补生产者必须同步减一**——「名字不许无声消失」这条有读者。
  `agent.ts:173-176` 也写明这是**形状词汇表，不是生产者普查**。
  **将来的接线点在模型适配层**（`model_agent.py` 需先抽取 token usage），
  不是「补一个 emitter 就行」。
- 6 项非挂起的 Medium（`_run_stop` 与 UI `isActive` 竞态、`_cancel_host_request` 游离任务、
  `asyncio.to_thread` 不可取消、`_active_runs` 无并发上限、同 session 第二个 run 阻塞、
  无 `conversation_turns` 行时 `record_event`/`finish` 静默 no-op）。

### 7.5 FIX-15 桌面策略选择器（FIX-7 的 UI 侧收尾）

FIX-7 确认 Agent 侧拒绝 `policyId` 是**正确的**，但桌面侧的「目标策略」选择器仍把它放进
`agent_run_start`，后果是**用户一选策略 run 就启动失败**（`-32602`）。默认值「按环境自动」
不带该字段，所以现有测试从未覆盖这条路径。修法与验收见
[`review-report.md`](./review-report.md) 的 FIX-15 条目与本节更新。

### 7.6 对第二批修复的对抗式证伪（VERIFY-FINAL）

第一批修复曾被 `VERIFY-5` 改写过（见 5.7 节）。第二批同样由同一位置做了独立证伪——
结论里**至少有一条推翻了我们自己的说法**，因此单列。

**被证伪的第一条：FIX-6 的「nonce 上限」不是它声称的界。**
`claim_nonce` 是 `COUNT(*) → 判断 → await flush`，读与写之间有 `await`，凡是 commit 前通过
COUNT 的请求都读到同一个陈旧值。真 app、真签名、`threading.Barrier` 对齐的实测：
`overshoot = concurrency − headroom`，6 组中 5 组逐位吻合；**余量 4 / 并发 64 → 表里 188 行**
（声称上限 128）；最小反例 `before=124, concurrency=16 → after=140`。因为 claim 发生在**验签之前**，
**未认证**调用方开 N 条连接就能塞 `128+N` 行——正是注释想防的那件事。`README.md:104` 与
`service.py:303` 把它写成 security bound，属于**过度声称**。→ 已开 **FIX-17** 重做。

**同一实验还发现它对合法节点是错的**：200 次**合法**唯一 nonce → **128 接受 / 72 拒绝**，
首次拒绝在 #129，**响应体与真坏签名逐字节相同**；稳态上限 `0.43 req/s`。一个每秒心跳的节点
128 秒后被限流，而它收到的是「你的签名对这个节点无效」——**把可恢复的运维事件伪装成密钥故障**。
这才是真正的缺陷：上限的目的是限制**垃圾**写入，而验签成功的请求不是垃圾。

**被证伪的第二条：FIX-7 的两个安全网没有读者。**
删掉 `_terminal_emitted`（终态至多一次）= **0 条测试变红**；删掉「投递成功才记账」那一半
= **0 条变红**（两次的红集与基线完全相同）。`test_...exactly_one_terminal_event` 之所以绿，
是因为**调用图本来就只到一次**，不是守卫拦住了第二次；手工双调 `_emit_terminal` 则能证明守卫
确实承重。对照之下 AG-2（不注册 done-callback）= **4 条红**，即主梁里只有一根有人看。
两条用例把「恰好一次」写进名字，验的却是「代码路径只会走一次」。→ 由 **FIX-18** 修补。

**FIX-18 的修补方式与结果**（**只动测试，`sidecar.py` 一个字符未改**，哈希
`3BE49E3A…12A13F` 与 FIX-7 交付时一致）：先让测试观测「**投递要约次数**」而不是「事件是否存在」
——`LocalSidecar._notify` 每次要约都记录「要约那一刻两个账本的快照」，只有真正通过传输才计入
`events`；再新增 3 条用例分别钉住三根梁：

| 新用例 | 钉住的守卫 | 变异后 |
| --- | --- | --- |
| 同一 runId 的第二次终态永不达 sink | `_terminal_emitted` 去重（`sidecar.py:1126`） | AG-1（`if run_id in …` → `if False`）：**1 条红**，`AssertionError: 3 != 1`（此前 0 条） |
| 投递失败后重试、且**成功才记账** | `_terminal_emitted.add` 的位置（`sidecar.py:1135-1147`） | AG-3（记账挪到投递前）：**3 条红**，含新用例 `['agent.completed'] != ['agent.completed','agent.completed']` |
| done-callback 重投自己的终态 | `sidecar.py:964`（与 `:947` 是**两行不同代码**） | AG-4（删这一处重投）：**只有这 1 条红**——证明它钉的是另一根梁而非重复 |

Agent 测试 **164 → 167**（skip 数不变），Lead 独立复跑 `Ran 167 tests … OK (skipped=4)`。

**诚实边界**（「有读者」不等于「全覆盖」，写清楚以免被读成后者）：

- AG-1 守卫**仍然无法从公共 RPC 自然触发**：真实调用图只走到 `_emit_terminal` 一次，新用例是
  **直接双调**施压。回归保护有了，但「线上真会出现第二次终态」这一前提**未被证明**。
- `sidecar.py:941` 的「无终态记账」分支 AG-5 实测 **0 条红**，其 docstring 自称公共 RPC 不可达 ——
  属**已知无读者但据称不可达**，未为它造打桩测试（那等于测 mock）。
- `_completed_runs` 去重、`_terminal_emitted` 的 512→256 修剪、`_undelivered_terminals` 的 256 裁剪
  仍无读者（需 ~257/513 个 run 才触发，成本高于收益）。
- 终态的**跨进程语义**未覆盖：`_terminal_emitted` 是内存账本，进程重启后同一 runId 的终态可再投一次
  （对桌面是重放，不是死锁）。这是设计选择，不在本轮回归范围。

**FIX-8 的负面结果带来一次自我修正**：`M1`（`interpret_exit_status` 改回 `< 0` 符号判据）=
**2 条红**，且同时造出两个方向的错（未收到 exit-status 报成成功 = 假成功；`0x80000000` 报成
未收到 = 假失败）。但 `M4`（**只**把 terminal `closed` 帧改回 `< 0` 判据）= **0 条红**。
初判「terminal 帧没有行为读者」随后被自己修正：**读者本来就在**
`scripts/test_ssh_mock.py:926-932`（断言 `0x80000000 → -2147483648`、无 exit-status → `None`），
只是不在 C++ 单测里；FIX-16 又在便宜目标上补了帧形状断言，并用同一份 `M4` 变异体复算确认
**它会变红**（缺口闭合）。**这段「先宣布漏洞、再自己推翻」的过程保留下来**，因为它说明了
变异测试的结论同样需要第二个人复核——包括做变异的那个人自己。

**FIX-10 是四项里证据最完整的**，两条反证都成立：用**真 fixture** 逐命令注入 +1 个未知键，
13 条转发命令全部 accepted，**73 条 host 自造响应全部 refused（unrecognized_keys），泄漏 0**
——「没有全局放松」成立；而那「9」**不是自证的**（改名 `server.updated` → 集合断言变红；
从 `EVENT_NAMES` 删名 → 也红），`9` 本身是写死的字面量，但同一测试块另有 `deepEqual` 把九个名字
钉住，**成对存在则不自证**。

**12 条子不变式里，4 条只有一个读者**：nonce 的未过期上限、nonce 长度上限各 1 条；
禁用租户三条（CP-6a/6b/6c）**共用同一个测试**——「24/24 路由逐一驱动」在测试里被压缩成一次遍历。
最扎实的是「nonce 独立事务」（**27 条红**）与「迁移与模型一致」（3 条、跨文件）。

**迁移②通过**：造出重复 `public_key` 后 `alembic upgrade head` → `exit=1`、
`RuntimeError … (SHA256:BuifJbNvcJdhTf/2 x2)`、`alembic current` **与** DB 版本都停在
`fc2e168fa85c`、约束不存在、`indexes=[]`、**2 行数据保留**、无残留表；修数后重跑 `exit=0` 并出现约束。

### 7.7 FIX-9 桌面宿主审批回执 / `decision` 白名单 / `mcp_server_list` 真状态 / `consume` 有界

四项都在 `native/desktop_host/**`（8 个文件），决策与证据：

| 项 | 决策 | 为什么 |
| --- | --- | --- |
| 未知 `approvalId` | **不转发 + 显式错误** `AGENT_APPROVAL_UNKNOWN` | 宿主对它一无所知却回 `{"accepted":true}`，UI 会显示「已批准一次」而实际零授权；转发则等于把伪造 id 当合法响应喂给 sidecar。为不误伤真实的 sidecar 自有审批，新增**有界外来观察表**（`kMaxObservedApprovals=256`，只记非 `HOST_APPROVAL_TOOLS` 的通知），这类审批照旧转发并回报 sidecar 的 `accepted` |
| `decision` 白名单 | 在**任何账本/sidecar 工作之前**拒绝 `approve_once`/`reject` 之外的一切 | `approve_session` 在宿主与 sidecar **双方**都不可授予，放过去只可能产出一张误导性回执；前置拒绝同时保证不烧票、不过管道 |
| `mcp_server_list` | `McpSupervisor::status(id)` 只读 API + 真实 `pid`/`program`/`startedAt`/`toolCount` | 原实现硬编码 `running:false / toolCount:0`，运行中的服务器被报成未运行 |
| `consume` | 上界 **180 s = `kMaxApprovalTtlSeconds`**，超时销毁票并返回既有 `TicketError::Expired` | 任何票都不可能活过一次有界等待，所以这个界**不会误拒**有效期内的票；4 个卡死的 `Responding` 请求最多占用 worker 池 180 s 而不是永久。复用既有错误码，`denied_by_policy` + `detail.code=approval_ticket` 的 fail-closed 映射现成 |

**差异回归（四轮「修复前失败」对照，逐项还原并用 SHA256 核对）**：① 未知 id 改回 `nullopt` → DB 测试红；
② 删白名单 → DB 与 sidecar 两个 target 同时红；③ `view_for` 改回硬编码 → DB 红；
④ deadline 改成 `now + 24h` → **不是失败而是挂死**：同一二进制 600 s 仍未返回、被手动 kill。
**第④轮比任何断言都更有说服力**：它复现了 R2-H-2 的本来面目。
`ctest -R "desktop_host_tests|transfer_tests|transfer_manager_tests|desktop_host_database_tests|desktop_host_self_check|desktop_host_sidecar_tests"` → **6/6 Passed**（16.32 s）。

**反面护栏（修复前后都应绿，约束「别过度收紧」）**：假 relay sidecar 真实发出 `filesystem.write`
（非 host 动作）审批 → 响应仍被转发且 `accepted=true`；relay 侧日志证明**真正过管道**的决策恰为
`["approve_once"]`，`approve_session`/`maybe` 从未过管道。**这类反证比正向断言更有价值**。

**未修边界**：`view_for` 的 `tools` 仍是 `[]`（本次只让 `status` 变真）；`McpSupervisor::stop()`
仍返回它自己那份窄形状（三个键都是 optional，`mcp_server_stop` 的回执形状与 `mcp_server_list`
不一致，权威读法以 `mcp_server_list` 为准）；sidecar 对不认识的 id 仍回 `{"accepted": false}`（宿主现在
先决策，用户可见结果不再依赖它）；UI 里 `approve_session` 的死分支仍在（`apps/` 另一个作用域）。

### 7.8 FIX-16 给 SIGPIPE 屏蔽补「第二个读者」

FIX-8 在 `native/src/main.cpp:57` 加了 `#ifndef _WIN32` 的 `std::signal(SIGPIPE, SIG_IGN)`，
但本机 Windows 无 SIGPIPE、WSL 未装、无 Docker，**没有行为测试**。FIX-16 的处置：

- **独立复核「该不该有」**（不引用他人结论）：内置 OpenSSL 3.5.9 的
  `crypto/bio/bss_sock.c:156` → `writesocket` → `include/internal/sockets.h:202` 的
  `#define writesocket(s,b,n) write((s),(b),(n))`（Linux 走这一支），全树 `MSG_NOSIGNAL` **0 命中**；
  `native/` 全树 `signal`/`sigaction` 真实命中**仅 1 处**；且**无** `exec*`/`fork`/`popen`/`system`/
  `posix_spawn`（0 命中），所以「后面会被 exec 重置」的反证不成立。
  cpp-httplib 的同类调用在 `Server::Server()` **构造函数体内**，而 `--terminal`/`--sftp-*`
  worker 模式**永远不构造 `Server`** —— 这正面确认了改动必要。
- **补上护栏**（`native/tests/test_network.cpp`，+312 行，未动 CMake）：断言唯一性、
  `#ifndef _WIN32` 包裹、晚于 `int main(` 且早于 worker 派发，并**断言非空匹配**
  （`size() > 1000` 字节 + landmark 必须命中），路径解析**断言为绝对路径**（cwd 无关性变成被断言的性质）。
  `yukinal_ssh_tests` 63 → **64 用例**。
- **五个变异全部变红**：删守卫行（2 红）、把守卫挪到 `run_terminal_session` 之后（2 红）、
  重新引入 `SIG_DFL`（1 红）、把扫描指向错的小文件（1 红，非空匹配护栏生效）、
  把 resolver 改成相对路径（4 红）。每次变异后立刻还原并核对 SHA256。
- **仍未闭环**：Linux 上真实 RST → `EPIPE` 的现场验证（无环境，**不伪造**）。护栏是源码级弱证据，
  测试注释里写明它**不是**行为测试、也**不**证明内核真的把 RST 变 EPIPE。

**一处仍然存在的脆弱点（写入文档，未修）**：`native/desktop_host` 的 SIGPIPE 保护是**传递依赖**
（`httplib::Server` 构造函数体的副作用，`native/desktop_host/**` 对 `SIGPIPE` 命中 **0** 行）。
若有人换掉 HTTP 实现、或把 `UiServer ui_server;` 惰性化挪到 sidecar 生成之后，SIGPIPE 会**静默**
回到默认终止行为，而**没有任何测试会红**。彻底了结的最小成本是在桌主侧也加一条非行为断言
（钉住 `:374` 早于 `:435` 这个顺序），本轮未做。

### 7.9 FIX-17 nonce 上限：把「声称的界」变成真的界

**证伪结论见 7.6。** 修复只有一处但很关键：`claim_nonce` 由
「`COUNT(*)` → 判断 → `await flush`」改为**单条** `INSERT … SELECT … WHERE count < cap`。
计数与写入由**同一条语句**完成，数据库在同一个写锁下求值，因此并发请求不再能同时读到同一个陈旧值。

**Lead 独立复跑 6 组矩阵**（真 app、真签名、`threading.Barrier` 对齐；`cap=128`）：

```
headroom= 4 concurrency=16 before=124 after=128 overshoot=  0 codes={200: 4, 401: 12}
headroom= 4 concurrency=32 before=124 after=128 overshoot=  0 codes={200: 4, 401: 28}
headroom= 4 concurrency=64 before=124 after=128 overshoot=  0 codes={200: 4, 401: 60}
headroom= 8 concurrency=16 before=120 after=128 overshoot=  0 codes={200: 8, 401: 8}
headroom= 8 concurrency=32 before=120 after=128 overshoot=  0 codes={200: 8, 401: 24}
headroom= 8 concurrency=64 before=120 after=128 overshoot=  0 codes={200: 8, 401: 56}
```

**overshoot 从 188−128=60 变成 0**，且每组恰好接受 `headroom` 个请求（不多不少）。
这条已写成回归测试 `test_the_cap_holds_when_claims_arrive_at_the_same_instant`
（32 线程 barrier、断言表内行数恰为 cap、断言 200/401 计数），控制平面 **148 → 149**。

**一并修正措辞**（`README.md` 与 `claim_nonce` docstring）：把这个上限说清是
**未认证垃圾写入的界**，**不是**每节点速率限制 —— 但它确实会像一条粗粒度限流：
节点在 300 s 窗口内持续超过 `128/300 ≈ 0.43 req/s` 就会打满，而由于拒绝必须与坏签名
**逐字节相同**，节点被告知的是「你的签名无效」。需要更快轮询的部署应调大
`MAX_OUTSTANDING_NONCES_PER_NODE`，**不要把默认值当成容量指标**。

**未修（诚实列出）**：合法的慢速节点仍可能碰到这条界（本轮选择了「说清楚 + 给出旋钮」而不是
「让已验证的行不计数」——后者要加列、加迁移，并且会让「垃圾上限」与「重放账本」耦合）。
「合法节点不被误伤」这一半**没有修**，只在文档里说清了它的表现与调法。

---

### 7.10 FIX-11 桌面宿主接通 keyboard-interactive（`server.auth_challenge` 的生产者）

**修复前**：这台桌面宿主上 TOTP / 二次验证**不可能完成**。服务端在握手中间发
`{"type":"auth_prompt",…}`（`native/src/ssh.cpp:1494-1499`），而宿主的 `handle_frame`
只处理 `ready`/`output`/`error`，`auth_prompt` 落在末尾被**丢弃**；契约
（`server.auth_challenge`）与 UI（`ServerAuthChallengeModal`）都齐备，唯独没有生产者；
`server_auth_respond` / `server_auth_cancel` **永远返回 `{"accepted": false}`**。

**修复**（4 个文件，+543/−24）：

| 位置 | 内容 |
| --- | --- |
| `desktop_terminal.hpp` | 新增 `AuthChallenges`：待答挑战的**纯状态机**（`begin`/`take`/`cancel`/`sweep`/`forget_session`），TTL `180 s`。放在头文件里是为了让它能被直接单测而不必拉起 PTY |
| `desktop_terminal.cpp` | 会话记录启动时的 `username`/`host`；`handle_frame` 遇到 `auth_prompt` 时注册挑战并 `emit("server.auth_challenge", …)`，字段与 schema 逐字段对齐；新增 `respond_auth` / `cancel_auth`，并在 `close()` 里 `forget_session` |
| `ipc_router.cpp` | 两个命令真正接上 `terminals_`：读到挑战 → 编码成 `{"type":"auth_response","responses":[…]}` 写进该会话 stdin；未知/过期/已答/条数不匹配**返回显式错误** `AUTH_CHALLENGE_UNKNOWN`，不再谎报 `accepted:false` |
| `test_database.cpp` | 替换那条把旧行为钉死的断言（`accepted:false` → 显式错误码、且不带 `accepted` 字段）；新增挑战状态机用例 |

**两个关键设计点**（都是被服务端语义逼出来的）：

1. **条数必须在宿主侧先校验**。服务端在 `responses.size() != expected_auth_responses` 时会把
   会话标记 `invalid` 并**直接失败**（`native/src/ssh.cpp:1397-1402`），所以把用户的错误条数
   转发过去等于**答错一次就把终端打死**。因此 `take()` 在条数不符时拒绝**且不消耗**挑战
   （用户可以重输），并把期望条数写进错误文本。
2. **挑战只能答一次、且会过期**。`take()` 成功即销毁条目；`close()` 会 `forget_session`
   清掉该会话的挑战，避免留下指向已死 PTY 的可答 id。

**验收**：

```
ctest --test-dir native/build-nmake -C Release
100% tests passed out of 13      （desktop_host_database_tests 9.04s、sidecar_tests 1.74s）
```

**差异回归（变异，逐个还原并核对 SHA256）**：

| 变异 | 结果 |
| --- | --- |
| M1 去掉「条数必须等于期望」的校验 | `a response count the challenge did not ask for must be refused with the expected count` → **exit 1** |
| M2 答完后不销毁挑战 | `a challenge must be answerable exactly once` → **exit 1** |
| 还原后 | `yukinal_desktop_host_database_tests.exe` **exit 0**，头文件哈希与修复版逐字节一致 |

**未覆盖 / 已知边界（如实）**：

- **没有端到端 PTY 行为测试**：`auth_prompt` 帧来自真实的 `yukinal-ssh-service` 子进程，本机
  要构造一个会发 keyboard-interactive 挑战的 SSH 服务器才能跑通全链路。已覆盖的是：挑战状态机
  （纯逻辑，4 组规则）、事件载荷与 schema 的字段一致性（按 schema 逐字段构造）、以及 IPC 层
  对未知 id 的显式拒绝（`test_database.cpp` 的 router 断言）。**「帧真的被写进 stdin」这一步只有
  代码路径与 `write_frame` 的既有测试作为依据，没有针对 auth_response 的字节断言**；
- `server.auth_challenge` 的 `username`/`host` 来自启动时的 target 元数据；取不到时退化为
  prompt 的 `name`、再退化为 `"unknown"`/`serverId`（schema 要求非空），属**显示值**而非认证值；
- UI 侧（`apps/desktop`）本轮**未改**：它本来就订阅该事件并按 `{authId, responses}` 回答，
  与宿主现在发出的载荷一致；但没有在真实 WebView 里点过一次。

---

## 8. 写入面与并发纪律

多个 agent 同时改同一个仓库，因此对写入范围做了显式切分，避免互相覆盖：

| 任务 | 独占写入范围 |
| --- | --- |
| FIX-3 | `scripts/check.mjs`、`.github/workflows/check.yml`、`native/CMakeLists.txt` |
| FIX-4 | `scripts/package.mjs`、`scripts/lib/release-artifacts*.mjs`、`docs/release.md`、`docs/cpp-desktop-handoff.md`（载荷布局段落） |
| FIX-5 | `services/control-plane/src/yukinal_control_plane/**`、`services/control-plane/tests/**` |
| R1/R2/R3 审查者 | 只读源码；各自只写一份 `docs/review/*.md`；临时探针只写 `target/`、`.tmp/`、`%TEMP%` |
| Lead | 最终汇总文档、跨文档一致性修正、统一跑门禁 |

**审查者被明确要求不跑 `node scripts/check.mjs`**（完整门禁耗时且会与写入者竞争），
由 Lead 在所有写入者停下后统一跑一次。这一条有实际价值：事后核对发现
`scripts/package.mjs` 的 mtime 落在某次门禁运行窗口内，但因为 `check.mjs` 全文不引用
`package.mjs`，那次门禁结果确实不受影响——这一点被如实记录，而不是含糊带过。

**未跟踪外部文件**：`ssh_agent_development_brief.md` 是外部任务输入，全程保持未跟踪、不提交，
且仓库构建与门禁都不依赖它。

---

## 9. 修复后门禁

```
$ node scripts/check.mjs
check: green          exit=0
```

14 个步骤逐项（**最终实测**，`node scripts/check.mjs` 在当前工作树上 `check: green`、`exit=0`）：

| # | 步骤 | 实测 |
| --- | --- | --- |
| 1 | publication hygiene | green（609 tracked files） |
| 2 | secret scan | green（633 files checked） |
| 3 | cross-platform command runner tests | 7 pass |
| 4 | release artifact tests | 24 pass |
| 5 | gate runner guardrail tests | 2 pass |
| 6 | docs links | green（39 markdown files, 207 links） |
| 7 | configured native CTest suite | `100% tests passed out of 13` |
| 8 | control plane tests (SQLite) | `Ran 149 tests` OK |
| 9 | Python Agent tests (full unittest discovery) | `Ran 167 tests` OK（skipped=4） |
| 10 | build contract libs | OK |
| 11 | typecheck | OK |
| 12 | unit tests | shared 142 / provider-sdk 4 / agent-sdk 5 / desktop 325（324 pass + 1 skip） |
| 13 | desktop bundle (vite build) | OK |
| 14 | Python Agent source sidecar smoke | green（含 `clean shutdown on stdin close`） |

相对第一批完成时：控制平面 139 → **149**、Agent 153 → **167**、shared 135 → **142**、
desktop 322 → **325**、CTest 目标数与用例只增不减；`check: green` 保持不变。

**文档门禁对审查报告本身也是绿的**：把 10 份报告与两份交付文档临时纳入扫描集合
（`git add -N docs/review docs/review-report.md docs/review-fixes.md`）后，
`publication hygiene`（621 tracked）与 `secret scan`（625 files checked）仍为 green —— 即这些报告
不含私有阶段号、未发布文档指针（章节符号加数字形式的相对引用）或形似凭据的字面量。**这一条不是走过场**：
该组合第一次运行时确实红了一次——`docs/review-report.md` 里一处「见」加章节符号加数字的引用被
`§NN` 规则命中，改成中文小节名后转绿。这也说明「把报告临时纳入跟踪再跑」是值得保留的收尾步骤，
而不是只在跟踪文件上跑一次就宣布合规。

---

## 10. 未修项与遗留代价

### 10.1 第二批之后仍然**没有**修（每条都有理由）

第一轮刻意没有顺手改这些，它们随后各有独立任务与反向验证（进度见
[`review-report.md`](./review-report.md)）；而**第二批的证伪又暴露出三条新的**，此处一并列出：

- **P0-1**：签名规范串不含 node id + `public_key` 无唯一约束 → 同一把 Ed25519 公钥登记进两个租户后，
  仅改未签名的头即可跨租户读写（读：P66/P99；写：P98）。前置条件是该公钥被登记进两个租户
  （schema 与 API 都允许）。修法涉及签名规范与数据库约束，会让既有节点凭据与部署失效。→ **FIX-6**
- **P0-2**：`unknown→running` 绕过派发门禁（门禁只在 `queued→running` 调用，
  且 `task_authorisation_problem` 不比对 tenant / node / fingerprint）。→ **FIX-6**
- **P0-4 剩余 9 条**：禁用租户对其余 9 条含 `{tenant_id}` 的路由仍无效（FIX-5 只修了
  完全没有门禁的 `approvals/expire`）。→ **FIX-6**
- 审批不绑定 tenant / node / fingerprint；生命周期 4 条永久挂起 → **FIX-7**；审批入口的误导性回执
  （R2-H-1）与 `consume` 占用宿主 worker（R2-H-2）→ **FIX-9**；keyboard-interactive 宿主接线
  → **FIX-11**；零生产者事件通道与 13 条 verbatim 转发命令的响应闸门 → **FIX-10**。
- 各报告的 Medium / Low（`service_status` 未加引号、`SshResources` 析构半释放、
  `pnpm check` 无 lint / 静态分析等）继续留在各报告正文里。

**第二批证伪暴露的三条新问题**（都不在原始审查里，是「验证修复」本身产生的）：

- **nonce 上限可被并发突破，且会拒绝合法节点** → **FIX-17**。这是最重要的一条：一个写在
  README 里、被称为 security bound 的上限，实测在 64 并发下变成 188 行；同时它把「可恢复的
  限流」伪装成「你的签名无效」。
- **FIX-7 的两个安全网不可回归**（删掉守卫 0 条测试变红）→ **FIX-18**。代码是对的，缺的是读者。
- **FIX-6「24/24 路由」在测试里被压缩成一次遍历**（CP-6a/6b/6c 共用同一个测试）。这条**尚未**开任务；
  它是一个真实的覆盖收缩：路由表若新增一条含 `{tenant_id}` 的路由而测试的集合断言同时被放宽，
  两条改动各自都合法、合起来就漏了。**留给下一轮**，不假装已经处理。

### 10.2 需要外部环境才能闭环

- **native H-2 的现场触发**：本机 Windows 无 `signal.SIGPIPE`、WSL 无可用发行版、无 Docker，
  **无法在 Linux 上实测 RST → SIGPIPE 终止**（静态链完整）。FIX-8 因此只交付了**源码级护栏**
  （断言 `#include <csignal>`、唯一一处 `std::signal(SIGPIPE, SIG_IGN)` 位于 `#ifndef _WIN32` 内且
  晚于 `int main(`、早于 worker/HTTP 派发），并**明确标注它不是行为测试**。建议在 Linux CI 加
  RST 注入用例闭环。
- **native H-1 的真实触发**：FIX-8 用 paramiko mock 证明了「服务器不发 exit-status 会得到假成功」
  与「大退出码被误判为 unknown」，但真实 OpenSSH 的行为未验证。
- **完整 C++ 重构建**：本机 WebView2 SDK `1.0.1150.38` 低于 `native/CMakeLists.txt` 要求的
  `1.0.2903.40`，完整 `cmake --build` 在本机无法跑通（**环境限制，非源码缺陷**）；
  逐 target 构建正常。打包复用已有的 `yukinal-desktop-host.exe`，因此 FIX-4 不受影响。
- **控制平面 P0-1 是破坏性客户端契约变更**：节点驱动必须把**自己的 node id** 签进规范串。
  本轮已确认仓库内 `agent/`、`native/`、`packages/`、`apps/` 下没有任何签名辅助的消费方
  （`X-Yukinal-*` / `sign_headers` / `canonical_request` 命中 0），所以改动被限制在
  `services/control-plane/` 内；但**版本号与外部节点驱动的升级说明**仍待维护者决定。
- 真实 OpenSSH 容器验收（需 Docker）、真实云 Provider live（需 API key）、
  控制平面的 PostgreSQL 部署与 TLS 终止。

### 10.3 已接受的行为变更

`--verify` 现在枚举整个输出根，所以升级 `APP_VERSION` 后输出根下遗留的旧版本载荷目录会让校验失败。
这是**期望行为**（旧载荷不应与新清单混放），已在 `docs/release.md` 写成显式注意事项：
清理旧目录或改用新的 `--out`。

### 10.4 一条仍然存在的门禁边界

`check-publication.mjs` **只扫被跟踪文件**（`git ls-files -z`）。因此未被跟踪的
`docs/review/`、`docs/review-report.md`、`docs/review-fixes.md` 目前不受该门禁保护——本轮靠人工
临时 `git add -N` 后实测才确认合规（并因此发现并修掉一处 `§NN` 引用）。
若这些报告要随仓库发布，需要先把它们纳入跟踪，届时该门禁才会自动覆盖。
