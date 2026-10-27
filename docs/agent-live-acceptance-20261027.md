# Agent 实机验收记录 — 2026-10-27

一台全新 Debian 12 服务器（2 vCPU / 3.7 GiB / 40 GB xfs，仅开放 tcp/22）上的
端到端验收。目的不是复述功能，而是记录**界面观察与运行时事实之间的差异**，以及
每条差异的处置。

环境：Yukinal 2.0.1，`native/build-full` Release 构建，DeepSeek (`deepseek-flash`,
`effort=max`)，运行模式 `目标模式`，数据目录
`%APPDATA%\dev.yukinal.workspace`。

观测手段：`scripts/watch-agent-run.py` 以只读方式同时跟随
`agent-state/conversations.sqlite::conversation_events` 与
`audit/python-agent-tools.jsonl`。它不参与运行，也不写产品数据库。

## 四轮实际运行

| 轮次 | 请求 | 模型轮次 | 工具调用 | 审批 | 结论 |
| --- | --- | --- | --- | --- | --- |
| 1 | 健康快照 + 磁盘/内存/端口三项确认 | 3 | 6 | 3 | 事实全部核对通过 |
| 2 | 建 `/tmp/yukinal-probe.txt`，读回，删除，确认 | 6 | 6 | 4 | 事实全部核对通过 |
| 3 | 建 `/tmp/yukinal-probe2.txt` 并删除 | 5 | 4 | 3 | 事实通过，但含未验证断言 |
| 4 | 建 `/tmp/yukinal-probe3.txt`，**拒绝**审批 | 1 | 1 | 1（拒绝） | fail-closed 正确 |

所有被引用的数值（磁盘 6%、inode 1%、内存 3.73 GiB/11%、`SwapTotal: 0 kB`、
revision 前缀、`ABSENT`、`bytesWritten: 77`）都与原始工具输出逐字一致。没有发现
编造数值。

## 已修复的缺陷

### 1. 只读命令被报告为「变更」

`server.exec` 以 `risk_level="destructive"` 注册——这对授权是正确的，注册表无法
知道任意命令做什么——但 stage 派生也用了这个声明风险，于是：

```
13:10:39  intake -> change     ← 只因为执行了 `date -Is`
13:10:56  verify -> change     ← 只因为执行了 `test -e`
```

一次真实的写入被报告成四次变更，UI 上的阶段指示器在 `change ⇄ verify` 之间抖动。

**修复**：`agent/src/yukinal_agent/command_effect.py` 增加一个小而保守的分类器，
只把由纯读取命令组成的链（`date`/`df`/`grep`/`test`/`cat` 等，无管道、重定向、
命令替换、`VAR=` 前缀）判为读。`model_agent._effective_risk()` 只在**派生 stage 时**
使用它，`derive_stage` 与注册表声明均未改变，因此授权、审批票据与策略判定完全不变。
未识别的一律按声明风险处理（fail-closed）。

已知取舍：带引号交替的 `grep -E '^(A|B):'` 会被保守地判为 effect，因为它含有 `|`。

### 2. 健康检查被迫走 destructive 审批

`server.info` 的 `memory` 只有 `availableBytes/totalBytes/usagePercent/usedBytes`，
没有 swap；`disks` 没有 inode；也没有任何「某路径是否存在」的读。于是三轮里出现了
`grep /proc/meminfo`、`df -hT`、`df -i`、`test -e` 四次 `server.exec`，每次一次
critical 审批。

**修复**：`agent/src/yukinal_agent/read_commands.py` 注册五个**命令为常量**的只读工具：

| 工具 | 固定命令 |
| --- | --- |
| `system.swap` | `grep -E '^(SwapTotal\|SwapFree\|SwapCached):' /proc/meminfo` |
| `filesystem.inodes` | `df -i -x tmpfs -x devtmpfs -x squashfs` |
| `filesystem.mounts` | `df -hT -x tmpfs -x devtmpfs -x squashfs` |
| `system.time` | `date -Is` |
| `filesystem.exists` | 由**已校验**路径拼出的单条 `test -e`（仅输出 PRESENT/ABSENT） |

安全论证与 `server.exec` 不同：这些命令是本文件里的常量，模型不提供任何 shell 文本，
没有可注入的位置。它们仍走同一条 `server.exec` 宿主通路，因此输出上限、超时和脱敏
原样适用。目录清单被刻意保持得很小——其余一切仍然经过 `server.exec` 与人工批准；
把这类命令逐条加进清单会变成一个审核更松的第二 shell。

`filesystem.exists` 额外限制：绝对路径、长度 ≤512、禁止空白/引号/shell 元字符、
禁止 `..`/`.` 段、必须落在允许的目录前缀下。

### 3. 拒绝后审计没有终态行

批准路径写 `started → succeeded(approval=approved)`；拒绝路径只有
`started → awaiting_approval`，且**批准后重新执行会生成新的 event_id**。审计因此
无法回答「这次调用最后被怎么决定了」——那个事实只存在于会话事件流里。

**修复**：`AuditEvent` 增加 `call_id`（agent 级调用标识），`ToolRegistry` 把它写到
每一次尝试行；`ToolRegistry.audit_decision()` 新增，由 sidecar 在
`agent.approval.respond` 时以 `status="rejected"|"approved"` 落一行。该写入是
best-effort：决定本身是 RPC 的结果，不能因为审计 sink 失败而丢失（与既有
`_record_run_event` 的处理一致）。

### 4. 审批等待被算进工具耗时

`filesystem.write` 的 `agent.tool_result.durationMs` 是 10613 ms——其中 10.5 秒是
人在犹豫。UI 直接展示这个数字，会读成「这个工具很慢」。

**修复**：`agent.tool_result` 增加 `approvalWaitMs`，与 `durationMs` 并列上报。
`durationMs` 保持语义不变（工具自身执行时间）。

## 已记录、未修复

### 5. 端到端恢复 —— 已修复（规则：同窗口继续，不同窗口重开）

`ModelDrivenAgent` 的 checkpoint 恢复能力本身是真的，但桌面链路曾走不到：UI 重发必然
生成新 `runId`，thread id 又由 `runId` 派生，旧 checkpoint 永不被查询。

实测（修复前）：审批卡片出现时杀掉 host，重启后在同一会话重发同一句话——新 runId
`run_84fcada5`、新 thread `agent-0b9e8fcd`、模型重跑一轮、指纹整体改变。

**修复**：把"定位"与"校验"拆成两件事。

- `ExecutionContext` 之外新增 `checkpoint_key`：一个按**会话轮次**稳定的定位键。
  thread id 变为 `hash("turn", tenant, user, checkpoint_key)`。
- 侧车把该键持久化在 `conversation_turns.metadata_json.resumeKey`，格式为
  `runid:<hash>` 或 `turn:<digest>`；`digest` 取 `(sessionId, targetId, mode,
  permissionMode, policyId, toolAccess, planBinding, promptText, taskId)` 的规范化哈希。
  会话 ID 是按窗口分配的，因此**同一句话在另一个窗口得到不同的 digest → 不同的线程
  → 从头开始**，正是要求的规则。
- 键只能来自那张表：把它写进对话文本没有用，模型无法用归档内容铸出一个键。
- `checkpoint_binding` 仍然是**校验器**：作用域、目标、允许工具、预算、goal 模式任一
  不符就 `checkpoint_context_mismatch`。
- `run_id`/`trace_id` 不再参与绑定——它们是"第几次尝试"，不是授权。有 turn key 时
  `task_id` 也不参与（durable task 身份已由 `checkpoint_binding` 覆盖）。
- 已经结束的轮次不是可恢复的：重复请求会丢弃旧线程并**作为新请求**执行，答案不会被
  重放。丢弃失败只是家务事，不会让运行失败。
- `agent.context.reset` 现在同时清理两种 thread id，否则重置后仍会留下可恢复的挂起审批。
- `_checkpoint_matches` 支持"归档前缀 + 本轮消息"的形态，因为恢复时的历史投影比首次
  运行更长；它只比较**已知的授权字段**，历史文本不参与。

回归：`test_agent_live_acceptance_fixes::PendingApprovalSurvivesARestartInTheSameWindow`
三个用例（重启后交回同一审批且只执行一次 / 已结束轮次不重放 / 不同窗口重开）。

### 5b. 自动批准下可以连改（修复过程中发现）

`approval_ask` 模式下这一步已被关掉：拒绝后同一动作被 `approval_denied` +
`tool_repetition_detected` 双重拦住。但 `自动批准` 模式下没有人看到第一次变更的结果，
模型可以在同一个推理里接着下发第二个变更。

**修复**：`permissionMode == "auto"` 时，一次真实变更（按 `_effective_risk` 判定，纯读的
`server.exec` 不算）之后的下一轮是**只读确认轮**：写类调用在派发前被拒绝，模型收到
"先确认已经发生了什么"的指令后再作答。拒绝次数上限为 2，超过以
`confirmation_turn_required` 收束，避免模型把预算耗在被拒绝上。

这是本轮唯一超出原始清单的改动，且是修复端到端恢复时才暴露的：把拒绝路径接回模型后，
"自动批准"这条更宽的通路必须有对称的约束。

回归：`test_agent_live_acceptance_fixes::AutoApprovalConfirmationTurn`。

### 6. 拒绝后模型拿不到失败结果 —— 已修复

拒绝时运行曾把 `status` 设为 `succeeded` 并直接收束，答复是运行时写死的
「已拒绝当前操作，后续执行已停止」。循环没有回到模型（`steps=2`：一模型一轮工具），
会话存档里留下一条没有对应 `ToolMessage` 的孤儿 `tool_call`。

**修复**：拒绝/过期现在把 `approval_denied` 阶段的 `ToolMessage` 交回模型，并让运行
继续一轮。要点：

- 答复由模型写，运行时不代写结语；
- `approval_denied` 不是需要诊断的观测失败，所以它清除 `recovery_read_required` /
  `recovery_diagnosis_required`，不会被误当成"需要更多读"；
- `_repeated_unchanged_call` 把 `approval_denied` 加进去：**同一动作再提一次会被
  `tool_repetition_detected` 拒绝**，因为重新提问就是把一次"不同意"变成"请你换个说法"；
  这也是防止模型刷审批卡片的唯一硬约束；
- 该次重试的提示语被改写成「用户未批准这个动作，这是同一个动作」而不是通用的重复调用
  文案，避免模型把"被拒绝"读成"结果没变化"。

回归：`test_agent_model_loop` 的拒绝/过期两个用例改为断言模型收到 ToolMessage 且
`tool_call_id` 与 assistant 的调用一致；`test_agent_capability_gates` 新增
`RejectionReachesTheModelGate`，覆盖"模型拿到拒绝 → 重试被拒 → 由模型报告 blocker"全链。

### 7. 未验证断言

第 3 轮没有调用 `filesystem.list`，答复里仍写「`/tmp` 其他文件没有改动」。第 2 轮它
调了 `list` 才这么说，第 3 轮没调也照说。这与 `SAFETY_POLICY` 的
「do not present a value you did not read」冲突。

**已加固提示词**：`SAFETY_POLICY` 现在显式要求答复的每一句都能对应到本轮实际读到的
值，并把「nothing else changed」点名成一类**关于主机的断言**而不是客套收尾语——它需要
自己的一次读取，读不到就应当省略而不是软化。这条无法用代码强制（它是模型输出），
因此只由 `test_operations_policy` 钉住提示词文本，实际效果仍需下一轮实机观察。

### 8. 偏好迁移会静默放宽审批策略

**已修复**，见下节。

### 9. 每次工具调用约 1.35 s 固定开销

实测 `server.exec` 自身执行 616–703 ms，观测 1964–2060 ms。6 次调用约 8 s 纯开销，
占第二轮 23.5 s 的 35%。来源是「审计 started → 抛审批 → UI 往返 → 铸一次性票据 →
再审计 started → 执行」。第 2 项修复直接减少了需要审批的调用数，但单次开销未动。

## 顺带修复：偏好迁移

`preferences-store.ts` 的 `migrate` 曾无条件把 `agentPermissionMode` 重置为默认值。
任何一次持久化版本号 +1，用户手选的「每次询问」都会在没有提示的情况下变成
「自动批准」——一个安全相关的默认值被静默放宽。现在迁移只修复缺失或不可读的字段，
不覆盖有效选择；`apps/desktop/tests/preferences-migration.test.ts` 钉住这一点。

## 重复代码

`PermissionPicker.tsx` 定义了另一套批准方式选择器，但全项目没有任何地方 import 它。
真正生效的只有 `ConversationSettings.tsx` 的 `<select>`。未删除（避免超出本次范围），
但应清理，否则后来人会改错文件。

## 回归入口

```bash
pnpm check:ai
```

新增 `test_agent_capability_gates.py`（5 个能力门，打印可引用的证据）与
`test_agent_live_acceptance_fixes.py`（本文档 1–4 项与第 5 项的当前行为）。
桌面侧新增 `preferences-migration.test.ts`。

## 工具输出着色

C++ SSH 服务侧的 `server.exec` / `server.logs` 输出按字节原样返回，没有 ANSI 过滤——
这次**未改动 C++**。着色在渲染层完成，理由是可验证性：

- 从 `server.exec` 里剥掉 ANSI 会破坏「操作员看到的字节就是目标产生的字节」这一取证
  属性，而审计与事件流本来就只记录结构化字段。
- 在渲染层做，就有一个纯函数可以对着恶意输入写测试；在 C++ 里做，只能用字符串包含
  来断言，而且真正危险的路径（渲染）仍未被覆盖。
- 服务仍然是「只执行固定操作、返回有限字节」的薄层，没有为显示需求增加新的解析面。

实现：`terminal-colour.ts` 把输出解析为 `StyledRun[]`，只保留精确的
`ESC [ <SGR params> m`，其余控制序列（光标、滚动区、窗口标题、设备查询、剪贴板）全部
丢弃，其余 C0 控制字符剥除；`TerminalOutput.tsx` 逐片段渲染为 `<span>`，文本始终是
文本，不经过 HTML 注入路径。SGR → 语义色的映射用 `materials.css` 新增的
`--content-*` 变量，取值复用对话视觉模块已有的低饱和色，外壳与终端主题仍为单色。

已知限制：`TerminalPane` 里的 xterm 主题仍是单色，因此**交互式终端**里的
`ls --color` 依然不显示颜色；本次只覆盖 Agent 工具卡片中的命令输出。

## Computer Use 实机复验 — 2026-10-27

通过 Yukinal C++ 桌面宿主和已配置的 DeepSeek Provider，在已连接的测试服务器上运行一次只读 Agent 请求。
LangGraph 在第 1 轮调用 `server.exec` 读取 `uname -s`、`uptime`，再调用 `server.info` 交叉核对；
三次工具调用均成功，内核和运行时长相符。该请求没有写入、删除、安装、改配置、重启或停止服务。
操作记录显示两条命令和 `server.info` 的结果；本次可见证据中没有 API 密钥或 SSH 凭据。

Computer Use 也检查了当前设置页和运维 Agent 模型页：所选连接/模型显示为 `deepseek · deepseek-flash`，
Agent 页面说明它与启动的 subagent 共用当前对话模型；没有显示旧的彩色角色队列或系统提示词编辑器。工具设置页
显示尚无 MCP 服务器配置；这不影响本次经已连接 SSH 工具完成的只读循环，也不作为 MCP 端点验收。

随后关闭并重新打开桌面宿主。在“目标任务”页看到两份任务记录，分别打开详情核对：两项任务均仍为
“等待用户选择”，各自的计划修订 1 均保留六个只读步骤，六步全部为 `pending`，计划未获批准，也没有
活动运行。由此验证了两份持久任务与计划在桌面重开后的恢复；本次没有批准或启动任何计划步骤。

一次从普通历史对话发出的计划保存尝试没有关联 durable task，宿主以 `durable_task_required` 拒绝，
没有改变任务或计划状态。任务看板中的计划在关闭前已存在，重开后仍保持原样；该拒绝是预期的
fail-closed 结果，不作为计划已由本轮 Python Agent 成功创建的证据。

本机随后重新运行完整 `pnpm check` 并通过：发布卫生与秘密扫描通过，文档链接 58 个文件/299 个链接通过，
C++ CTest 17/17、control plane 157 项、Python Agent 345 项、桌面 640 项均通过；生产 UI bundle 与
Python Agent sidecar smoke 也通过。Python 3.14 在部分测试清理时打印非致命 `ResourceWarning`，未造成门禁失败；
针对 LangGraph 循环与会话持久化的单测复跑未重现该警告。

### 本轮追加 Computer Use 验收

- 在 Yukinal UI 中创建隔离的单步只读任务，计划仅含 `server.info`。工具调用返回成功，UI 显示步骤由宿主结算成功；任务列表刷新后调用记录仍在。该专用任务保持待用户选择，未批准或启动其他计划步骤。
- 另建仅用于审批超时观察的任务，自动执行保持关闭。`server.exec` 的 `true` 调用进入待审批后无人批准；等待 120 秒后 UI 显示“审批已过期”，并记录审批发起时尚未执行。命令没有运行，也没有重试。
- 原生界面检查了模型与 subagent 共用当前模型的说明、设置草稿跨分区保留、诊断日志保持收起、窗口最大化/还原，以及生成过程中手动上滚后未被拉回最新位置。禁发 Vite fixture 中另查看了合成资源趋势、历史表格和代码；发现并修复旧“绿队”参与者标签，样例现在显示“运维 Agent”，fixture 回归与浏览器可访问树均通过。修复后桌面 640 项测试、类型检查、`pnpm check:ai` 和全量 `pnpm check` 均通过；模拟预览不作为真实服务器数据或所有结构化回复组件的验收证据。
- 为复验新增结算步骤的重启恢复，使用窗口关闭控件后等待超过 60 秒：Computer Use 的 `list_windows()` 始终没有返回 Yukinal 可操作窗口，而 `list_apps()` 仍报告应用运行且保留一个窗口条目；按应用标识启动也返回“未暴露可操作窗口”。没有用进程强杀、终端命令或服务器配置改动绕过该状态。此前两份既有任务/计划已由更早的一次桌面重开测试确认恢复，但不能据此宣称本次新增步骤也已完成重启恢复。
- 2026-10-28 复验时用项目自带的 `pnpm desktop:dev:cpp` 启动开发 Host；裸 Host 可执行文件因缺少 `--dev-url`/`--ui-dir` 不会创建窗口。Computer Use 重新进入服务器范围后，目标任务仍为“等待选择”，显示 1 次 `completed` 运行；修订 1 的唯一 `server.info` 计划步骤为 `succeeded`、尝试 `1/1`，宿主验证条件仍显示成功。全局范围下历史列表为 0，切到服务器范围后恢复 11 条会话；这是范围筛选，不是数据丢失。此复验没有再次启动计划或读取主机输出。该运行是本机 C++ **开发构建**，不代表安装包 GUI 通过。
- 2026-10-28 结构化回复与范围隔离：在原生开发窗口用真实 Provider 请求一条只含虚构数据的回复，五种 widget 均已渲染；指标趋势、进度值可访问，详情展开后显示完整正文。复制按钮反馈成功，粘贴到空白 composer 后确认卡片正文、步骤状态、指标值与趋势、进度及详情都在，测试结束后清空草稿。回到最近对话重开后，widget 正常显示，未观察到重复入场动画。
- 2026-10-28 Python Agent durable task 再启动恢复：Computer Use 在“目标任务”中选中 Python Agent 验收任务，确认任务处于等待用户选择、计划修订待选择且四个步骤均为 `pending`；详情计数为 1 次计划创建运行、0 个计划步骤、0 条证据/发现/选项。关闭并重新启动 C++ 开发宿主后，同一任务和计划状态恢复。测试停在等待选择，没有批准计划或执行任何服务器步骤。单独从普通聊天发出的 `investigation.plan` 请求因未绑定 durable task 而被 `durable_task_required` 拒绝；该路径没有触发服务器工具，也不作为成功创建计划的证据。
- 2026-10-28 denylist 与桌面 autoconfig 集成：新增 host-only `YUKINAL_SSH_SERVER_EXEC_DENYLIST_CONFIG`，策略文件只含 `server_exec_denylist`，不替换 `YUKINAL_SSH_SERVICE_AUTOCONFIG` 或桌面生成的目标配置。通过 Computer Use 重启 C++ 开发宿主后，已连接目标继续可用；真实 `server.info` 成功。随后请求一次 `server.exec true`，并对该精确 no-op 调用批准一次以到达 host 执行门禁；UI 返回 `denied_by_policy`，宿主没有把命令发往 SSH。模型在拒绝后又发起了 `system.time` 与 `ssh.disk_usage` 两个额外只读工具调用，虽与该请求中的“只调用一次”约束不符，但没有执行写操作；因此本次结果只证明 denylist 执行门禁与 autoconfig 组合，不作为自然语言单工具约束的证明。测试结束后关闭策略宿主并移除临时策略文件。
- 首次发这条合成请求时，点过“切换到全局”但运行仍自动回退到唯一已连接服务器，并把残留的 `selectedTaskId` 附到旧目标任务会话。该轮只生成合成文本/UI 回复，界面没有显示工具事件，计划步骤没有启动。已修复：工作区显式记录 `auto/global` 范围，显式全局抑制唯一服务器回退并解绑所选 durable task；选择服务器或任务重新启用自动范围。热更新后从新会话显式切回全局再次发送，UI 保持“全局工作区”，会话为独立新对话，未见工具事件。`apps/desktop/tests/ui-logic.test.ts` 覆盖显式全局与自动唯一服务器两条路由；全套桌面回归覆盖流式/无效 widget 回退和历史入场状态。
- 本次完整 `pnpm check` 最终为 green：显式配置的 CTest 14/14、control plane 157 项、Python Agent 345 项、桌面 641 项、生产 UI bundle 与 Python sidecar smoke 均通过。桌面类型检查通过，文档链接 58 个 Markdown/299 条链接通过。Python 3.14 测试输出有非致命 asyncio/SQLite 资源警告，未导致失败。

## 未能验证

仍未覆盖：真实多目标并行、压力或长时间稳定性，以及本次源码对应的安装包 GUI。当前只配置一个可用真实目标，模拟预览不能替代多目标服务器证据。黑名单 host 配置与桌面 autoconfig 已在 C++ 开发宿主中联合验收；此项不等于多目标或安装包验收。
原生结构化回复检查使用的是 C++ 桌面开发构建，不替代安装包或其他平台 GUI 验收。

### 后续确定性回归补充

Python Agent 的成功工具结果现已调用 `host.investigation.plan.step_result` 推进持久计划；
同一 call ID 的结算重放不会重复推进，宿主返回拒绝时 Agent 会停止而不重跑已成功工具。
该路径已加入 `pnpm check:ai`，C++ 宿主数据库回归也覆盖计划推进、完成和重放。当前本地确定性
门禁通过；Computer Use 已在桌面重开后复核单步 `server.info` 结果仍为成功、尝试 `1/1`。
