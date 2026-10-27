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

## 未能验证

真实模型、真实 SSH 与界面显示已在本轮覆盖；**未覆盖**：审批超时（120 s 未等待）、
命令黑名单（需 `YUKINAL_SSH_SERVICE_CONFIG`，会顶掉桌面 autoconfig 传输路径）、
持久任务/计划（`investigation.plan`）链路、并行只读之外的多目标场景、以及任何
压力或长时间稳定性。这些仍按 `docs/ai-acceptance.md` 单独记录。
