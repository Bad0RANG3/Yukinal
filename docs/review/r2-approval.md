# R2-A：审批 / 授权状态机端到端追查（host ticket ledger ↔ sidecar admission）

## 1. 审查范围、方法与总体结论

**审查范围**（只读）：`native/desktop_host/src/desktop_approval.cpp`、`native/desktop_host/include/yukinal/desktop_host/desktop_approval.hpp`、`native/desktop_host/src/ipc_router.cpp`（审批相关切片）、`native/desktop_host/src/sidecar.cpp`、`agent/src/yukinal_agent/sidecar.py`、`agent/src/yukinal_agent/host_tools.py`、`agent/src/yukinal_agent/ssh_tools.py`、`agent/src/yukinal_agent/model_agent.py`、`packages/shared/src/{schemas,types}`、`apps/desktop/src/features/agent/*`、`apps/desktop/src/lib/ipc.ts`、`native/desktop_host/tests/{test_database.cpp,test_sidecar.cpp}`。**未修改任何被审查文件**；本次唯一写入物是本报告。

**方法**：静态把两侧状态机逐行对齐后，用两类实测补足——(1) 仓库既有测试作为**基线**（`test_database.cpp:4073-4347` 的 ledger 块、`:4605-4615` / `:4961-4973` 的两处 effectful ticket gate、`test_sidecar.cpp:260-265`）；(2) 自建一个**端到端探针**（`IpcRouter` + 真实 `DesktopHostSidecar` + 伪造 relay sidecar）以确定性地把 host ticket 推进到 `Phase::Responding`，从而观测「未知 approvalId」「越白名单 decision」「worker 池被 `consume` 占满」三条路径的真实行为。探针源码与二进制按写作用域要求只存在于 `%TEMP%`，**已在本次审查结束前删除**；其构建/运行配方与逐行输出完整记录在 第 4 节。

**总体结论**：两侧状态机在**「是否真的授权」这一语义上是 fail-closed 的**——host ticket 是 effectful 动作的唯一通行证，且通行证校验把 run / trace / call / tool / input fingerprint / target host / serverId / workspaceId / environment / taskId / planId / planStepId / evidenceIds 全部纳入比对（`desktop_approval.cpp:126-147`），其中 12 个字段的「改一个就烧票」由 `test_database.cpp:4229-4269` 逐个实测；任何不匹配都会以 `denied_by_policy` + `approval_ticket` 拒绝执行（实测 `D2`/`D3`/`D4b` 及探针场景 `S1`/`S2`）。**但回执语义与用户可见状态是不可信的**：host 对「自己不认识的 approvalId」返回 `{"accepted": true}`（实测 D1），UI 据此显示「已批准一次」（`useAgentRun.ts:466-475`），而实际上什么都没被授权；ticket 铸造失败被静默吞掉（`ipc_router.cpp:8968-8973`）而审批卡片照常弹出；过期在 UI 与 sidecar 两侧各有独立判定且互不通气（UI 侧 `useAgentRun.ts:453-456` 本地早退，不通知 sidecar，sidecar 的 Future 白等整个 TTL）。此外 `HostApprovalLedger::consume` 在 `Responding` 相位**无超时、无取消感知**地自旋（`desktop_approval.cpp:369-371`），而 `kHostWorkerCount = 4`（`sidecar.cpp:42`），实测 4 个停在此处的 effectful 请求即可让 worker 池饱和，使一个与之无关的 `host.mcp.catalog` 请求被阻塞 **8002.624 ms**（对照：未饱和时 0.453 ms）。

发现计数（本报告口径）：**Critical 0 / High 2 / Medium 3 / Low 2（共 7）**。编号前缀 `R2-` 表示本报告局部编号，与 R1/R3 的 H-3、M-1 等编号**不共用序列**。

| ID | 级别 | 一句话 | 位置 |
| --- | --- | --- | --- |
| **R2-H-1** | High | host ledger 不认识的 `approvalId` 仍回报 `{"accepted": true}`，UI 显示「已批准一次」，而实际零授权（Q2） | `desktop_approval.cpp:309`、`ipc_router.cpp:9152-9171` |
| **R2-H-2** | High | `consume` 在 `Responding` 相位无上界自旋且不感知取消；4 个停驻请求即可占满 `kHostWorkerCount = 4`（Q4） | `desktop_approval.cpp:369-371`、`sidecar.cpp:36-42,483-495` |
| **R2-M-1** | Medium | ticket 铸造失败被静默吞掉，审批卡片照常弹出且可点（SF1） | `ipc_router.cpp:8968-8973` |
| **R2-M-2** | Medium | `decision` 只按长度校验、无白名单；host 会把越白名单决策原样转发给 sidecar（Q3） | `ipc_router.cpp:9141,9160-9162` |
| **R2-M-3** | Medium | 过期有两条互不通气的路径：UI 侧本地早退（`useAgentRun.ts:453-456`）**不通知 sidecar**，sidecar 的 Future 白等整个 TTL；`finish_response` 的冲突又被静默吞掉（SF5+SF9） | `useAgentRun.ts:260-263,453-456`、`sidecar.py:1086-1094`、`ipc_router.cpp:9156-9158` |
| **R2-L-1** | Low | `sessionGrantable` 是死代码：生产者硬编码 `False`，`isSessionGrantable` 零生产调用点（Q5） | `sidecar.py:1076`、`risk.ts:90-101` |
| **R2-L-2** | Low | `hostInput` 在投影前被剥离，UI/审计无法比对「批准了什么」与「校验了什么」（SF2） | `ipc_router.cpp:8977-8981` |

与 R3 的关系见 第 3.6 节：**R3 对 H-3 的翻案结论在本轮被完整复核并确认**——本文不写「越权」，只写「静默 no-op + 误导性回执」。R2-H-1 是 R3 H-3 的**精确重述**（同一机制、同一结论），R2-H-2 是 R3 明列「未能验证」项 4 的**实测补齐**。

---

## 2. 状态机全图与静默失败点（Q1）

### 2.0 状态机总览（`■` = 静默失败点，编号对应 2.2）

```
 [sidecar] PythonSidecar._await_approval
     │  result.status == "awaiting_approval"                      sidecar.py:1021-1057
     │  approval_id = "apr_" + sha256(run_id ␟ call_id ␟ fingerprint)[:32]
     │  _pending_approvals[approval_id] = Future
     ▼
  emit agent.waiting_approval  ─────────────────────────────────►  sidecar.py:1059-1084
     │                                                              ■SF9  expiresAt = now+120s，UI 无定时器
     ▼
 [host] SidecarClient 收到通知 → notification_sink                 ipc_router.cpp:8959-8974
     │
     ├─ if is_host_approval_tool_name(toolName):
     │      record_waiting_event(...)   ── 抛错则 catch 后丢弃 ────  ■SF1  静默无 ticket
     │           └─ Binding{9 个绑定字段}, Phase::Pending            desktop_approval.cpp:163-301
     │              TTL 校验: now < expires_at <= now+180
     │              票数上限 kMaxTickets = 256
     │
     ├─ approval.erase("hostInput")   ────────────────────────────  ■SF2  输入对 UI 不可见
     ▼
  sink → WebView2 → listenDesktop(schema 校验)                     ipc.ts:179-186
     │                                                              ■SF10 校验失败即丢弃
     ▼
 [UI] useAgentRun.on("agent.waiting_approval")                     useAgentRun.ts:254-259
     │  AgentEntryView 渲染审批卡（sessionGrantable===false → 无「本次运行批准」）
     ▼
  用户点击 ──► respondApproval                                      useAgentRun.ts:450-484
     │         ■SF9  Date.parse(expiresAt)<=now → 本地早退，不通知 sidecar
     ▼
 [host] agent_approval_respond                                     ipc_router.cpp:9137-9179
     │  approvalId≤256 / runId≤256 / decision≤32（无白名单 ── R2-M-2）
     │
     ├─ begin_response(id, run_id, decision, now)                   desktop_approval.cpp:304-334
     │     ├─ 未知 id            → std::nullopt  ────────────────►  ■SF3  不报错、继续往下走
     │     ├─ 已过期             → erase + throw "expired"
     │     ├─ run 不匹配         → throw "run id does not match"
     │     ├─ 非 Pending         → throw "already answered"
     │     ├─ approve_session    → throw "cannot be session grants"
     │     ├─ 其它 decision      → throw "approval decision is invalid"
     │     └─ approve_once/reject → Phase::Responding，返回 ResponseToken
     │
     ├─ finish = [&](bool accepted){ if(!token) return; ... }       ■SF3/SF4/SF5  三重静默
     │
     ├─ client.request("agent.approval.respond", params)  ────────►  [sidecar]
     │                                                                 _approval_respond   sidecar.py:774-792
     │                                                                   ├─ decision ∉ {approve_once,reject} → -32602
     │                                                                   ├─ 未知/run 不符/done → {"accepted": false}   ■SF11
     │                                                                   └─ 合法 → Future.set_result → {"accepted": true}
     │                                                                        └─► agent 恢复，_execute_tool 发 host.tool.execute
     │
     └─ accepted:true → finish(true) → finish_response                desktop_approval.cpp:336-353
           ├─ entry 不存在       → 静默 return                        ■SF4
           ├─ 非 Responding      → throw → 被 catch(...) 吞掉          ■SF5
           ├─ accepted && approve_once && 未过期 → Phase::Approved
           └─ 否则               → erase

 [host] host.tool.execute / host.ssh.effectful_action_target        ipc_router.cpp:9993-10176, 4082-4279
     │  durable plan 检查 → task/guardrail/plan 检查 → target 检查 → prepare_host_tool_call
     ▼
  approvals_.consume(ExecutionBinding)                               desktop_approval.cpp:355-380
     ├─ 未知 id                → Missing        ─┐
     ├─ 已过期                 → erase + Expired │
     ├─ Pending                → NotApproved     ├─► host_action_approval_failure
     ├─ Responding             → wait_for(1s) 循环 ■SF8 无上界、无取消    ipc_router.cpp:4062-4080
     └─ Approved               → matches() ? 放行&erase : Mismatch  ─┘  → denied_by_policy
                                                                            + detail.code = "approval_ticket"
```

**读法**：整条链路上**只有 `consume` 与 `begin_response` 的抛错分支是「有声」的**；其余每一个「不做事」的分支都是 `return` 或 `catch(...){}`。这就是为什么「用户看到已批准、动作被拒」这类现象在生产中完全无法归因。

### 2.1 正常路径（T1–T12）

| # | 阶段 | 位置 | 输入 → 输出 |
| --- | --- | --- | --- |
| T1 | sidecar 判定需要审批 | `sidecar.py:1021-1057` | `result.status == "awaiting_approval"` → 生成 `approval_id = "apr_" + sha256(f"{run_id}\x1f{call_id}\x1f{fingerprint}")[:32]`（`:1049-1051`），登记 `_pending_approvals[approval_id] = _PendingApproval(...)`（`:1052-1054`） |
| T2 | sidecar 发 `agent.waiting_approval` | `sidecar.py:1059-1084` | 事件含 `runId/traceId/callId/inputFingerprint/input/hostInput/reason/factsSummary/target/expiresAt`；plan-bound 工具另带 `planId/planStepId/evidenceIds`（`:1077-1081`） |
| T3 | host 铸造一次性 ticket | `ipc_router.cpp:8959-8973` → `desktop_approval.cpp:163-301` | `record_waiting_event` 校验字段/目标/taskId/planId+planStepId/`hostInput`/TTL，用 `content_revision_hex(input.dump())` 作 fingerprint，`entries.emplace` 进 `Phase::Pending`（`:299-301`） |
| T4 | host 投影给 UI | `ipc_router.cpp:8977-8993` | **剥离 `approval.hostInput`** 后经 notification sink 广播 `agent.waiting_approval` |
| T5 | UI 呈现审批卡 | `useAgentRun.ts:254-259`、`AgentEntryView.tsx:142-172` | `setRunState("waiting_approval")` + 审批条目；`sessionGrantable === false` 时不渲染「本次运行批准」（`:164-168`） |
| T6 | 用户点击 | `useAgentRun.ts:450-465` | `callDesktop(agentApprovalRespond, {approvalId, runId, decision, respondedAt})` |
| T7 | host 收响应 | `ipc_router.cpp:9137-9151` | 字段解析（`approvalId`≤256、`runId`≤256、`decision`≤32）→ `begin_response(approvalId, runId, decision, now)` |
| T8 | host 转发给 sidecar | `ipc_router.cpp:9160-9162` | `client.request(found->second, params, timeout)`（`agent_approval_respond` 方法名，见 `:9077-9086`） |
| T9 | sidecar 兑现 Future | `sidecar.py:774-792` → `1021-1106` | 决策合法 → `future.set_result(...)`；`_await_approval` 恢复，工具真正执行 |
| T10 | host 兑现 ticket | `ipc_router.cpp:9163-9171` | 收到 `accepted` bool → `finish(accepted)` → `finish_response`（`desktop_approval.cpp:336-353`）置 `Approved` 或 erase |
| T11 | effectful 动作取 ticket | `desktop_approval.cpp:355-380` | `consume(ExecutionBinding)` → `Approved` 且 `matches()` → erase + 放行；否则 `TicketError` |
| T12 | host 执行或拒绝 | `ipc_router.cpp:10138-10176`、`:4082-4279` | ticket 缺失 → `host_action_approval_failure`（`:4062-4080`）→ `denied_by_policy` + `detail.code = approval_ticket` |

### 2.2 静默失败点清单（本报告的核心产出）

| ID | 位置 | 静默行为 | 后果 |
| --- | --- | --- | --- |
| **SF1** | `ipc_router.cpp:8968-8973` | `record_waiting_event` 抛出的任何 `std::exception` 被 `catch` 后**只留注释**（"A malformed or unsupported approval simply mints no ticket"） | ticket 不存在，但 `agent.waiting_approval` 已照常广播给 UI → 卡片可点，点了必然无效 |
| **SF2** | `ipc_router.cpp:8977-8981` | 广播前 `approval.erase("hostInput")` | UI/审计看不到 host 侧真正被指纹化的输入；「用户批准了什么」与「host 校验了什么」在 UI 上不可比对 |
| **SF3** | `desktop_approval.cpp:309` + `ipc_router.cpp:9152-9159,9163-9171` | 未知 approvalId → `begin_response` 返回 `std::nullopt` → `finish` 闭包首行 `if (!token.has_value()) return;` → 仍然走 `finish(accepted)` 并返回 `{accepted:true}` | **回执说成功，实际零授权**（实测 D1） |
| **SF4** | `desktop_approval.cpp:339-340` | `finish_response` 找不到 entry 时**静默 `return`** | 与 SF3 叠加，形成「无副作用也无日志」的完整静默链 |
| **SF5** | `ipc_router.cpp:9156-9158` | `finish_response` 抛异常被 `catch (...) { /* nothing to do */ }` 吞掉 | 过期/已答/相位错误等真实冲突对调用方不可见 |
| **SF6** | `sidecar.cpp:212` | `complete_request` 找不到 `pending[id]` 时 `return` | 迟到/重复的响应帧被静默丢弃 |
| **SF7** | `sidecar.cpp:251-255` | notification sink 抛异常 `catch (...)` | UI 通知失败不留任何可观测痕迹 |
| **SF8** | `desktop_approval.cpp:369-371` | `Phase::Responding` 时 `changed.wait_for(lock, 1s); continue;` —— **无总超时、无取消感知**（`host.tool.cancel` 的 atomic 令牌根本传不到 `consume`） | 4 个停在此处的请求即可占满 `kHostWorkerCount = 4`（实测 Q4） |
| **SF9** | `sidecar.py:1086-1090` + `useAgentRun.ts:260-263,453-456` | 过期有**两条互不通气的路径**：(a) sidecar 侧 TTL 到期发 `agent.approval_expired`；(b) UI 侧**只在点击时**才比对 `Date.parse(expiresAt) <= Date.now()` 并本地早退，**不通知 sidecar** | 路径 (b) 不通知 sidecar，其 Future 白等完整 TTL（`sidecar.py:114` 的 120 s），host ticket 也保持 `Pending` 直到 `record_waiting_event` 的清理循环（`desktop_approval.cpp:276-282`）碰巧扫掉它。**UI 没有定时器**，所以在 (a) 到达前卡片上的按钮一直可见；一旦点击，`AgentEntryView.tsx:154-155` 的 `approvalStatus ?` 会整体替换按钮区，按钮随之消失——这一点是 fail-closed 的 |
| **SF10** | `apps/desktop/src/lib/ipc.ts:179-186,191-197` | `listenDesktop` 对 schema 校验失败的事件**丢弃并只打 warning**，且 `warnDroppedEvent` 用 `reportedDroppedEvents` 按事件名去重 | `agent.waiting_approval` 载荷一旦超出 `agent.ts:15-42` 的 strict schema（`superRefine` 还额外要求 `server.exec` 必带 `traceId/callId/inputFingerprint/planId/planStepId`），UI 静默收不到审批；**首次之后的同类丢弃连 warning 都不再打** |
| **SF11** | `sidecar.py:788-789` | 未知/run 不匹配/已处理的 approvalId → `{"accepted": False}`，**而非错误** | 与 SF3 组合时，两侧都「不做事」但回执形态不同（host 说 true、sidecar 说 false）；仅当 host 侧也认识该 id 时 `finish(false)` 才会 erase ticket |

> **关于 R1 的 H-3**：`docs/review/r1-desktop.md:298-338` 把「返回误导性 `accepted`」写成安全隐患；`docs/review/r3-verification.md:390-456` 已翻案为「静默无操作 + 误导性回执」。本轮**独立复核并同意 R3**：`begin_response` 返回 `nullopt`（`desktop_approval.cpp:309`），因此**不发生任何授权**；`approve_session` 在 host 与 sidecar **两侧**都被硬拒（实测 D5/D7）。本文一律不使用「越权」表述。

---

## 3. 五个必答问题的逐条判定

| # | 问题 | 判定 | 关键证据 | 置信度 |
| --- | --- | --- | --- | --- |
| Q1 | 状态机与静默失败点 | **成立** | 第 2 节 T1–T12 + SF1–SF11 | 高（逐行对照） |
| Q2 | 未知 approvalId 的 `accepted` 实际值 | **成立**（回执 true 但零授权） | 实测 D1/D2/D3 + `desktop_approval.cpp:309`、`ipc_router.cpp:9152-9171` | 高（运行期实测） |
| Q3 | `decision` 白名单缺失的可达后果 | **成立**（但均 fail-closed） | 实测 D5/D6/D7 + `ipc_router.cpp:9141` | 高（运行期实测） |
| Q4 | `consume` 阻塞对 worker 池的影响 | **成立** | 探针场景（控制台标签 `S0`/`S1`/`S2`）+ `probe-report.json` 的 `catalog_latency_ms` 对照；静态 `sidecar.cpp:36-42,195,235-242,442-495`、`desktop_approval.cpp:369-371` | 高（运行期实测，含对照） |
| Q5 | `sessionGrantable` 端到端是否可达 | **成立**（结构扫描；不可达） | `sidecar.py:1076`、`risk.ts:90-101`、`AgentEntryView.tsx:151-153,164-168` | 中高（**穷尽静态调用点扫描，无运行期证据**） |

### 3.1 Q2：未知 approvalId 的 `accepted` 真实值 —— 判定 **成立**

**问题原述**：当 host ledger 不认识的 `approvalId` 被 respond 时，`accepted` 是什么？是否等价于授权成功？

**静态链**：

```
ipc_router.cpp:9145-9151   begin_response(...)  → try/catch → failure("INVALID_PARAMS", what())
desktop_approval.cpp:307-313  if (found == entries.end()) return std::nullopt;      // ← 未知 id
ipc_router.cpp:9152-9159   const auto finish = [&](bool accepted) {
                             if (!token.has_value()) return;                          // ← 静默 no-op
                             try { approvals_.finish_response(...); }
                             catch (const std::exception&) { /* nothing to do */ }
                           };
ipc_router.cpp:9160-9162   forward 到 sidecar（method 见 :9077-9086）
ipc_router.cpp:9163-9171   accepted 为 bool → finish(accepted) → return {true, {{"accepted", accepted}}}
```

`token` 为 `nullopt` 时，`finish` 什么也不做，但**返回给 UI 的 `accepted` 完全由 sidecar 决定**。若 sidecar 恰好认识这个 id（例如 host ticket 因 TTL/taskId 校验失败而从未铸造，见 SF1），sidecar 会返回 `accepted: true`。

**实测（D1）**：

```
D1 respond(apr_sidecar_only, host ledger has no such ticket) ok=1 accepted=true
   code= payload={"accepted":true}
```

`apr_sidecar_only` 从未在 host ledger 里存在过（relay 直接把它放进 sidecar 侧待批表）。回执 `{"accepted":true}`，UI 路径（`useAgentRun.ts:466-475`）命中 `approve_once` 分支 → 显示「已批准一次」。

**随后对同一 id 发起 effectful 动作（D2/D3）**：

```
D2 host.tool.execute(apr_sidecar_only) code=denied_by_policy detail=approval_ticket
   message=host action has no host-issued approval ticket
D3 host.tool.execute(apr_never_minted) code=denied_by_policy detail=approval_ticket
```

→ **回执为真、授权为零**。UI 上「已批准一次」与随后工具卡片的「策略拒绝」并存，用户没有任何线索指向真正原因（ticket 铸造失败被 SF1 吞掉）。这是本报告最值得优先修的一条。

**反向情形（host 有 ticket、sidecar 说 false）—— 实测 D4/D4b**：

```
D4  respond(apr_host_stale, host ticket + sidecar accepted=false) ok=1 accepted=false
D4b host.tool.execute(apr_host_stale, burned) code=denied_by_policy detail=approval_ticket
```

此路径**是安全的**：`finish(false)` 在 `finish_response`（`desktop_approval.cpp:345-351`）里因 `approved = accepted && ...` 为假而 erase ticket，后续动作 fail-closed。

**仓库既有测试覆盖了什么、漏了什么**：

| 分支 | 覆盖 | 位置 |
| --- | --- | --- |
| host 不认识 id 时 `consume` → `Missing` | ✅ | `test_database.cpp:4212-4213`（"an unknown approval id must have no ticket"）、`:4221-4222`（forged id） |
| host 有票、sidecar 拒绝 → 无票可领 | ✅ | `test_database.cpp:4314-4320`（"a response the sidecar did not accept must issue no ticket"） |
| **两侧都不认识 id** → `accepted == false` | ✅ | `test_sidecar.cpp:260-265` |
| **host 不认识、sidecar 认识 → `accepted == true`** | ❌ **无任何测试** | D1 所测 |

即：**`consume` 侧**对未知 id 的 fail-closed 已有回归（`:4212`），但 **`agent_approval_respond` 侧**回报给 UI 的 `accepted` 取值**完全没有覆盖**。这是 R2-H-1 能在仓库测试全绿的情况下长期存在的原因。

### 3.2 Q3：`decision` 白名单缺失的可达后果 —— 判定 **成立**（但每条都 fail-closed）

**静态**：host 只按长度校验 `decision`：

```
ipc_router.cpp:9137-9144   approvalId ≤256, runId ≤256, decision ≤32   // 无 z.enum 白名单
```

而三处白名单各不相同：

- TS 契约：`APPROVAL_DECISIONS = ["approve_once","approve_session","reject"]`（`packages/shared/src/types/chat.ts:411`，`permission.ts:140-145` 的 `z.enum`）
- sidecar：只接受 `("approve_once","reject")`（`sidecar.py:783-786`）
- host ticket：`approve_session` 显式抛错（`desktop_approval.cpp:326-328`）

**实测三条分支**：

```
D5 respond(apr_relay_pending, approve_session on a HOST ticket) ok=0 accepted=(absent)
   code=INVALID_PARAMS message="host action approvals are per call and cannot be session grants"
D6 respond(apr_sidecar_only, approve_forever = 越出所有白名单) ok=0 accepted=(absent)
   code=INVALID_PARAMS message="invalid approval response"
D7 respond(apr_sidecar_only, approve_session on a SIDECAR-ONLY ticket) ok=0 accepted=(absent)
   code=INVALID_PARAMS message="invalid approval response"
```

D6/D7 的 `responded` 记录同时证明：**host 把越白名单的 decision 原样转发给了 sidecar**，是 sidecar 拒绝的（`probe-report.json` 的 `responded` 数组含 `[apr_sidecar_only, approve_forever]` 与 `[apr_sidecar_only, approve_session]`）。D5 则是 host 在转发**之前**就抛错。

**可达后果**：`agent_approval_respond` 返回 IPC 层 `INVALID_PARAMS`（`ok=0`），前端落进 `useAgentRun.ts:476-478` 的 `catch` → 追加一条**原始错误文本**的条目。**不存在**「用户以为批准了、实际静默跳过」的路径，因为 host ticket gate 对每个 effectful 动作都 fail-closed（`:10165-10175`）。危害等级因此是**中**（错误处理粗糙 + 契约分层不一致），不是 High。

**额外发现**：`useAgentRun.ts:466-475` 对 `accepted === true` 且 `decision === "approve_session"` 的分支会显示「已批准本次运行」；但该分支**在生产中不可达**（host 与 sidecar 都会先拒），只有在 host ledger 不认识该 id 且 sidecar 接受 session 授予时才会走到——而 sidecar 同样不接受。即该分支是**死代码**，却给出了最强的成功文案。

### 3.3 Q4：`consume` 阻塞对 worker 池的影响 —— 判定 **成立**（含运行期实测）

**静态链**：

```
sidecar.cpp:36-42      kHostWorkerCount = 4    // 注释自陈「小线程池」
sidecar.cpp:442-481    handle_host_frame：内联调用 handler（:467）
sidecar.cpp:483-495    host_worker()：出队后同步执行 handle_host_frame（:493）
desktop_approval.cpp:369-371
                       case Phase::Responding: changed.wait_for(lock, 1s); continue;   // 无上界
```

`consume` 只在 ticket 已经是 `Responding` 时才等待（`Pending` 直接返回 `NotApproved`，`:367-368`）。因此要复现占用，必须让 `agent_approval_respond` 的 `begin_response` 先于饱和请求到达——这正是探针 relay 的 `saturate()` 所构造的时序。

**实测（同一次运行内的对照；`S0`/`S1`/`S2` 是探针的本地场景标签，与 2.2 节的 `SF1`–`SF11` 无关，见 4.3 的编号说明）**：

```
P0 agent_spawn ok=1 toolCount=0 pid=33776
...
S0 respond(apr_relay_pending, approve_once) ok=1 accepted=true waited_ms=9102
S1 host.tool.execute(apr_relay_pending, after the burst) code=denied_by_policy detail=approval_ticket
S2 host.tool.execute(apr_relay_pending again)              code=denied_by_policy detail=approval_ticket
```

`data/probe-report.json`（下为逐字摘录；该文件随探针目录一并删除，原始数值已固化在此）：

```json
"balanced": true,
"burst_started":   1792262353.951,
"catalog_sent":    1792262355.053,
"respond_released":1792262363.053,
"catalog_latency_ms": { "2000": 0.453, "2001": 8002.624 },
"effectful_ms": { "1001": 9102.892, "1002": 8953.550, "1003": 8802.779, "1004": 8652.340 },
"unsent": [], "respond_id": 7
```

读法：

- `2000` 是**对照组**——同一方法 `host.mcp.catalog` 在 `tools.list` 期间发出，耗时 **0.453 ms**。
- `2001` 是在 4 个 effectful 请求**已经停驻在 `consume`** 之后发出的同一方法，耗时 **8002.624 ms**（≈ relaying 的释放延迟），即被无关请求完全堵住。
- 4 个 effectful 请求（1001–1004）分别停驻 **8652–9103 ms**，全部等到 `respond_released` 才返回。
- 4 个停驻请求 + 1 个 catalog 请求 = **5 个被阻塞的 RPC 争抢 4 个 worker** ⇒ 池子被停驻的 `consume` 调用完全吃满。
- 四个 plan 绑定互不相同（`itask_s0..s3` / `iplan_s0..s3` / `step_s0..s3`），因此不是 `duplicate_call` 折叠而是**四条真实并发路径**；`balanced: true` 说明四条都走到了 ticket gate。

**证据边界（必须声明）**：本探针直接驱动 `IpcRouter::handle_host_request` 与真实 `DesktopHostSidecar`，因此它证明的是「ledger 会把调用方停驻任意久」，而「4 个线程即饱和」这一推论来自 `kHostWorkerCount = 4` 的**结构事实**（`sidecar.cpp:42`，注释在 `:39-41`）——与 R1 M-1 自陈的口径一致（`r1-desktop.md:420`：「worker 占用的影响是基于 `kHostWorkerCount = 4` 的结构推断，未做运行期测量」）。本轮把该推断**升级为实测**：饱和度不是推断，`catalog_latency_ms` 的 0.453 ms → 8002.624 ms 是同一进程内的对照测量。

**为何是 High**：`host.tool.execute`（MCP 调用）被 `sidecar.cpp:39-41` 的注释明确列为「不应阻塞 `host.tool.cancel`」的对象，而 `host.tool.cancel` 在 `handle_host_frame` 里被**内联特判**（`:445-446`）以绕开池子——这恰好说明作者已意识到池子会被占满。但这条逃生路径有两处结构性缺陷：

1. **`host.tool.cancel` 本身也要先被 worker 取出**。它在 `process_frame_line` 里走的是 `enqueue_host_request`（`sidecar.cpp:195` → `:235-242`），与普通请求**共用同一个队列**，「内联处理」只省掉了 handler 派发，并没有省掉线程。池子被停驻的 `consume` 全部吃满时，cancel 帧只能排在队列里，等不到执行。
2. **`cancel_host_request` 只认已在 `cancellations` 里登记的 requestId**（`sidecar.cpp:429-438`），而登记动作发生在 worker **取出**请求之后（`:448-452`）。仍停在队列里的请求没有令牌，cancel 会返回 `{"cancelled": false}` 而不报错。

即便 cancel 能送达，`consume` 也读不到它：其签名（`desktop_approval.hpp:85-86`）只有 `ExecutionBinding` 与 `now`，既无超时也无取消令牌。攻击面是**本地 DoS / UI 冻结**，不是提权。

**证据分级**：上面第 1、2 条是**静态结构推断**（阅读 `sidecar.cpp:195,235-242,429-438,442-481` 得出，未构造实测——需要让 cancel 帧在池子饱和时到达）；「`consume` 在 `Responding` 无上界」这一点是**实测**（`effectful_ms` 四个值全部等于释放延迟，说明循环确实一直在转）。

### 3.4 Q5：`sessionGrantable` 端到端是否可达 —— 判定 **成立**（不可达）

**生产者硬编码 false**：

```
sidecar.py:1059-1084   "sessionGrantable": False        // :1076，字面常量
sidecar.py:2002-2031   PythonSidecar(...) 未传 approval_ttl_seconds → 120s 恒生效（:114）
```

**消费者**：

```
packages/shared/src/types/risk.ts:80     SESSION_GRANTABLE_DANGEROUS_ENVIRONMENTS = ["development","staging"]
packages/shared/src/types/risk.ts:90-101 isSessionGrantable(...)
```

`isSessionGrantable` 的**全部**调用点是 `risk.test.ts:32,42,43`（`grep -rn "isSessionGrantable" --include=*.ts --include=*.tsx`）。**生产代码零调用** ⇒ 该函数与 `SESSION_GRANTABLE_DANGEROUS_ENVIRONMENTS` 是纯死代码，真正的判定被 `sidecar.py:1076` 的硬编码 `False` 取代。

**UI**：

```
AgentEntryView.tsx:151-153   sessionGrantable === false → 「此操作每次都需要单独批准，不能记住到本次运行。」
AgentEntryView.tsx:164-168   「本次运行批准」仅在 sessionGrantable !== false 时渲染
apps/desktop/tests/agent-approval.test.tsx:39,47-56  覆盖了 false / 缺省 / true 三种渲染
```

**因此**：生产链路上「本次运行批准」按钮**永不出现**（`agent.ts:30` 的 `sessionGrantable: z.boolean().optional()` 只在旧 sidecar 缺字段时让按钮回来）。即便强制让它出现（缺字段/true），点击后：
- host ticket 存在 → `begin_response` 抛 `approve_session` 不允许（实测 `D5`）
- host ticket 不存在 → 转发给 sidecar → `_approval_respond` 返回 `INVALID_PARAMS`（实测 `D7`）

两种情况都不接受，UI 落进 `useAgentRun.ts:476-478` 的 catch，或（若被接受）本应显示「已批准本次运行」（`:472-474`）。

**置信度声明**：Q5 的结论建立在**穷尽静态调用点扫描**上，**没有运行期证据**——没有跑 Electron UI，也没有伪造一个 `sessionGrantable: true` 的事件穿过真实 `listenDesktop`。若要升到「高」，需要一个 React 测试：注入 `sessionGrantable: true` 的 `agent.waiting_approval`，点击「本次运行批准」，断言 `agentApprovalRespond` 被调用后 UI 显示「已批准本次运行」而非错误。当前 `agent-approval.test.tsx` 只覆盖了**按钮显隐**，未覆盖**点击后的失败呈现**。

### 3.5 TTL 不对称（附带发现，非题目所问但影响 SF1/SF9）

```
sidecar.py:114,120-121      _approval_ttl_seconds = 120，合法范围 1–3600
sidecar.py:1058             expires_at = now + ttl                 → 事件里 120s
desktop_approval.cpp:24     kMaxApprovalTtlSeconds = 180
desktop_approval.cpp:238-242  到期 ≤ now 或 > now+180 → throw
desktop_approval.cpp:296-298  票数上限 kMaxTickets = 256 → throw
```

当前 120 < 180，**恰好安全**；但 120 这个值只在 `PythonSidecar.__init__` 默认值上，`main()`（`sidecar.py:2002-2031`）**不传该参数**，所以「sidecar TTL 一旦被配置到 >180 就静默无 ticket」的失效模式是**配置可达**的：`record_waiting_event` 抛错 → 被 SF1 吞掉 → 卡片照常、回执照真（SF3）→ 动作被拒。TTL 上限没有一个**双侧共享的常量或协商**，这是 SF1 之所以危险的结构性原因。

### 3.6 与 R3 的一致性核查（强制项）

| R3 结论 | 本文复核 | 一致性 |
| --- | --- | --- |
| `unknown approvalId` → `begin_response` 返回 `nullopt`、不发生授权（`r3-verification.md:414,437,445`） | 实测 D1+D2 确认：回执 true，动作被 `denied_by_policy` 拒 | ✅ 一致，本文沿用「静默 no-op + 误导性回执」口径 |
| `approve_session` 在 host 与 sidecar **两侧**硬拒（实测 D5/D7） | 两侧均确认 | ✅ 一致 |
| R3「未能验证」项 4（`r3-verification.md:662-663`）= 本任务的 Q4 | **本轮实测完成**：`catalog_latency_ms` 0.453 → 8002.624 ms | ✅ 补齐，未与 R3 冲突 |
| R3 C-1（keyboard-interactive 在 service 层已实现+测试，缺口只在 `native/desktop_host`） | 本文不涉及该链路，未复核 | ⚪ 未复用 |

**无证据冲突**：本轮所有实测结果都与 R3 同向。唯一需要标注的**口径差异**是 R1 的 M-1 把 Q4 归为「结构推断」（`r1-desktop.md:420`），本文将其**升级为实测**，属于加强而非推翻。

**须记录的引用更正**：任务描述中「`test_database.cpp:1145-1167` 附近的审批断言」**指向错误**。该区间实际是 `insert_snapshot` 健康拒绝与 `server_auth_respond`/`server_auth_cancel` 的空闲 `accepted:false` 断言（`test_database.cpp:1140-1167`），**与审批 ledger 无关**。审批相关断言的真正位置：

- `test_database.cpp:4073-4347`：`agent.waiting_approval` → ticket 铸造 → `begin_response`/`finish_response`/`consume` 的完整 ledger 块（`:4074-4075` 注释、`:4150`、`:4212-4225`、`:4264-4266`、`:4292`、`:4311`、`:4319` 的 `TicketError` 断言、`:4284` 的 `approve_session` 拒绝断言）
- `test_database.cpp:4605-4615`：SSH effectful ticket gate
- `test_database.cpp:4961-4973`：`host.tool.execute` ticket gate
- `test_sidecar.cpp:260-265`：两侧都不认识 approvalId 的 `accepted == false`

---

## 4. 最小复现命令

### 4.1 立即可跑（仓库既有，无需新增文件）

```powershell
# R-A1  ledger 全链（含 approve_session 拒绝、TicketError 各分支）
native/build-nmake/yukinal_desktop_host_database_tests.exe

# R-A2  两侧都不认识 approvalId → accepted == false（Q2 的已覆盖分支）
native/build-nmake/yukinal_desktop_host_sidecar_tests.exe `
  D:\yukinal\agent\.venv\Scripts\python.exe D:\yukinal\agent\src

# R-A3  sidecar 侧 decision 白名单
agent\.venv\Scripts\python.exe -m unittest discover -s agent/tests -p "test_agent_*.py"
```

实测（本报告定稿前复跑，全部 `EXIT=0`）：

| 命令 | 退出码 | 耗时 / 规模 |
| --- | --- | --- |
| R-A1 | 0 | 8860 ms |
| R-A2 | 0 | — |
| R-A3 | 0 | `Ran 100 tests in 8.967s` … `OK` |

### 4.2 静态定位（Q2/Q3/Q5 的失败点，一行一条）

```powershell
# SF3：未知 id 只让 begin_response 返回 nullopt
Select-String -Path native/desktop_host/src/desktop_approval.cpp -Pattern 'return std::nullopt;' -Context 2,0 | Select-Object -First 3

# SF3：finish 闭包对 nullopt token 静默返回，却仍回报 accepted
Select-String -Path native/desktop_host/src/ipc_router.cpp -Pattern 'if \(!token.has_value\(\)\) return;' -Context 6,10

# Q3：decision 只限长度、无白名单
Select-String -Path native/desktop_host/src/ipc_router.cpp -Pattern '"decision"' -Context 3,3

# SF1：ticket 铸造失败被吞
Select-String -Path native/desktop_host/src/ipc_router.cpp -Pattern 'mints no ticket' -Context 6,2

# Q5：sessionGrantable 的唯一生产者硬编码 False，消费者零调用
Select-String -Path agent/src/yukinal_agent/sidecar.py -Pattern 'sessionGrantable'
Select-String -Path apps/desktop/src,packages/shared/src -Include *.ts,*.tsx -Pattern 'isSessionGrantable' -Recurse
```

### 4.3 端到端探针（本次审查临时构建，已删除；配方如下）

**目的**：确定性地把 host ticket 推进到 `Phase::Responding`，以观测未知 id 的回执（D1）、越白名单 decision 的转发（D6/D7）、以及 worker 池饱和（Q4）。

**构建与运行**（在 `%TEMP%\r2probe` 下，`router_probe.cpp` + `relay/yukinal_agent/sidecar.py`）：

```powershell
$d = "C:\Users\Bad0RANG3\AppData\Local\Temp\r2probe"
cmd /c "call `"C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat`" >nul 2>&1 && cl /nologo /DWIN32 /D_WINDOWS /EHsc -std:c++20 /MT /utf-8 /DYUKINAL_DESKTOP_VERSION=`"1.0.0`" /I D:\yukinal\native\desktop_host\include /I D:\yukinal\native\build-nmake\_deps\nlohmann_json-src\include /I D:\yukinal\native\build-nmake\_deps\yukinal_sqlite-src /Fe:router_probe.exe /Fo:router_probe.obj router_probe.cpp /link /nologo /LIBPATH:D:\yukinal\native\build-nmake /LIBPATH:D:\yukinal\native\build-nmake\_deps\openssl-install\lib /LIBPATH:D:\yukinal\native\build-nmake\_deps\libssh2-build\src yukinal_desktop_host_core.lib yukinal_ssh_core.lib yukinal_sqlite.lib libssh2.lib libssl.lib libcrypto.lib ws2_32.lib advapi32.lib user32.lib gdi32.lib crypt32.lib bcrypt.lib shell32.lib comdlg32.lib ole32.lib"
Remove-Item -Recurse -Force "$d\data" -ErrorAction SilentlyContinue
.\router_probe.exe "D:\yukinal\agent\.venv\Scripts\python.exe" "$d\relay" "$d\data"
Get-Content "$d\data\probe-report.json"
```

**探针结构**（可在需要时按此重建）：

1. `router_probe.cpp`（链接 `yukinal_desktop_host_core.lib`，直接使用 `IpcRouter`——`ipc_router.hpp:44-49`，这是文档化的测试入口）：
   - 设 `YUKINAL_AGENT_PYTHON` / `YUKINAL_AGENT_SOURCE` / `YUKINAL_DATA_DIR`
   - 建 `DesktopDatabase(probe_dir)` + `MemoryCredentialStore`（`test_database.cpp:76-109`）
   - 裸 SQL 插入 server `srv_probe`（`43.240.221.2:22`、`root`、`idn_probe`、`environment=staging`）与 identity `idn_probe`；`HostTrustStore::load(probe_dir/"known_hosts").pin("43.240.221.2",22,"SHA256:"+43×'A')`
   - `make_plan()` 为每个并发请求造**独立**的 `itask_*/iplan_*/step_*`（否则折叠为 `duplicate_call`）
   - 场景序列：`P0` spawn → `D1`/`D2`/`D3`（Q2）→ `D4`/`D4b`（反向）→ `D5`/`D6`/`D7`（Q3）→ `S0`/`S1`/`S2`（Q4 释放后）→ `P9` kill

> **编号说明**：探针的控制台标签 `S0`/`S1`/`S2` 是**探针本地场景名**（spawn 后的第 0/1/2 个饱和度检查点），与 2.2 节的静默失败点编号 `SF1`–`SF11` **完全无关**。本节以下引用探针输出时一律保留原始标签以免与日志不符。
2. `relay/yukinal_agent/sidecar.py`（~230 行的伪造 sidecar）：`tools.list` 时连发 3 条 `agent.waiting_approval`（`apr_relay_pending` / `apr_host_stale` / `apr_session_grant`）；`agent.approval.respond` 复刻 `sidecar.py:783-786` 的白名单（非 `approve_once`/`reject` → JSON-RPC `-32602`）；收到 `apr_relay_pending` 后启动 `saturate()`——按 0.15 s 间隔发 4 个 `host.ssh.effectful_action_target`（id 1001–1004，均带 `approvalId=apr_relay_pending`，但各自的 task/plan/step/trace/call 不同），0.5 s 后发 `host.mcp.catalog`（id 2001，对照组为 `tools.list` 内的 id 2000），再等 8.0 s 才回响应 → 逐 id 记录延迟并写 `probe-report.json`。

> **为什么需要伪造 sidecar**：`consume` 在 `Pending` 相位立即返回 `NotApproved`（`desktop_approval.cpp:367-368`），只有 `Responding` 才等待。因此必须让 `begin_response` 先执行、再让饱和请求到达——真实 sidecar 无法精确构造该时序。

**实测输出摘要**：见 3.1（D1–D4b）、3.2（D5–D7）、3.3（Q4 的 `catalog_latency_ms` 对照与 `effectful_ms`）。完整控制台输出（P0/D1/D2/D3/D4/D4b/D5/D6/D7/S0/S1/S2/P9/PROBE_DONE）与 `probe-report.json` 已在上文逐条引用。

### 4.4 建议补入仓库的回归（修复可测性的前提）

**注意一个可测性障碍**：`IpcRouter` **没有**注入假 sidecar 的公开接缝——`sidecar_`、`ensure_sidecar()`、`configure_sidecar()` 全在 `private` 段（`ipc_router.hpp:127-128,157-158`）。`SidecarClient::set_host_handler`（`test_sidecar.cpp:100,143`）是**相反方向**的接缝（host 应答 sidecar 的请求），注入它无法让 `agent_approval_respond` 收到伪造的 `accepted`。因此复现 R2-H-1 只有两条路：

**(a) 单元级（今天就能加，但抓不到 R2-H-1 本身）**：直接断言 ledger 层对未知 id 的行为——这一条**已经存在**（`test_database.cpp:4212-4213`），不是新增价值。

**(b) 端到端（能抓到 R2-H-1，需按探针方式配置假 agent 源）**：沿用 `test_sidecar.cpp:196-210` 的 `SetEnvironmentVariableA("YUKINAL_AGENT_PYTHON"/"YUKINAL_AGENT_SOURCE", ...)` 模式，把 `YUKINAL_AGENT_SOURCE` 指向一个**假 sidecar 源目录**（即本报告 4.3 的 relay），然后：

```cpp
// 建议位置：native/desktop_host/tests/test_sidecar.cpp，紧接 :260-265 之后
// R2-H-1 回归：host ledger 不认识、sidecar 认识的 approvalId 不得回报 accepted:true
// （假 source 的 tools.list 连发一条 waiting_approval 但不让 host 铸票，
//   并对该 id 的 respond 回 {"accepted": true}）
const auto sidecar_only = router.invoke(
    "agent_approval_respond",
    {{"approvalId", "apr_sidecar_only"}, {"runId", "run_router_execute"},
     {"decision", "approve_once"}, {"respondedAt", "2026-10-16T00:00:00Z"}});
check(!sidecar_only.ok, "the host must not report success for an approval it does not track");
```

**更小的替代方案**：在 `IpcRouter` 上加一个 `set_approval_sidecar` 或把 `approvals_`（`ipc_router.hpp:213`）暴露为测试接缝，这样 F1 的回归只需伪造 `begin_response` 的 `nullopt` 返回，不必启动假 Python 进程。

`IpcRouter::set_host_tool_runner`（`ipc_router.hpp:94-98`）已允许注入假 runner，可用以断言 ticket gate 先于任何副作用触发——这条与 F1 正交，但配合 (b) 可完整覆盖「回执 true 但零授权」。

---

## 5. 修复建议（按「最小改动 + 可测」排序）

| # | 改动 | 位置 | 可测性 |
| --- | --- | --- | --- |
| **F1** | 未知 `approvalId` 不再静默：`begin_response` 返回 `nullopt` 时，`agent_approval_respond` 直接返回 `failure("AGENT_APPROVAL_UNKNOWN", "The host does not track approval `" + id + "`")`，**不转发、不回报 `accepted`** | `ipc_router.cpp:9145-9160` | ✅ 4.4 的 C++ 回归；UI 侧 `useAgentRun.ts:476-478` 已能把 IPC 错误渲染出来 |
| **F2** | `finish_response` 找不到 entry 时不再静默：改为返回 `bool` 或抛特定错误，由 `ipc_router.cpp:9152-9159` 记录（至少 stderr/诊断日志） | `desktop_approval.cpp:339-340`、`ipc_router.cpp:9156-9158` | ✅ 单元级：两次 `finish_response` 同一 id |
| **F3** | `record_waiting_event` 失败不再静默：`ipc_router.cpp:8968-8973` 的 catch 里改发 `agent.approval_unavailable` 通知（或直接把 sink 派发改成「先铸票、失败则不广播」） | `ipc_router.cpp:8968-8981` | ✅ 造一个 TTL 200s 的 waiting 事件，断言 UI 收不到卡片 |
| **F4** | `decision` 加显式白名单：`ipc_router.cpp:9141` 后校验 `decision ∈ {approve_once, approve_session, reject}`，越界即 `INVALID_PARAMS` 且**不转发**；TS 契约成为唯一真源（生成或共享常量） | `ipc_router.cpp:9137-9144`、`packages/shared/src/types/chat.ts:411` | ✅ D6 的变体：断言 sidecar 未收到该请求（`responded` 为空） |
| **F5** | `consume` 加**总超时**与取消感知：签名增加 `const std::atomic<bool>* cancel` 与 `timeout`（例如沿用 `kMaxApprovalTtlSeconds`），循环里检查 `cancel->load()` 并限制累计等待；`ipc_router.cpp:10165-10175` 调用处传入既有 `cancel` 令牌 | `desktop_approval.hpp:65-80`、`desktop_approval.cpp:355-380`、`ipc_router.cpp:10165-10175` | ✅ 把 4.3 的 Q4 场景固化为回归：断言 `catalog_latency_ms` 与对照同量级 |
| **F6** | 让过期在 UI 与 sidecar 之间**单向同步**：`respondApproval` 本地判定过期时（`useAgentRun.ts:453-456`）不要只改状态，而是要么发 `agent_approval_respond(decision="reject")` 让 sidecar 提前释放 Future，要么新增一条显式的取消 RPC；同时用基于 `expiresAt` 的定时器把按钮置灰，而不是等 `agent.approval_expired` 才反应 | `useAgentRun.ts:260-263,450-456`、`sidecar.py:1086-1094` | ✅ `agent-approval.test.tsx` 加「超过 `expiresAt` 后按钮 disabled，且确实发生了一次调用」用例 |
| **F7** | 让 `sessionGrantable` 有真实生产者：`sidecar.py:1076` 改为 `isSessionGrantable(...)` 的等价判定（Python 侧需镜像 `risk.ts:80` 的环境白名单），或**删除** `isSessionGrantable`/`SESSION_GRANTABLE_DANGEROUS_ENVIRONMENTS` 与 UI 的 session 分支，把「本次运行批准」彻底下线 | `sidecar.py:1076`、`packages/shared/src/types/risk.ts:80-101`、`AgentEntryView.tsx:164-168`、`useAgentRun.ts:472-474` | ✅ 二选一：前者加一条「staging 环境下 sessionGrantable=true」的 sidecar 测试；后者删代码 + 删 `agent-approval.test.tsx:47-56` 的 true 分支 |
| **F8** | TTL 上限改为双侧共享常量并在 host 侧**校验前置**：sidecar 发出前 clamp 到 `min(ttl, 180)`，或 host 把上限提升到 sidecar 的 3600 并在 `initialize` 时协商 | `sidecar.py:114-121`、`desktop_approval.cpp:24` | ✅ 造 `approval_ttl_seconds=200` 的 sidecar，断言要么 clamp、要么明确报错 |

**不建议**的修法：在 `finish_response` 里对未知 entry **补建** ticket（让「未知 id」变成「隐式批准」）——那才是真正的越权，且与 R3 的翻案结论直接冲突。

---

## 6. 未验证 / 未覆盖

1. **Q5 无运行期证据**：未启动 Electron UI，未让 `sessionGrantable: true` 的事件穿过真实 `listenDesktop`。结论基于穷尽静态调用点扫描（已注明置信度「中高」）。
2. **未做真实 `host.tool.execute` 副作用的端到端验证**：探针断言了 ticket gate 的拒绝回执（`denied_by_policy` + `approval_ticket`），但**没有**验证「批准后动作真的执行」这一正向路径（需要真实 SSH/MCP 目标）。正向路径由 `test_database.cpp:4605-4615,4961-4973` 与既有 SSH 测试覆盖。
3. **未测量真实 IPC 往返下的 worker 占用**：`router_probe` 直接调用 `handle_host_request`，绕过了 WebView2 ↔ `main.cpp` 的进程边界（`main.cpp:293,297-308`）。因此「UI 冻结」是推论，测量到的是**池内阻塞**。
4. **未覆盖 `host.ssh.filesystem_write_target` 的 ticket 路径**（`:10414,10437-10438`、`ssh_tools.py:985-1106` 的 `_desktop_filesystem_write`）：该路径不走 `HostApprovalLedger`，`is_host_approval_tool_name`（`desktop_approval.cpp:101-104`）只认 `server.exec`/`docker.restart`/`systemd.restart`/`package.install`。其准入机制是否另有等价保护，本轮未追查。
5. **未复核 control-plane 的审批链**（`DEFAULT_APPROVAL_TTL_SECONDS = 900`）：那是独立服务，与桌面 host 的 180s 上限无关，不在本题范围。
6. **Q4 的两条附加论断为静态推断**：3.3 节末尾「`host.tool.cancel` 与普通请求共用队列、需等 worker 取出」与「未登记的 requestId 会被答以 `cancelled: false`」来自阅读 `sidecar.cpp:195,235-242,429-438,445-452`，**未构造实测**（需要让 cancel 帧在池子饱和时到达）。已被实测的只有「停驻无上界」这一点。
