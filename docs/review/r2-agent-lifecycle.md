# R2-E：Python Agent 运行生命周期深度审查

## 1. 范围、方法、与 R1/R3 的关系

**范围**（只读，未修改任何被审查文件）：

- 生命周期主干：`agent/src/yukinal_agent/sidecar.py`（`handle` / `handle_async` / `_run_start` / `_admit_run` / `_execute_run` / `_execute_session_run` / `_await_approval` / `_run_stop` / `_context_reset*` / `serve_stdio` / `shutdown`）
- 图执行与预算：`agent/src/yukinal_agent/model_agent.py`
- 工具执行与审计：`agent/src/yukinal_agent/tools.py`、`agent/src/yukinal_agent/audit.py`
- 持久化：`agent/src/yukinal_agent/conversation_store.py`、`agent/src/yukinal_agent/checkpoints.py`、`agent/src/yukinal_agent/filesystem_action_ledger.py`
- 宿主 RPC 与子进程：`agent/src/yukinal_agent/host_rpc.py`、`agent/src/yukinal_agent/ssh_service_process.py`、`agent/src/yukinal_agent/mcp_manager.py`
- 消费侧（只用于判定 Python 侧结论是否**用户可见**）：`native/desktop_host/src/ipc_router.cpp`、`native/desktop_host/src/sidecar.cpp`、`apps/desktop/src/features/agent/{useAgentRun.ts,run-lifecycle.ts,AgentPanel.tsx,AgentComposer.tsx}`、`apps/desktop/src/stores/preferences-store.ts`

**方法**：

1. **逐分支穷举**：对本轮的核心问题（「哪些分支会让 run 永远停在 running」）不做举例式说明，而是把 `_run_start`（`sidecar.py:594-646`）→ `_admit_run`（`:648-706`）→ `_execute_run`（`:867-887`）→ `_execute_session_run`（`:889-1004`）的每一个 `return` / `raise` / `except` / `finally` 都单独判定「是否有终态事件到达 UI」。
2. **实测优先**：本轮新增 6 个探针脚本（`r2_gaps.py`、`r2_terminal_loss.py`、`r2_concurrency.py`，以及上一段已跑的 `r2_prestep_cancel.py`、`r2_sync_cancel.py`、`r2_stop_during_admission.py`、`r2_prologue_and_admission.py`），全部在 `%TEMP%` 下运行，使用 `PythonSidecar` + 真实 `ConversationStore` + 桩模型，不依赖 host。
3. **基线**：`agent/.venv/Scripts/python.exe -m unittest discover -s agent/tests -p "test_*.py"` → `Ran 153 tests … OK (skipped=4)`（本轮未改动任何被测代码，基线不变）。
4. **置信度**：高 = 源码逐行确认 **且** 本地可复现；中 = 一侧实测、另一侧只能静态推断；低 = 需要特定环境/时序才能定性（本轮标为「结构推断」）。

**与 R1/R3 的关系（不重复已有关闭项）**：

- R1 是**广度**审查，对运行生命周期只给了点观测；R1 已登记的相关项只有 M-5（`host_rpc._cancel_host_request` 的取消帧是 fire-and-forget，`r1-python.md:595-620`）。本报告 **Q2 直接引用并深化它**，不重新发现。
- R3 已确认 python H-3 成立：`policyId` 在 `_parse_run_request`（`sidecar.py:1612-1615`）被无条件拒绝，UI 主动选策略即失败（`docs/review/r3-verification.md:585-628`）。本报告 **只在状态机的「早退分支」表里登记它一行**，并明确它**不是**永久挂起（失败发生在 run 创建之前）。
- 本报告编号前缀 `R2E-` 自成序列，与 R1 的 H-1/M-5、R3 的 H-3 不共用。

**总体结论**：Python Agent 的运行生命周期在**正常路径**上是完备的——每个正常终止都在 `_execute_session_run` 的 `finally`（`sidecar.py:988-1004`）里落 `finish` + `_completed_runs` + `admission.completed`，并对 `CancelledError`/`Exception` 各有一条终态事件（`:966-987`）。风险集中在**三处「终态写入器不可达」**的位置：`finally` 之外的前置区、协程首步之前的取消窗口、以及 `_emit` 自身抛错。三者都能让 run 在「进程存活、状态为 running、但永不推进」的状态下停住，而 UI 只有两条依赖**进程死亡**的兜底信号（`useAgentRun.ts:296-310`、`:318-335`），因此每一条都是用户可见的永久挂起。另有两个独立缺口：`_run_stop` 无法取消「正在 admission 中」的 run（`_active_runs` 尚未登记），以及「审计写入失败」会连带吞掉终态通知（`_emit` 把审计与通知绑在同一序列里，且异常向上传播到外层 `except` 再次 `_emit`、再次失败）。

**发现计数（本报告口径）**：**High 4 / Medium 4（共 8）**，其中 4 项是永久挂起。

| ID | 级别 | 一句话 | 位置 |
| --- | --- | --- | --- |
| **R2E-H-1** | High | 取消在协程首步之前到达时，`_execute_run` 的 `except CancelledError`/`finally` 都不会执行，run 永久 running（永久挂起） | `sidecar.py:696-697`、`:771`、`:877-887` |
| **R2E-H-2** | High | `_execute_session_run` 的前置区（`:894-909`）在 `try:` 之外，异常逃逸为「无人取回的 task 异常」，无终态事件、无 `finally` 清理（永久挂起） | `sidecar.py:894-909`、`:910`、`:696` |
| **R2E-H-3** | High | `_emit` 在写审计时抛错会吞掉终态事件；外层 `except Exception` 再 `_emit` 一次、再次抛错，于是「turn=succeeded 但 UI 从未收到 agent.completed」（永久挂起） | `sidecar.py:1015-1019`、`:948-954`、`:980-986`、`:977-987` |
| **R2E-H-4** | High | `_active_runs` 没有任何并发上限；跨会话可无限并发，而同会话的第二次 run 会在 `_admit_run` 内**阻塞 start RPC 且不可取消**（`_active_runs` 里还没有它） | `sidecar.py:129,697,995`、`:654`、`:771`、`sidecar.cpp:42` |
| **R2E-M-1** | Medium | admission 阶段（`_hydrate_conversation` 内，最长 8s host RPC）`_run_stop` 必然返回 `{"stopped": false}`，UI 抛「停止请求未被接受，请重试。」 | `sidecar.py:654`、`:768-770`、`useAgentRun.ts:440-441` |
| **R2E-M-2** | Medium | `delivery: "sync"` 让 `agent.run.start` 挂到 run 结束；配合 R2E-H-1 的取消窗口，start RPC 永不返回（host 侧超时 15 分钟） | `sidecar.py:699-701`、`ipc_router.cpp:9116-9118` |
| **R2E-M-3** | Medium | `_run_stop` 与 UI 的 `isActive` 竞态：run 已正常结束但 UI 尚未收到终态时点「停止」，得到「停止请求未被接受，请重试。」 | `sidecar.py:769`、`useAgentRun.ts:437-441` |
| **R2E-M-4** | Medium | `_cancel_host_request` 的取消帧仍是无主任务（深化 R1 M-5）：它既不在 `_active_runs`，也不在 `serve_stdio` 的 `tasks` 集合里，`shutdown()` 不会等它 | `host_rpc.py:87-103`、`sidecar.py:1108-1113`、`:1910,1963-1966` |

### 1.1 六问一句话判定

| 问题 | 判定 | 置信度 |
| --- | --- | --- |
| **Q1 生命周期状态机** | 正常路径完备（14 个阶段、11 条终态映射），**但有 4 条分支能让 run 永久停在 running**：协程首步前的取消（H1）、前置区异常（H2）、前置区 `_emit` 异常（H2b）、终态 `_emit` 异常（H3）；17 条 `_admit_run` 之前的早退分支全部**不是**挂起（失败发生在 run 创建前） | **高** |
| **Q2 取消传播** | 进入 `try` 之后的取消能正确贯穿模型调用与工具执行（各层都先落审计/事件再 re-raise）；**但有 5 个缺口**：admission 窗口内不可取消（M-1）、首步前取消丢终态（H1）、host 取消帧无主（M-4）、`_run_stop` 与 UI `isActive` 竞态（M-3）、`to_thread` 使「已取消」不等于「副作用未发生」 | **高**（前四项）；**中**（`to_thread` 语义） |
| **Q3 并发** | **`_admit_run` 没有任何准入上限**——`_active_runs` 纯粹是记账，跨会话可无限并发（实测 2 个并发进入模型）；同会话靠 `_session_locks` 串行，代价是第二个 start RPC 在 `_admit_run` 里阻塞且**不可取消**（实测 0.4s 未返回、stop 返回 `stopped: false`） | **高** |
| **Q4 checkpoint/resume** | resume 一致性是**显式校验、失败即报错**：`_run_fingerprint`+`_run_checkpoint_binding`+`_checkpoint_matches` 三层比对覆盖 provider/model/policy/budget，换模型不会静默续跑；审计脱敏成立（只存 key/type）；**残余风险**是图 state 里存原始 tool arguments（`model_agent.py:514`）会进 checkpoint sqlite（0600） | **高**（校验）；**中**（原始参数落盘） |
| **Q5 超时与资源** | LLM（≤90s / ≤120s）、工具（≤30s）、审批（120s）、host RPC（默认 30s / 水合 8s / 工具 45s）、MCP（30s）、SSH 子进程都有超时且 `SshServiceProcess` 在 `_serve_runtime` 的 `finally` 里停；**未覆盖三处**：前置区无 `finally`（H2/H2b）、取消帧无超时无主（M-4）、`to_thread` 不可取消 | **高**（清单）；**中**（退出路径未端到端实跑） |
| **Q6 审计一致性** | 「审计了但没执行」**按设计存在**（`started` 行写在参数校验之前，靠 `error_code` 区分），「执行了但没审计终态」只在进程被杀时出现；**真正的问题是反向耦合**：审计写失败会吞掉终态通知（H3），且 `record_event`/`finish`/`update_messages` 在缺少 turn 行时**静默 no-op**，使「前置区失败」与「仍在运行」在磁盘上无法区分（都是 `running` + 0 事件） | **高** |

### 1.2 结论对 R3 的对齐

R3 已确认 python H-3（`policyId` 被无条件拒绝）**成立**。本报告的处理：只把它登记为早退分支 **E1**（`sidecar.py:1612-1615`），并明确 **它不是永久挂起**——`_parse_run_request` 在 `_run_start` 的第一步就抛 `-32602`，此时没有 `begin`、没有 `create_task`、没有 admission 置位，UI 侧 `useAgentRun.ts:423-429` 走 catch → `lifecycle.fail()` + `runState="failed"`。因此本报告的永久挂起清单**不包含** `policyId`，也不会与 R3 的结论重复计数。

---

## 2. Q1 运行生命周期状态机全图

### 2.1 正常路径（T1–T14）

| # | 阶段 | 位置 | 输入 → 输出 / 副作用 |
| --- | --- | --- | --- |
| T1 | 参数解析 | `sidecar.py:595` → `:1549-1708` | `params` → `request` dict；任何非法字段立即 `_RpcFault(-32602)`（含 `policyId`，`:1612-1615`） |
| T2 | 重置互斥 | `:596-597` | `_resetting_all` 或 `sessionId ∈ _resetting_sessions` → `-32602 "Wait for the context reset to finish…"` |
| T3 | 会话绑定 | `:598-601` | `bind_session` 抛 `ValueError` → `-32602`（会话属于别的 server 上下文） |
| T4 | 指纹与受理复用 | `:604-615` | `messageId` 命中 `_admissions`：内容不一致 → `-32602`；已 started → `{"started": false, "duplicate": true}`；未 started 且 `resume=false` → 同上；未 started 且 `resume=true` → 复用 `runId`、`resumed=True` |
| T5 | 首次受理（不执行） | `:617-628` | `resume=false` 且无 `messageId` → `-32602`；否则写入 `_admissions[messageId] = {…, "started": False, "completed": False}` 并返回 `{"runId", "started": false}` |
| T6 | 运行去重 | `:630-635` | `runId ∈ _run_sessions` → `-32602 "runId is already running"`；`runId ∈ _completed_runs` → 指纹不同 `-32602`，相同则 `{"started": false, "duplicate": true}` |
| T7 | 预占会话 | `:639-646` | `_run_sessions[runId] = sessionId`（**先预占再 await**，注释 `:637-638`）；`_admit_run` 返回后若尚未进 `_active_runs` 就回收预占 |
| T8 | 会话历史水合 | `:654` → `:708-713` | 取 `_session_locks[sessionId]`；`needs_hydration` 为真时发 `host.chat.context`（**超时 8.0s**，`:727`），失败 → `-32602/-32603` |
| T9 | 参与者校验 | `:655-658` | `validate_participant` 抛 `ValueError` → `-32602` |
| T10 | 模型装配 | `:659-677` | `_run_provider_model` → `with_reasoning_effort`（`ModelFactoryError` → `-32602`）→ `ModelDrivenAgent(...)`（构造失败 → `-32602 "configured model cannot run the registered tools"`） |
| T11 | 受理置位 + `begin` | `:679-695` | `admission["started"] = True`；`request["runId"] = run_id`；`_conversations.begin(request, fingerprint)` 写 `conversation_turns` 行（`conversation_store.py:318-337`），`ValueError` → `-32602` |
| T12 | 派发执行 | `:696-698` | `asyncio.create_task(self._execute_run(...))` → `_active_runs[runId] = task`；`_run_sessions[runId] = sessionId` |
| T13 | 响应 | `:699-706` | `delivery=="sync"` → `await task` 后返回 `{"runId","started":true,"result":…}`；否则返回 `{"runId","started":true}`；`resumed` 时补 `"resumed": true` |
| T14 | 执行体 | `:867-887` → `:889-1004` | 取会话锁 → 前置区（`:894-909`：读 context、读 memory、`record_event("context.loaded")`、`_emit agent.started`）→ `agent.run(...)`（`:914-932`）→ 审批循环（`:933-934`）→ 终态事件（`:944-964`）→ `finally`（`:988-1004`：`finish(runId, final_status)`、清 `_pending_approvals`、`_active_runs.pop`、`_completed_runs[runId]=fingerprint` 并 LRU 裁剪到 256、`admission["completed"]=True`） |

**终态事件映射**：

| 结果 | `final_status` | 事件 | 位置 |
| --- | --- | --- | --- |
| 成功 | `"succeeded"` | `agent.text` × N + `agent.completed(state="completed")` | `:945-954` |
| 业务失败 | `result.status`（如 `"failed"`/`"timed_out"`） | `agent.failed` | `:955-964` |
| 取消（进入 try 之后） | `"cancelled"` | `agent.completed(state="cancelled")` | `:966-976` |
| 异常（进入 try 之后） | 保持 `"failed"` | `agent.failed(error="agent_run_failed")` | `:977-987` |
| 取消（在拿到会话锁之前，即队列中） | —— | `finish("cancelled")` + `agent.completed` | `:877-882` |

### 2.2 早退分支全表（`_admit_run` 之前 / 之中），**均非永久挂起**

判定口径：这些分支要么在 run 创建之前失败（无 `begin`、无 `create_task`、无 admission 置位），要么在 `_run_start` 的 `finally`（`:642-646`）里回收了预占。**UI 侧不会进入 running**：`useAgentRun.ts:410-415` 的 resume 调用收到 `error` 会走 `catch` → `lifecycle.fail()` + `setRunState("failed")`（`:423-429`）。

| # | 分支 | 位置 | 返回 | 是否留下脏状态 |
| --- | --- | --- | --- | --- |
| E1 | 任意参数非法（含 `policyId`、`parts` 文本超 64KiB、`delivery` 非 async/sync 等） | `:595` → `:1549-1708` | `-32602` | 无（R3 H-3 即此列） |
| E2 | 正在 reset / resetAll | `:596-597` | `-32602` | 无 |
| E3 | 会话属于别的 server 上下文 | `:598-601` | `-32602` | 无 |
| E4 | `messageId` 已受理但内容不同 | `:608-609` | `-32602` | 无（旧 admission 保留） |
| E5 | 同一消息重复调用 | `:610-613` | `{"started":false,"duplicate":true}` | 无；UI 显示「已由另一个调用启动」 |
| E6 | `resume=false` 无 `messageId` | `:617-619` | `-32602` | 无 |
| E7 | `resume=false` 正常受理 | `:620-628` | `{"started":false}` | 写 `_admissions`（**这是设计上的第一次调用**） |
| E8 | `runId is already running` | `:630-631` | `-32602` | 无 |
| E9 | `runId` 已用过且内容不同 | `:632-634` | `-32602` | 无 |
| E10 | `runId` 重复且内容相同 | `:632-635` | `{"started":false,"duplicate":true}` | 无 |
| E11 | 水合 host RPC 失败/超时（8s） | `:727-729` | `-32603 "Could not load this conversation's saved context; retry the run"` | `_run_sessions` 由 `:642-646` 回收 |
| E12 | 水合返回结构非法/超限 | `:730-737` | `-32603` | 同上 |
| E13 | 水合消息字段非法 | `:738-760` | `-32603` | 同上 |
| E14 | 参与者不与既有会话自洽 | `:655-658` | `-32602` | 同上 |
| E15 | 模型装配失败（provider/model 不可用） | `:659-666` | `-32602` | 同上 |
| E16 | `ModelDrivenAgent` 构造失败（模型不能跑已注册工具） | `:667-677` | `-32602 "configured model cannot run the registered tools"` | 同上；**注意 `:680` 尚未置位 `admission["started"]`**，所以重试仍可成功 |
| E17 | `_conversations.begin` `ValueError`（runId 属于别的 turn / 早前 context 代） | `:692-695` | `-32602` | 同上；但 `:679-689` 已置 `admission["started"]=True`，**重试会得到 `duplicate:true` 而永远无法执行**——见 5.4（已实测） |

> **重要更正**：E17 是本轮新发现的边界：`admission["started"]` 在 `:680` 置位，而 `begin` 在 `:693`。若 `begin` 因任何 `ValueError` 失败，`_run_start` 的 `finally` 只回收 `_run_sessions`，**不会回滚 admission**，于是同一 `messageId` 的后续重试（无论 `resume=false` 还是 `resume=true`）都命中 `:610-611` 直接返回 `duplicate:true`，用户无法通过「再试一次」恢复，必须换 `messageId`。UI 每次 `start` 都会 `newId("msg")` 生成新 id（`useAgentRun.ts:379`），所以 UI 层可绕过；但 sidecar 的幂等重试语义已被破坏，且这条记录**永远无法被执行**。实测见第 8 节。

### 2.3 永久挂起分支穷举（**本报告唯一不可替代的产出**）

判定标准：**是否有任何一个终态事件（`agent.completed` / `agent.failed`）到达 `notification_sink`**。UI 的 `running` 只在 `agent.completed`（`useAgentRun.ts:264-273`）、`agent.failed`（`:274-280`）、sidecar 退出（`:296-310`）、restart 记录（`:318-335`）四处被清零；后两者都以**进程不再存活**为前提。因此「进程存活 + run 不再推进」= 永久挂起。

| # | 分支 | 触发条件 | 为什么终态不可达 | 实测 | 置信度 |
| --- | --- | --- | --- | --- | --- |
| **H1** | 取消在协程首步之前到达 | `agent.run.stop` 在 `_admit_run` 已经把 task 放进 `_active_runs`、但事件循环尚未执行 `_execute_run` 的第一条语句时到达 | `task.cancel()` 把一个「尚未开始」的协程标记为 cancelled：协程体**从不运行**，因此 `_execute_run` 的 `try`（`:874`）、`except CancelledError`（`:877-882`）、`finally`（`:883-887`）以及 `_execute_session_run` 的 `finally`（`:988-1004`）**全部不执行**。`_completed_runs` / `admission.completed` 永远不被写 | `r2_prestep_cancel.py`：`task cancelled: True`、`events: []`、`_active_runs: ['run-1']`、`_run_sessions: ['run-1']`、`admission.completed: False`、`turn status: running`、`second stop: {'stopped': False}`、`context.reset → -32602 "Stop the active session run…"`、`context.resetAll → -32602 "Stop all active runs…"`、重试 → `duplicate: True`。只有重启 sidecar 进程可清 | **高** |
| **H2** | `_execute_session_run` 前置区抛异常 | `:896-901` 的任何调用（`context()`、`memory_context()`、`message_bytes`）或 `:902` 的 `record_event` 抛错 | 前置区在 `try:`（`:910`）**之外**，异常先穿出 `_execute_session_run`，再穿过 `_execute_run` 的 `try`（该 try 只捕获 `CancelledError`，`:877`）。**注意与 H1 的关键差别**：`_execute_run` 的 `finally`（`:883-887`）**仍然会执行**（所以 `_active_runs`/`_run_sessions` 被清空），但唯一写 `_completed_runs` 与 `admission["completed"]` 的位置是 `_execute_session_run` 的 `finally`（`:988-1004`），它**不执行**。异常随后逃出 task，而 `asyncio.create_task`（`:696`）没有保留任何强引用，于是成为「Task exception was never retrieved」——**没有任何人观察它**，也就没有任何机会补发终态事件 | `r2_prologue_and_admission.py`（B）与 `r2_gaps.py`（C）：resume 调用返回 `{"started": true, "resumed": true}`（UI 据此 `acknowledge` 并进入 running），随后 stderr 打出 `Task exception was never retrieved … RuntimeError('simulated store failure')`，`events: []`、`_active_runs: []`、`_run_sessions: []`、`_completed_runs: {}`、`admission.completed: False`、`turn status: running`、`stop → {'stopped': False}`、重试 → `duplicate: True`。**注意 `_active_runs` 此时是空的**，所以连「停止」这个入口都不存在（H1 则相反：`_active_runs` 残留，能查不能清） | **高** |
| **H2b** | 前置区的 `_emit`（`agent.started`）抛错 | `:904-909` 的 `_emit` 因审计写入失败（磁盘满 / sqlite 锁 / 只读卷）抛错 | 同 H2：`_emit` 在 try 之外 | `r2_terminal_loss.py`（E）：`notified events: []`、`turn status: running`、`_active_runs: []`、`_completed_runs: []`、`admission.completed: False`；`context.reset: {'reset': True}`（因为 `_run_sessions` 已被 `_execute_run` 的 finally 清空）；重试 → `duplicate: True` | **高** |
| **H3** | 终态 `_emit` 抛错 | `:948-954`（`agent.completed`）或 `:958-964`（`agent.failed`）或 `:969-975`（取消分支）的 `_emit` 抛错 | `_emit`（`:1015-1019`）先 `record_event` 再 `notification_sink`。审计抛错 → 通知根本不发；异常被同层的 `except Exception`（`:977-987`）捕获后又调用 `_emit(agent.failed)`，**同一个审计失败再次抛错**，这次直接逃出 → `finally` 仍执行（所以磁盘状态是「干净的」）但 UI 永远等不到终态 | `r2_terminal_loss.py`（D）：`notified events: ['agent.started', 'agent.text']`（**没有 completed**），而 `turn status: succeeded`、`admission.completed: True`、`_completed_runs: ['run-d']`。即「审计说它成功了，UI 认为它还在跑」；`stop → {'stopped': False}`、重试 → `duplicate: True`。只能靠重启进程 | **高** |
| **H4** | `delivery: "sync"` + 上述任一情形 | UI 偏好 `agentDelivery="sync"`（`preferences-store.ts:43`，用户可切换）时，`useAgentRun.ts:411` 发 `delivery:"sync"` | `_admit_run:699-701` 在 sync 路径上 `await task`，所以 **`agent.run.start` 的响应帧本身就是终态信号**。任务被取消而未产生返回值时，`await task` 抛 `CancelledError`，`handle_async` 的 `except Exception`（`:312-313`）**不捕获 `CancelledError`（它是 `BaseException`）**，于是 `respond` 任务被取消，**没有任何响应帧写出**；host 侧只能等到 `ipc_router.cpp:9116-9118` 的 15 分钟超时 | `r2_gaps.py`（B）：`registered: True`、`stop: {'stopped': True}`、`driver: CancelledError propagated (no response frame)`、`events: []`、`_active_runs: ['run-b']`、`turn status: running`、`admission.completed: False`。对照 `r2_sync_cancel.py` 的竞速变体：`start task done: False`（请求永不返回） | **高** |

**归纳**：H1/H2/H2b/H3 的共性是**「终态写入器不在异常覆盖范围内」**——`_execute_session_run` 的 `finally` 覆盖了 `:910` 之后的一切，但漏掉了 `:894-909`；`_execute_run` 的 `finally` 只清 `_active_runs`（协程体至少开始执行时才行），而 `_completed_runs`/`admission.completed` 的唯一写入点在 `:996-1004`。H4 是把 H1 的后果放大到「请求-响应」层面。

**UI 侧的可达性（为什么这四条都是用户可见的永久挂起）**：H1/H2/H2b/H3 下 `agent.run.start` 都**成功返回** `{"started": true, "resumed": true}`（实测转录见第 8 节），而 `setRunning(true)` 早在 `useAgentRun.ts:358`（`lifecycle.begin` 成功之后）就执行了。终态处理各自如下：

- **H1 / H2 / H2b**（`agent.started` 从未发出，实测 `events: []`）：`RunLifecycle.started()` 不被调用，`lifecycle.runId` 仍为 `null`，于是 `useAgentRun.ts:418` 的 `acknowledge(started)` 命中 `newlyStarted = this.running && this.runId === null`（`run-lifecycle.ts:25`）为真 → `setRunId(started); setRunState("thinking")`，UI 显示「正在思考」。
- **H3**（`agent.started`/`agent.text` 已收到）：`agent.started` handler（`:199-204`）已 `setRunning(true); setRunState("thinking")`，`acknowledge` 返回 `false` 但状态已经是 running。

此后：

- 没有任何 `agent.completed` / `agent.failed` 到达 ⇒ `setRunning(false)`（`:267`、`:277`）永不执行；
- `sidecarRunning` 仍为 `true`、也没有新的 restart 记录 ⇒ 兜底 effect（`:296-310`、`:318-335`）永不触发；
- 用户重新发送：`lifecycle.begin` 返回 `false`（`run-lifecycle.ts:9`，`running` 已 latch）⇒ `start` 立刻返回 `"busy"`；
- 用户点停止：H1/H4 下 `_run_stop` 返回 `{"stopped": false}`（`:769`，task 已 `done()`，或压根没登记），`isActive` 为真 ⇒ 抛「停止请求未被接受，请重试。」（`:440-441`）；H2/H2b/H3 下同理（`_active_runs` 已空）；
- 唯一的恢复手段是**重启 sidecar 进程**，让 `sidecarRunning` 或 restart 记录变化，触发那两条兜底 effect。

注意 `loadTranscript`/`clearTranscript`（`:486-502`）只清 `runId`/`entries`，**不清 `running`**，所以切换会话也不能解除这个状态。

**明确列入「非挂起」以便不重复计数**：2.2 表中 E1–E17 全部在 run 创建前失败或由 `:642-646` 回收，UI 走 `catch` 路径。特别地，R3 确认的 `policyId`（E1）只是「选策略必然启动失败」，**不是**挂起。

---

## 3. Q2 取消传播（`agent.run.stop` + host cancel frame → LLM 流 + 工具执行）

### 3.1 取消链路

| 段 | 位置 | 行为 |
| --- | --- | --- |
| UI | `useAgentRun.ts:432-448` | `agentRunStop({runId})`；`stopped && lifecycle.finish(runId)` → `cancelled`；`!stopped && isActive` → 抛「停止请求未被接受，请重试。」 |
| host 命令表 | `ipc_router.cpp:9079` | `agent_run_stop` → `agent.run.stop`，非 `agent_run_start` 所以超时 **30 秒**（`:9116-9118`） |
| host 直接调用点 | `ipc_router.cpp:6598`、`:6697` | investigation task 的 stop 是**best-effort**（`(void)…request(..., 15s)` 且 `try{}catch(...){}` 吞掉），与 UI 的 stop 不是同一条路 |
| sidecar | `sidecar.py:763-772` | 只查 `self._active_runs.get(run_id)`；`None` 或 `task.done()` → `{"stopped": false}`；否则 `task.cancel()` → `{"stopped": true}` |
| 传播 | `_execute_run:875` 的 `async with lock` / `agent.run` 内 `asyncio.timeout` / `_call_model:460` `asyncio.wait_for` / `tools.execute:322` `asyncio.wait_for` | `CancelledError` 沿 await 链向上传播；`tools.execute:338-343` 先落一条 `status="cancelled"` 审计再 re-raise；`model_agent._execute_tool:619-628` 也先发 `agent.tool_result(status="cancelled")` 再 re-raise |
| host RPC 取消帧 | `host_rpc.py:35-38`、`:87-103` | `CancelledError` → `_pending.pop` + `_cancel_host_request(request_id)` → `asyncio.create_task(send_cancel())` 发 `host.tool.cancel` |
| host 侧 | `sidecar.cpp:420-440` | `cancel_host_request` 要求正数 `requestId`，否则 `-32602` |

### 3.2 五个缺口（(a)(b) 为取消传播，(c)(d)(e) 为边界与竞态）

**(a) admission 期间的 stop 必然丢失（R2E-M-1，中）**

`_active_runs[run_id]` 在 `:697` 才登记，而 `_admit_run` 在 `:654` 会先 await `_hydrate_conversation`（最长 8 秒的 `host.chat.context`）。整个窗口内 `_run_stop` 命中 `task is None` 分支返回 `{"stopped": false}`，UI 把它映射为错误提示「停止请求未被接受，请重试。」（`useAgentRun.ts:440-441`），而 run 随后**照常执行**。

实测（`r2_stop_during_admission.py`）：

```
inside admission -> _active_runs: [] _run_sessions: ['run-c']
stop during admission: {'stopped': False}
second stop: {'stopped': False}
execute-call response: {'runId': 'run-c', 'started': True, 'resumed': True}
model.entered: 1
events at end: ['agent.started','agent.text','agent.completed']   turn status at end: succeeded
```

注意 `_run_sessions` 已被预占（`:639`），所以 `context.reset`/`resetAll` 在此期间也会被拒绝——用户既停不掉、也重置不了。

**(b) 首步之前的取消窗口（R2E-H-1，高）**

见 2.3 H1。窗口大小 = 「`_active_runs[run_id] = task`（`:697`）之后、事件循环轮转到 `_execute_run` 的第一条语句之前」。对 async delivery，`_admit_run` 在 `:703` 立即返回，把这个 task 交给循环；UI 的 stop 是另一条 RPC，必然晚于这个窗口——**所以 H1 在 async 路径上需要「host/客户端在极短时间内连续发 start 与 stop」**。但对 **sync delivery**，UI 的 start 请求本身还在等待，用户按停止是完全自然的操作，H1 会稳定复现（`r2_gaps.py` B）。

**(c) host 取消帧无人持有（R2E-M-4，中；深化 R1 M-5）**

R1 已登记 `host_rpc.py:103` 的 `asyncio.create_task(send_cancel())`（`r1-python.md:595-620`）。本轮补充两点 R1 未写的**可观测后果**：

1. 该 task **不在** `serve_stdio` 的 `tasks` 集合里（`:1910` 定义、`:1933-1935` 只给 `notify` 登记、`:1960-1962` 只给 `respond` 登记），所以进程退出前 `:1965-1966` 的 `await asyncio.gather(*tasks, …)` **不会等它**；`sidecar.shutdown()`（`:1108-1113`）也只 cancel `_active_runs`。
2. 更关键的是**语义**：`host.tool.cancel` 只是通知 host「这个 requestId 我不等了」。即使取消帧成功送达，host 侧的被取消工具仍可能在远端继续执行（`ssh_tools.py` 大量 `asyncio.to_thread` 调用 —— 例如 `:1296 edit_remote_file`、`:1464 create_remote_backup`、`:1726 restore_remote_file`、`:2042 remove_verified_backup` —— `to_thread` 无法被取消，线程会跑完）。也就是说：**「UI 显示已取消」不等于「副作用没有发生」**。对写/破坏性工具，这是取消语义层面的真实缺口，而不是帧投递层面的。

**(d) `_run_stop` 与 UI `isActive` 的竞态（R2E-M-3，中）**

`_run_stop:769` 把「task 已完成」也归入 `{"stopped": false}`。终态事件与 task 完成之间有真实窗口：`_execute_session_run` 先把终态事件交给 `_emit`（`:948-954`），再进 `finally`（`:988-1004`）—— 其中 `:995` 才 `_active_runs.pop`。但**通知的实际投递是异步的**：`serve_stdio` 的 `notification_sink`（`:1932-1935`）是 `create_task(write_frame(...))`，投递到 UI 需要经过 host 转发。因此存在这样的顺序：

1. sidecar 端 task 已经跑完并 `_active_runs.pop`；
2. UI 尚未收到 `agent.completed`，所以 `lifecycle.current.running` 仍为 `true`、`isActive(runId)` 为 `true`；
3. 用户点「停止」（按钮在前两处状态下都是可点的：`AgentPanel.tsx:502` `canStop={Boolean(run.runId) && !run.stopping}`，`AgentComposer.tsx:715`）；
4. `_run_stop` 返回 `{"stopped": false}`（`:769`），UI 在 `:440-441` 抛「停止请求未被接受，请重试。」并把它作为一条 error entry 显示（`:444`）。

结果：**一次已经成功结束的运行，因为用户点了停止，界面上多出一条红色错误**（终态事件随后到达并正常收尾，`setRunState(event.result.state)` 会覆盖 `runState`，但那条 error entry 留在 `entries` 里）。这是纯 UI 噪声而非状态机缺陷，因此定为 Medium 的体验级问题，但它同样源于 `{"stopped": false}` 这个返回值**承载了两种完全不同的语义**（「没有这个 run」与「这个 run 已经结束了」）。修复建议：把返回值扩展为 `{"stopped": false, "reason": "not_found" | "already_finished"}`，UI 只在 `not_found` 时报错。

**(e) 锁协议把取消窗口前移到 admission（结构推断，中）**

`_hydrate_conversation`（`:711-712`）**无条件**获取 session 锁，即使 `needs_hydration` 为假（`_hydrate_session_history:718-719` 会立刻返回，但锁已经拿了）。因此同会话的第二个 run 会在 `_admit_run:654` 就阻塞——**此时 `create_task`（`:696`）还没执行**，`_active_runs` 里没有它，`_run_stop` 必然失败。这解释了 4.1 的实测（`(F)` 中 `second start done: False` 且 `_active_runs` 只有 `run-1`），也说明 `_execute_run:877-882` 那条「排队的 turn 在拿到会话锁之前被取消」的分支（注释 `:878`）在当前锁协议下**几乎不可达**：要走到 `:875` 时锁仍被占用，需要另一个 run 在第二个 run 的水合释放锁之后抢到锁，而那个 run 自己也会在水合处阻塞。

推论（若该分支真的被走到）：`:877-882` 只调用 `finish` + `_emit`，**不写 `_completed_runs`，也不置 `admission["completed"]`**（那是 `:996-1004` 的职责，而 `_execute_session_run` 根本没进入）。于是该 `runId` 的幂等去重失效、admission 永久停在 `completed: False`，而 `_prune_admissions`（`:1006-1013`）只删除 `completed` 的条目、否则直接 `return`，因此这类条目**永远不会被淘汰**。影响有限（需要 256 个以上未完成条目才会越过上限继续增长），但属于同一处的记账不完整。置信度：**中**（源码头尾确认；因窗口近乎不可达而未实测）。

---

## 4. Q3 并发（结构推断 + 实测）

### 4.1 重复 start、同会话并发、全局并发

| 问题 | 结论 | 证据 |
| --- | --- | --- |
| 同一 `runId` 重复 start | 被 `:630-631`（运行中）与 `:632-635`（已完成）挡住；同内容重复返回 `duplicate: true` | 源码 + `r2_prestep_cancel.py` 的 `retry start: {'started': False, 'duplicate': True}` |
| 同一 `messageId` 重复 start | 被 `:607-615` 挡住，`resume=false` 第二次返回 `duplicate: true` | `r2_gaps.py` 各探针均复用同一 `messageId` |
| 同一 `sessionId` 的两次 run | **第二次 start RPC 会在 `_admit_run` 里阻塞**：`_execute_run:873-876` 第一个 run 持有 `_session_locks[sessionId]` 并在内部 await 模型；第二个 run 的 `_hydrate_conversation:711-713` 也要同一把锁，于是第二个 start 请求**在响应帧写出之前**就卡住 | `r2_concurrency.py`（F）：`after 0.4s -> second start done: False`，`_run_sessions: ['run-1','run-2']`，`_active_runs: ['run-1']`，`admissions: {'msg-1': True, 'msg-2': False}`；`stop run-2 while blocked: {'stopped': False}`；释放模型后 `second start resolved after 0.41s: {'runId': 'run-2', 'started': True, 'resumed': True}` |
| 不同会话的两次 run | **完全并发，无全局上限** | `r2_concurrency.py`（G）：`model concurrent entries: 2`，`_active_runs: ['run-a','run-b']`，`_session_locks: ['sA','sB']` |

### 4.2 `_active_runs` 不是并发闸门（**结构性结论，高**）

`_active_runs` 的全部引用点：`:129`（初始化）、`:643`（`_run_start` 的 finally 判据）、`:697`（登记）、`:768`（`_run_stop` 查询）、`:885`（`_execute_run` 的 finally 清理）、`:995`（`_execute_session_run` 的 finally 清理）、`:1109`（`shutdown`）。**没有任何一处做「长度上限」判断**。

`_admissions` / `_completed_runs` 的 256 上限（`:998-999`、`:1006-1013`）是**按 `runId`/`messageId` 的幂等 LRU**，与并发无关。真正的串行化只有 `_session_locks`（每个 `sessionId` 一把）。因此：

- 跨会话并发 = 无界。每个 run 都持有自己的 `ModelDrivenAgent` + 图状态 + checkpoint thread。
- host 侧的承载能力是有限的：`kHostWorkerCount = 4`（`native/desktop_host/src/sidecar.cpp:42`，R2-A 已实测 4 个停驻请求可饱和 worker 池，见 `docs/review/r2-approval.md:16`）。**Python 侧不设界 ⇒ 并发 run 数 ≥5 时，host RPC（`host.chat.context`、`host.tool.execute`）会排队等待 worker**，而 `host.chat.context` 的超时是 8 秒（`sidecar.py:727`）—— 这会把「并发太多」转换成「水合超时 → `-32603 retry the run`」（E11），或者更糟：在 H2 窗口内造成挂起。

置信度：`_active_runs` 无上限 = **高**（源码穷举）；「≥5 并发会饱和 host worker」= **中**（R2-A 实测了 host 侧饱和，但未把两者串起来跑端到端）。

### 4.3 同会话阻塞的次生后果

第二个 start 在阻塞期间：
- 无法停止（`_run_stop` 查 `_active_runs` 查不到）——同 R2E-M-1；
- 无法 reset（`_run_sessions` 已含该 session，`:811-812` / `:837-838`）——`r2_concurrency.py` 未打印该项，但 E11/E12 路径已由 `r2_prestep_cancel.py` 的同型输出确认；
- 若 UI 走 sync delivery，这个阻塞就是 **15 分钟**（`ipc_router.cpp:9117`）级别的无响应（R2E-M-2）。

---

## 5. Q4 checkpoint / resume 一致性

### 5.1 指纹与绑定字段（逐字段清单）

| 机制 | 位置 | 覆盖字段（逐字核对 `:1802-1819` / `:1830-1839`） | 不覆盖 |
| --- | --- | --- | --- |
| `_run_fingerprint` | `sidecar.py:1801-1824` | `prompt`, `parts`, `requireCheckpoint`, `planBinding`, `sessionId`, `targetId`, `persona`, `participant`, `providerId`, `model`, `reasoningEffort`, `toolAccess`（仅当键存在）, `permissionMode`, `mode`, `policyId` | **`runId`**（runId 由调用方单独比较）、`taskId`, `taskBudget`, `focusServerId`, `workspaceId`, `agentBudget` |
| `_run_checkpoint_binding` | `:1827-1841` | `sessionId`, `taskId`, `taskBudget`, `prompt`, `parts`, `workspaceId`, `focusServerId`, `target`, `policyId`, `permissionMode`, `mode`, `planBinding`, `persona`, `participant`, `reasoningEffort`, `toolAccess` + `agentBudget`（序列化后的 `asdict`） | **provider 凭据**（`:1828-1829` 注释明确排除） |
| `ModelDrivenAgent._checkpoint_matches` | `model_agent.py:841-857` | `task_id`, `tenant_id`, `user_id`, `allowed_tools`, `allowed_target_ids`, `host_target`, `host_task_id`, `run_id`, `trace_id`, `checkpoint_binding`, `budget_binding` + 前两条消息 | —— |

### 5.2 `resume=true` 而 provider/model/policy 变了会怎样

- **provider / model / reasoningEffort 变了**：`_run_fingerprint` 变化 → 若走 `messageId` 复用，`_run_start:608-609` 直接 `-32602 "messageId was already admitted with different content"`；若用户换新 `messageId`，则进入 `_run_checkpoint_binding` 比较 → `_checkpoint_matches` 失败 → `model_agent.py:314-315` 返回 `checkpoint_context_mismatch`。**不会静默续跑**。
- **`policyId` 变了**：根本到不了 `_admit_run` —— `_parse_run_request:1612-1615` 对任何非空 `policyId` 直接 `-32602`（R3 已确认）。所以「resume 时换 policy」这条路径**在 Python Agent 上不存在**。
- **`taskBudget` 变了**：不在 `_run_fingerprint` 里，所以 `messageId` 复用**不会**报「content 不同」；但它在 `_run_checkpoint_binding` 里，并且独立成为 `budget_binding`，所以 `_checkpoint_matches` 会失败 → `checkpoint_context_mismatch`。**这是正确的**（预算变小后不能复用旧 checkpoint）。
- **`requireCheckpoint=true` 而 checkpoint 不存在**：`model_agent.py:320-321` → `checkpoint_missing`。

**结论（高）**：resume 一致性在 Python 侧是**显式校验、失败即报错**的，没有发现「换了模型却静默续用旧图状态」的路径。

### 5.3 密钥与 checkpoint / 审计

- checkpoint 绑定哈希**排除** provider 凭据（`:1827-1841` 注释与实现一致），所以凭据不会进入绑定字符串。
- **但图状态里有原始工具参数**：`model_agent.py:514` 把 `"arguments": call["args"]` **原样**写入 state（而 state 会被 checkpointer 持久化）。`_redact_arguments`（`:872-892`）只在**展示/事件**路径上生效（`:704`），不作用于 state。所以若模型把密钥塞进某个工具的 arguments，该明文会落在 `checkpoints.py` 建立的 sqlite 里（权限 0600，`checkpoints.py:26-47`）。
- 审计侧则相反：`AuditEvent` 只存 `argument_keys` / `argument_types`（`tools.py:494-500`、`:504-519`），**不会**泄漏参数值。

**结论（中）**：审计脱敏成立；checkpoint 里的原始 arguments 是「本地 0600 文件内的明文」，属于**已知设计取舍**而非越权，但 R1 未写、值得记录为残余风险。

### 5.4 admission 置位与 `begin` 的次序缺陷（中，**已实测**）

`:680` 先置 `admission["started"] = True`，`:693` 才 `begin`。若 `begin` 抛 `ValueError`（`:692-695`），admission 不回滚 → 同一 `messageId` 的重试永久返回 `duplicate: true`（`:610-611`）。UI 因每次 `start` 都新建 `messageId`（`useAgentRun.ts:379`）而**不会**卡住，但 sidecar 的幂等语义被破坏：它把一个**从未执行的 run** 记成了「已启动」，且该记录 `completed` 永远为 `False`，因此在 `_prune_admissions`（`:1006-1013`）的「只删 completed」策略下**无法被淘汰**。修复：把 `admission["started"] = True` 移到 `begin` 成功之后，或在 `:694` 的 `except` 里回滚。

实测（`r2_admission_stuck.py`，把 `ConversationStore.begin` 替换为抛 `ValueError` 的桩）：

```
first resume call: None {'code': -32602, 'message': 'runId belongs to an earlier context generation; start a new turn'}
  admission after failure: {'runId': 'run-x', 'started': True, 'completed': False, ...}
  _run_sessions: []   _active_runs: []
  turn row exists: 0                     # begin 失败 ⇒ 没有任何 conversation_turns 行
retry (resume=false) after begin is fixed: {'runId': 'run-x', 'started': False, 'duplicate': True}
retry (resume=true):                       {'runId': 'run-x', 'started': False, 'duplicate': True}
  model ran? []                          # 模型从未被调用
```

---

## 6. Q5 超时与资源清理

| 资源 | 超时 | 位置 | 清理 |
| --- | --- | --- | --- |
| 整轮 run | `max_total_seconds = max(0.01, min(120.0, maxRunMs/1000))`，`asyncio.timeout` 包住 `aget_state`+`ainvoke` | `sidecar.py:1796`、`model_agent.py:310-311` | 超时 → `task_timeout`/`timed_out` → `agent.failed` |
| 单次模型调用 | `min(90.0, max_total_seconds)` | `model_agent.py:460-461` | → `model_timeout` |
| 审批等待 | `approval_ttl_seconds`（默认 120，构造时校验 1..3600） | `sidecar.py:120-121`、`:1086` | `TimeoutError` → `agent.approval_expired` + `rejection_code="approval_timeout"`（`:1087-1090`） |
| 单个工具 | `min(max_tool_seconds=30.0, spec.timeout_ms/1000)` | `model_agent.py:606-609` → `tools.py:322` | `execution_timeout` 审计 + `ToolCallError` |
| 工具输出 | `max_output_bytes=200_000`（budget）与 `spec` 上限 | `tools.py:330-331` | `output_too_large` |
| host RPC 默认 | 30.0s | `host_rpc.py:24` | `HostRpcError("host request timed out")` + 发 cancel 帧 |
| `host.chat.context` | **8.0s** | `sidecar.py:727` | → `-32603 "Could not load this conversation's saved context; retry the run"` |
| `host.tool.execute` | 45.0s（`_execute_on_host` 默认） | `host_tools.py:1313-1319`、`:1362` | 由 host 返回错误 |
| 其它直连 host RPC | 18/15/15/20s | `host_tools.py` | 同上 |
| MCP 连接/发现 | 30s / 30s，httpx `Timeout(30, read=60)` | `mcp_manager.py` | `except BaseException` → `stack.aclose()` |
| SSH 子进程 | 健康检查 8.0s；stop → terminate → wait → kill | `ssh_service_process.py` | `_serve_runtime` 的 `finally` 停服务（`sidecar.py:1997-1999`）；`_healthy` 在 `finally` 关 writer（`:648-654`） |
| 审计落盘 | 无超时（`asyncio.to_thread` 阻塞线程） | `audit.py:55` | 无；失败向上抛 |
| 取消帧投递 | **无** | `host_rpc.py:103` | **无**（R2E-M-4） |

**Python 侧对应 `SshResources` 的机制**：`ssh_service_process.py` 的 `SshServiceProcess` 在 `_serve_runtime`（`sidecar.py:1990-1999`）里 start，并在 `finally` 中 stop —— 这是有覆盖的。`filesystem_action_ledger.py` 的 `_run_in_thread`（`:228`）也是 `to_thread`。

**未覆盖的三处**：

1. `_execute_session_run` 前置区无 `finally`（R2E-H-2/H2b）——连 `finish()` 都不会落。
2. `host_rpc._cancel_host_request` 的 task 无主（R2E-M-4）。
3. **`asyncio.to_thread` 不可取消**：`ssh_tools.py` 有 **22 处** `asyncio.to_thread`（`:79,121,359,370,383,390,398,406,409,416,431,440,450,835,842,849,858,1085,1296,1464,1726,2042`），另加 `filesystem_action_ledger.py:228`、`audit.py:55`。`await asyncio.to_thread(...)` 被取消时，**底层线程继续运行**，远端写操作可能已经生效。这是取消语义的真实缺口，见 3.2(c)。

**`_serve_runtime` / `main` 的退出路径**（置信度 中，未跑端到端）：`main():2030-2032` 只把 `SshServiceProcessError` 映射为 exit 2；`serve_stdio` 的收尾（`:1963-1966`）会 `await sidecar.shutdown()` → `host_rpc.close()` → `gather(tasks)`。由于 `tasks` 只含 `notify`/`respond` 两类（`:1933-1935`、`:1960-1962`），**取消帧任务不在其中**，所以进程可能在 cancel 帧写出前退出。另外 `serve_stdio` 的 `tasks` 里包含 `respond` 任务，而 `respond` 可能是一个**同步 delivery 的长跑 run**（`:699-701`），`shutdown()` 只 cancel `_active_runs`（`:1108-1113`）—— 即「stdin 关闭」这个正常退出路径会先取消 run，然后 `gather` 等 `respond` 任务返回；因为 `_execute_run` 会把 `CancelledError` **吞掉并返回 cancelled 结果**（`:877-882`），`respond` 能正常写出并结束。这条路径是自洽的，未发现泄漏。

---

## 7. Q6 审计一致性

### 7.1 「执行了但没审计」/「审计了但没执行」

| 情形 | 判定 | 位置 |
| --- | --- | --- |
| 审计 `"started"` 后所有正常终止 | **每个 started 都有配对的终态行**：超时 `execution_timeout`（`:332-337`）、取消 `execution_cancelled`（`:338-343`）、`ToolCallError`（`:344-374`）、成功（`:382-388`） | `tools.py:280-290` 之后的全部出口 |
| 参数非法 / 工具未注册 / 授权失败 | **「审计了但没执行」——按设计如此**：审计 `"started"`（`:280-290`）写在 `unknown_tool`（`:294-295`）、`invalid_arguments`（`:296-311`）、`authorization_failed`（`:313-315`）**之前** | 审计行 `status="started"` + 终态 `status="failed"`/`"denied"`，靠 `error_code` 区分 |
| 进程在 handler 内死亡 | **「执行了但没审计终态」**——`started` 行已落盘，终态行不会写 | `tools.py:322` 之后进程被 kill |
| sidecar 在 run 中被 kill | turn 的 `status` 停留在 `running`（`finish` 不执行） | `conversation_store.py:360-362` |
| **审计写失败导致终态事件丢失** | **「turn=succeeded 但无终态事件」**（R2E-H-3） | `sidecar.py:948-954` → `:1015-1019` |

### 7.2 顺序与回滚

- **审计先于执行**（`:280-290` 在 `:322` 之前）：这是正确的「先记账后动手」顺序，但代价是上面的「审计了但没执行」——**这不是缺陷**，因为 `error_code` 让两者可区分。
- **没有回滚**：`tools.py` 不提供补偿；`filesystem_action_ledger.py` 的 `claim`/`complete` 是**精确调用级的持久栅栏**，但它只覆盖 plan-bound host action，且失败时给的是 `action_outcome_unknown`（保守：承认「不知道结果」）而不是回滚。这是可接受的设计。
- **`_emit` 把审计与通知耦合成原子序列**（`:1015-1019`）：这是 R2E-H-3 的根因。正确做法是让 `record_event` 的失败**不阻断** `notification_sink`，或对通知路径用 try/except 包裹。

### 7.3 `record_event` / `finish` / `update_messages` 的静默 no-op（新，中）

- `record_event`（`conversation_store.py:352-358`）是 `INSERT … SELECT … WHERE EXISTS (SELECT 1 FROM conversation_turns WHERE run_id = ?)` —— 若 `conversation_turns` 无对应行，**静默不插**。
- `finish`（`:360-362`）是裸 `UPDATE` —— 无对应行时**静默不改**。
- `update_messages`（`:339-350`）同理。

正常路径下 `begin`（`:318-337`）已在 `_admit_run:693` 建行，所以三者都有效。但在 **H2/H2b 的前置区异常**路径里，`record_event("context.loaded")`（`:902`）本身就可能抛错，此时连 `begin` 建立的行也停在 `running` 且没有任何审计事件（实测 `events recorded in store: 0`）。这意味着**审计与 transcript 都无法事后区分「前置区失败」与「仍在运行」**——两者都是 `status='running'` + 0 事件。诊断性缺口，置信度 **高**。

---

## 8. 复现脚本与实测转录

所有脚本位于 `%TEMP%`，用 `D:\yukinal\agent\.venv\Scripts\python.exe` 从 `workdir D:\yukinal\agent` 运行，`PYTHONPATH=D:\yukinal\agent\src`。

**基线**（本轮未改任何源码）：

```pwsh
Set-Location D:\yukinal
$env:PYTHONPATH="D:\yukinal\agent\src"
& agent\.venv\Scripts\python.exe -m unittest discover -s agent/tests -p "test_*.py"
# Ran 153 tests in 23.740s — OK (skipped=4)
```

**H1 + M-2（首步前取消 / sync delivery）**：

```pwsh
& agent\.venv\Scripts\python.exe $env:TEMP\r2_prestep_cancel.py
& agent\.venv\Scripts\python.exe $env:TEMP\r2_gaps.py      # 探针 A 与 B
```

```
# r2_prestep_cancel.py
admit: {'runId': 'run-1', 'started': True, 'resumed': True}
task done: False model.entered: 0
stop response: {'stopped': True} None
task cancelled: True task done: True
events: []
_active_runs: ['run-1']  _run_sessions: ['run-1']
turn status: running
second stop: {'stopped': False}
context reset: -32602 'Stop the active session run before resetting its context'
context resetAll: -32602 'Stop all active runs before resetting model context'
retry start: {'runId': 'run-1', 'started': False, 'duplicate': True}
```

```
# r2_gaps.py — 探针 A（Python 语义确认）
trace: ['await-raised-CancelledError']
cancelled: True done: True          # 协程体一次都没进：trace 里没有 'body-entered'
```

```
# r2_gaps.py — 探针 B（sync delivery）
registered: True model.entered: 0
stop: {'stopped': True} None
driver: CancelledError propagated (no response frame)
model.entered: 0 events: []
_active_runs: ['run-b'] _run_sessions: ['run-b']
turn status: running
admission: {'started': True, 'completed': False, ...}
```

**H2 / H2b / H3（终态写入器不可达）**：

```pwsh
& agent\.venv\Scripts\python.exe $env:TEMP\r2_gaps.py                  # 探针 C
& agent\.venv\Scripts\python.exe $env:TEMP\r2_terminal_loss.py 2>$null # 探针 D 与 E
```

```
# r2_gaps.py — 探针 C：前置区异常
resume response: {'runId': 'run-c', 'started': True, 'resumed': True}
Task exception was never retrieved … RuntimeError('simulated store failure')
  at sidecar.py:896 (_execute_session_run, 在 try: 之外)
model.entered: 0 events: []
_active_runs: [] _run_sessions: [] _completed_runs: {}
turn status: running     admission.completed: False
stop: {'stopped': False}
events recorded in store: 0
retry: {'started': False, 'duplicate': True}
```

```
# r2_terminal_loss.py — 探针 E：前置区 record_event 抛错
notified events: []
turn status: running     admission completed: False
_completed_runs: []  _active_runs: []  _run_sessions: []
context.reset: {'reset': True}
retry: {'started': False, 'duplicate': True}
```

```
# r2_terminal_loss.py — 探针 D：终态 _emit 抛错
notified events: ['agent.started', 'agent.text']      # 没有 agent.completed
turn status: succeeded                                # 磁盘说成功
admission completed: True   _completed_runs: ['run-d']
_active_runs: []  _run_sessions: []
stop after lost terminal event: {'stopped': False}
retry: {'duplicate': True}
```

**M-1（admission 期间 stop 丢失）**：

```pwsh
& agent\.venv\Scripts\python.exe $env:TEMP\r2_stop_during_admission.py
```

```
inside admission -> _active_runs: [] _run_sessions: ['run-c']
stop during admission: {'stopped': False}
second stop: {'stopped': False}
execute-call response: {'runId': 'run-c', 'started': True, 'resumed': True}
model.entered: 1
events at end: ['agent.started','agent.text','agent.completed']
turn status at end: succeeded
```

**H4 / M-2（同步 delivery 语义）**：

```pwsh
& agent\.venv\Scripts\python.exe $env:TEMP\r2_sync_cancel.py
```

```
model.entered: True    in _active_runs: True
stop response: {'result': {'stopped': True}}
start task done: True … state: 'cancelled'   events: ['agent.started','agent.completed']
# 竞速变体：start task done: False（agent.run.start 永不返回）
```

**Q3（并发）**：

```pwsh
& agent\.venv\Scripts\python.exe $env:TEMP\r2_concurrency.py
```

```
# F：同会话
after 0.4s -> second start done: False | _run_sessions: ['run-1','run-2'] | _active_runs: ['run-1']
stop run-2 while blocked: {'stopped': False}
second start resolved after 0.41s: {'runId': 'run-2', 'started': True, 'resumed': True}
# G：跨会话
model concurrent entries: 2 ['prompt-run-a', 'prompt-run-b']
_active_runs: ['run-a', 'run-b'] _session_locks: ['sA', 'sB']
```

**5.4（admission 置位次序）**：

```pwsh
& agent\.venv\Scripts\python.exe $env:TEMP\r2_admission_stuck.py
```

```
first resume call: None {'code': -32602, 'message': 'runId belongs to an earlier context generation; start a new turn'}
  admission after failure: {'started': True, 'completed': False}
  _run_sessions: []   _active_runs: []
  turn row exists: 0
retry (resume=false): {'started': False, 'duplicate': True}
retry (resume=true):  {'started': False, 'duplicate': True}
  model ran? []
```

---

## 9. 修复建议（按优先级）

1. **把 `_execute_session_run` 的前置区移进 `try`**（对应 H2/H2b）。最小改动：把 `:896-909` 整体挪到 `try:` 之后，并让 `context_usage` 在异常路径有一个安全的初值。这一个改动同时消灭 H2 与 H2b。
2. **给 `_execute_run` 加「协程未开始」的补偿**（对应 H1）。可选方案：(a) 用 `asyncio.Task` 不需要的 `try/finally` 包住 `create_task` 与登记，把清理放在 `_run_start` 的 `finally` 里（`:642-646` 已有该 finally，只需补 `_completed_runs`/admission）；或 (b) 惯用法：让 `_execute_run` 的第一条语句是 `await asyncio.sleep(0)` 的替代——更稳的是 (a)，因为 (b) 只是缩小窗口。
3. **`_emit` 解耦**（对应 H3）。`record_event` 用 `try/except Exception` 包裹并只记 stderr，`notification_sink` 必须无条件调用。
4. **`_run_stop` 覆盖 admission 窗口**（对应 M-1）。登记一个「预占即登记」的占位（例如先 `_active_runs[run_id] = None` 或维护独立的 `_admitting` 集合），或者在 `_run_start` 的 `:639` 预占处同时记录「可取消」标志，由 `_run_stop` 在 admission 阶段设置一个 `cancel_requested` 标志、让 `_admit_run` 在 `create_task` 之前检查。
5. **为 `_active_runs` 加显式上限**（对应 4.2）。既然 host 只有 4 个 worker，Python 侧应有配额（例如 8），超限时 `-32602`/`-32603` 快速失败，而不是让 `host.chat.context` 8 秒超时。
6. **回滚 admission 置位**（对应 5.4）：把 `:680` 移到 `:693` 成功之后，或在 `:694` 的 `except` 里把 `admission["started"]` 复位。
7. **让取消帧有主**（对应 M-4）：把 `send_cancel` 的 task 交给调用方（`HostRpcClient` 持有一个 drain 集合，`close()` 时 `gather`）。

---

## 10. 与 R1/R3 的去重声明

- **不重复 R1**：R1 H-1（CTest glob 覆盖 100/153）、H-2（CI 不建 control-plane venv）、H-3（`policyId`）、M-1…M-4、L-1…L-5、20 项已排查、10 项建议测试，全部不在本报告。R1 M-5 被 **引用并深化**（R2E-M-4 补充「不在 `tasks` 集合 / `shutdown` 不等它 / `to_thread` 不可取消」三点）。
- **不重复 R3**：R3 已确认的 python H-3 在本报告只作为早退分支 E1 的一行登记，并明确标注**不是永久挂起**。R3「已修复」的 python H-1/H-2 未涉及。
- **不重复 R2-A**：`docs/review/r2-approval.md` 覆盖审批 ticket ledger；本报告只在 4.2 引用其 `kHostWorkerCount = 4` 的实测结论作为「Python 侧不设并发上限」的后果论据。
- 本报告新增且**只有本报告**有的内容：H1/H2/H2b/H3 四条永久挂起分支的机制与转录、H4（sync delivery 无响应帧）、M-1（admission 窗口不可取消）、M-3（stop 竞态）、4.2 的 `_active_runs` 无上限证成、5.4 的 admission 置位次序缺陷、7.3 的三个静默 no-op。
