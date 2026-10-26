# 第五轮审计与修复（2026-10-26）

配套文档：[三轮审查总报告](./review-report.md) ｜ [第四轮审查](./review-round4.md) ｜ [前两批修复说明](./review-fixes.md)

第四轮之后工作树停在一个**中间态**：功能改动（单 Agent + subagent、阶段事件、运维流程、许愿式变更）已写完，
但契约、门禁与 UI 都有未收口的裂口。这一轮先把门禁拉回绿，再补齐两侧契约，然后修掉第四轮已确认但没做的
UI 缺陷，最后清掉角色路由的残留。本文记录**实测到的**问题、修法与反向验证；每条都带 `file:line` 或可复现命令。

## 1. 起点：门禁是红的，而且红在三个不同的地方

`node scripts/check.mjs` 在第 7 步就失败：

```
check-docs-links: 1 problem(s)
  docs/workspace-tabs-spec.md: no heading for anchor -> ./security.md#用户批准意味着什么
✗ docs links failed (exit 1)
```

分开核实后，这是**三个独立问题**，而不是一个：

| # | 现象 | 根因 | 证据 |
| --- | --- | --- | --- |
| 1 | 类型检查失败 3 处 | `AgentCoordinationConfig` 已从契约删除，但 `useAgentRun.ts` 仍在导入并发送 `coordination`；预览 fixture 仍在 `switch` 里引用已删除的 `agentTeamTasks` | `tsc` 输出；`grep -rn coordination apps/desktop/src` |
| 2 | 文档锚点找不到 | **文档门禁自己坏了**：`check-docs-links.mjs` 按 `"\n"` 切行，三份文档是 CRLF，`/^#{1,6}\s+(.*)$/` 在行尾 `\r` 前匹配失败 → 该文件的锚点集合是**空集** | 探针：对 `docs/security.md` 调用旧 `anchorsOf` 得到 `[]`，而标题明明存在 |
| 3 | 根目录出现未跟踪的 `Testing/` | CTest 从仓库根被调用时会在根下写 `Testing/`，`.gitignore` 只忽略了 `native/build-*/` | `git status --porcelain` 显示 `?? Testing/` |

问题 2 值得单独说：**门禁的失败信息指向了错误的对象**。链接本身是对的（标题真的存在），坏的是检查器。
如果按错误信息去改文档，就会把一个正确的链接改错。修法是把它抽成 `scripts/lib/doc-anchors.mjs` 并在
`headingAnchors()` 里**先**做 `\r\n`/`\r` 归一化，再匹配；同时新增 `scripts/lib/doc-anchors.test.mjs`
（5 条用例，含「CRLF 与 LF 必须得到相同锚点」「围栏内的 `#` 不是标题」）并把它作为门禁的第 7 步。

> 反向验证：把 `docs/security.md` 改回 CRLF 后，**新的**门禁仍然是 green；旧的实现会报错。
> 这条差异就是「修好检查器」与「改坏文档」的区别。

## 2. 契约两侧：9 条通道里 3 条其实有生产者

第四轮的记录说 9 个事件名「没有任何生产者」。逐字扫描 `native/**` 与 `agent/src/**` 后发现这条记录已经过期：

| 通道 | 实际情况 | 证据 |
| --- | --- | --- |
| `agent.thinking` | **有**生产者：`_public_content_parts` 把 `thinking` / `reasoning` / `reasoning.summary` 映射成该类型，`_stream_model` 逐块转发 | `agent/src/yukinal_agent/model_agent.py:1523,1527,1532`、`:863` |
| `agent.usage` | **有**生产者：每轮模型调用后都发 | `model_agent.py:417` |
| `server.auth_challenge` | **有**生产者：keyboard-interactive 挑战到达时发（FIX-11 的成果） | `native/desktop_host/src/desktop_terminal.cpp:535` |
| `investigation.schedule_notification` | **有**生产者：调度器的通知 sink | `native/desktop_host/src/ipc_router.cpp:6480` |

因此把 `NO_PRODUCER` 从 9 收紧到 5（`server.connected` / `server.disconnected` / `server.updated` /
`terminal.output_lost` / `activity.created`），并逐条改写 `events/index.ts` 里那些「无生产者」的注释。
**这是把记录改对，不是把缺口藏起来**：剩下 5 条仍然没有生产者，仍然逐条保留了理由。

同时把 `agent.stage` 的 `report` 阶段补上了真正的生产者。此前 `STAGES` 声明了 `report`，运行时从不发，
是桌面在完成时**本地假定**的（`useAgentRun.ts` 里 `setStage("report")`）。现在由运行时在产生最终回答时发一次
（`model_agent.py::_advance_answer_stage`），桌面不再自行推断 —— 声明、运行时与界面三者一致。

## 3. UI：第四轮确认未修的项

### 3.1 终端退出码：「未知」曾被显示成「成功」

`TerminalPane.tsx` 原来的写法是 `payload.exitCode === null ? "" : " (exit N)"`，于是
`exitCode: null`（远端状态未知）与一次正常结束在视觉上完全一样。这正是 `terminal_exit.hpp` 存在的理由
（假成功会让人以为备份跑完了）。

现在抽出 `terminal-exit.ts`：

```ts
terminalExitLabel(null)  // "远端退出状态未知"
terminalExitLabel(0)     // "正常结束 (exit 0)"
terminalExitLabel(7)     // "异常结束 (exit 7)"
```

三条文案互不相同，且用一条纯函数测试钉住「三条必须互不相等」。抽成模块的原因是
**xterm 的写入无法被 `renderToStaticMarkup` 观察到**，把文案留在 JSX 里就没有任何测试能覆盖它。

### 3.2 `result_unknown` 在工具卡里退化成普通失败

活动页（`activity-view.ts`）早就会为 `result_unknown` / `timed_out` / `cancelled` 追加「效果可能已发生」；
但对话里的工具卡（`transcript.ts::toolOutcomeFromEvent`）**把这个字段丢了**，于是同一份证据在两处读出两个意思：
一处是「未知」，一处是「失败」。现在 `ToolOutcome` 带上 `executionState`，工具卡渲染
`toolExecutionNotice()`，并且文案刻意不写「失败」—— 对一个可能已经成功的重启说「失败」，就是让人去重试它。

### 3.3 `window.confirm` 还剩两处

第四轮把对话记录的删除改成了行内确认，但记忆库与服务器列表仍用 `window.confirm`。
它冻结 WebView2、盖住判断所需的上下文，也是这套界面里唯一一个不属于它的对话框。两处都改成行内确认条
（说明后果 → 取消（默认焦点）→ 破坏性动作），并因此改写了「什么可以用 `disabled`」这条既有规矩：

- `AgentHistoryPane` 的「重置上下文」原本是 `disabled={resetting || busy}`，它在记录列表的表头里，
  同样是焦点流程的一部分。改成 `aria-disabled` + 早退，硬 `disabled` 的允许计数从 3 降到 2。
- 记忆库的「重置全部上下文」同理。

### 3.4 消息框缺少 `combobox` 语义

输入框本来就有 `aria-autocomplete="list"`、`aria-controls`、`aria-activedescendant`，但**没有**
`role="combobox"` 与 `aria-expanded`。ARIA 1.2 里那三个属性只在 combobox 上成立；缺了这两项，
读屏不知道这个输入框带弹出列表，也不会宣告列表开合 —— 键盘导航能工作，但对辅助技术不可见。
另外列表的 `aria-label` 从「可提及的队伍与服务器」改为「可提及的服务器」（队伍已不再是可提及对象）。

## 4. 重构：角色路由的残留

角色选择器已经不在界面上，但它的机制还留在仓库里：

| 残留 | 状况 |
| --- | --- |
| `agent-nist.ts`（218 行） | 十三个已退役角色的提示词正文、localStorage 读写、`parseAgentRoles`、`handoffAgentRole`、`resolveAgentRole` 的 `@mention` 路由 —— **`apps/desktop/src` 里零个调用方**，只有测试在调用 |
| `agentRoleId` / `setAgentRoleId` | workspace store 里只剩 `MemoryLibrary` 一处写入 `null` |
| `roles` / `selectedRoleId` / `onSelectRole` 三个 prop | `AgentComposer` 与 `ConversationSettings` 里的 `@deprecated` 类型垫片，实际传入的是 `[]` / `null` |
| `agent_team_tasks` | C++ 宿主仍保留路由分支，而 Python sidecar 已改名为 `agent.subagents.list` |
| `ConversationStore.save_team_task` / `team_tasks` | 无调用方（只保留旧表的迁移读取） |

处置：

- 新建 `agent-teams.ts`（62 行）：只保留**持久化数据仍需要**的部分 —— 团队 id 词表与
  `agentTeamLabel()`（七只历史队伍一律读作「运维 Agent」）。它没有 storage、没有 provider、没有 `window`、
  没有 store —— 旧路由正是从这些依赖里长回来的。
- 删除 `agent-nist.ts` 与三个 `@deprecated` prop，删除 `agentRoleId` 状态。
- C++ 宿主去掉 `agent_team_tasks` 的三处引用。
- `ConversationStore` 删掉两个无调用方的写/读方法；**保留** `coordinator_team_tasks` 表与
  `subagent_tasks()` 的单向迁移读取（老安装的数据还在那张表里）。
- 测试：`agent-nist.test.ts` → `agent-teams.test.ts`，13 条只测旧路由（角色目录投影、mention 路由、
  provider 绑定、handoff）的用例随实现删除；保留并改写「历史 id 仍读作同一个 Agent」。

## 5. 门禁与测试（修复后实测）

```
$ YUKINAL_SSH_CMAKE_BUILD_DIR=native/build-full node scripts/check.mjs
check: green          exit=0          （19 个步骤，18 绿 + 1 组合步骤）
```

| 步骤 | 实测 |
| --- | --- |
| publication hygiene / secret scan | green |
| cross-platform command runner tests | 7 pass |
| release artifact tests | 26 pass |
| Agent runtime symlink tests | 3 pass + 1 skip（Windows 文件符号链接需要开发者模式） |
| gate runner guardrail tests | 13 pass |
| **docs anchor helper tests（新增）** | 5 pass |
| docs links | green（54 files / 268 links） |
| build native test targets | 绿（NMake + vcvars 自动发现） |
| configured native CTest suite | `100% tests passed out of 14` |
| control plane tests (SQLite) | `Ran 157 tests` OK |
| Python Agent tests | `Ran 324 tests` OK（skipped=4） |
| build contract libs / typecheck | green |
| unit tests | shared **146**、provider-sdk 4、agent-sdk 6、desktop **617** |
| desktop bundle | 371 modules |
| Python Agent source sidecar smoke | green（含干净关闭） |

`node scripts/check-ai.mjs` 单独复核：`Ran 183 tests` OK（新增 `test_agent_stage` / `test_operations_policy` /
`test_agent_auto_approval` 入闸）。

## 6. 反向验证

每条修复都做了「把它改回去，确认测试会红」：

| 修复 | 变异 | 结果 |
| --- | --- | --- |
| 文档锚点归一化 | 把 `docs/security.md` 改回 CRLF | 新门禁仍 green（旧实现会报错） |
| 终端退出码文案 | 让 `null` 与 `0` 共用一句 | `terminal-exit.test.ts` 的「三条必须互不相等」用例红 |
| `result_unknown` 提示 | 删掉 `toolExecutionNotice` 渲染 | `conversation-navigation-ui` 的未知结果用例红 |
| 行内确认 | 恢复 `window.confirm(` | `ui-workflow-quality` 的源码断言红 |
| `combobox` 语义 | 去掉 `role="combobox"` | `team-chat` 的 combobox 用例红 |
| 阶段 `report` 生产者 | 去掉 `_advance_answer_stage` | `test_agent_stage` 的两条 `report` 用例红 |
| 事件词表 9→5 | 把 `agent.usage` 放回 `NO_PRODUCER` | `event-vocabulary` 的「既是生产者又记为无生产者」用例红 |

## 7. 仍然没修 / 本机无法验证

**不要把「门禁绿」读成「没有缺陷」。** 以下是这一轮**确认存在但仍未处理**的项。

### 7.1 需要架构调整

- **`agent_run_start` 的同步 WebView 阻塞**：`delivery: "sync"` 仍会让一次调用最长等 15 分钟。
  FIX-20 已让宿主在超时后取消 Python 侧 run，但界面没有「正在取消」的反馈。根治要把该命令改成
  「受理即返回 + 事件推进」。
- **`ConversationStore` 在事件循环上同步执行 sqlite3**：单次毫秒级，但长事务会卡住整个 sidecar 的帧处理。
- **`serve_stdio` 每帧 `create_task` 无上限**：异常客户端可瞬间堆积任务。
- **`_provider_list` 串行发起最多 8 s 的凭据 RPC**：多 provider 时线性叠加。

### 7.2 UI（本轮未做）

- **`AgentFeed` 无虚拟化**：`MAX_TRANSCRIPT_ENTRIES = 500` 全量渲染。500 行以内可接受，
  但长对话滚动性能从未实测。
- **2.3 MB 的 Nerd 字体仍被无条件打包**（`dist/assets/JetBrainsMonoNLNerdFontMono-Regular-*.ttf`）。
  它只在用户显式选择 `jetbrains-nerd-mono` 时才会用到；`font-display: swap` 让首屏不受阻，
  但它仍在安装包里。按需加载需要把 `@font-face` 拆到独立 CSS 并在选择时注入 —— 本机无法验证
  打包后是否真的生效，所以没有动。
- **`#` 锚点之外，Markdown 的表格/代码块样式仍未在真实 WebView 里手验**。
- **`AgentComposer` 的候选列表在 `inert` 期间仍渲染上一次的内容**（这是为了退场动画，有注释说明），
  读屏在退出动画的 160 ms 内可能仍能读到它。

### 7.3 本机无法验证

- **PostgreSQL 路径**：控制平面的 CAS / 唯一约束只在 SQLite 上验过。
- **Linux `SIGPIPE` 的行为级验证**：仍然只有源码级护栏。
- **真实 OpenSSH / 真实云 Provider / 安装包 GUI**：本机没有 Docker、没有 API key。
- **WebView2 GUI 目标**：本机 SDK `1.0.1150.38` 低于要求的 `1.0.2903.40`；
  本轮通过 `YUKINAL_SSH_CMAKE_BUILD_DIR=native/build-full` 跑的是**已配置**的构建树，
  它的 `desktop_host` 目标在本机可以编译（`native/build-full` 的缓存里 WebView2 已解包），
  但这不等于「全新机器能从零构建 GUI」。

### 7.4 验证能力边界

- 桌面 UI 仍然是 **41+ 个文件全是 `renderToStaticMarkup`**，没有 DOM 环境：hook 内的状态机只能靠
  「纯函数 + 源码切片断言」钉住，不是真正的渲染级验证。
- 本轮新增的两处「文案必须在渲染里正确」的断言（终端退出、未知结果）都是**纯函数**级的；
  真正写进 xterm / 交给读屏的那一步没有自动化覆盖。

## 8. 复核方式

```bash
# 1. 权威门禁（普通 shell 即可，工具链由门禁自己发现）
node scripts/check.mjs

# 2. 显式指定一个已配置的原生构建树（推荐，避免「没有构建树 → 跳过 native」）
YUKINAL_SSH_CMAKE_BUILD_DIR=native/build-full node scripts/check.mjs

# 3. 门禁自身的护栏与新增的文档锚点测试
node --test scripts/lib/gate.test.mjs scripts/lib/doc-anchors.test.mjs

# 4. 分语言单独跑
ctest --test-dir native/build-full -C Release --output-on-failure --no-tests=error
python -m unittest discover -s services/control-plane/tests -p "test_*.py"
python -m unittest discover -s agent/tests -p "test_*.py"
pnpm -r --if-present typecheck
pnpm -r --if-present test

# 5. 空白字符/换行污染
git diff --check
```
