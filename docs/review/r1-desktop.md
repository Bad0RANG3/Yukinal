# R1-B 广度审查：C++ 桌面宿主 + React 桌面 UI + TS 包

## 范围与方法

**范围**（只读，未修改任何源文件）：

- `native/desktop_host/src|include|tests`（IPC 路由、审批账本、终端、MCP supervisor、sidecar 客户端、SQLite、凭据库、窗口）
- `native/src` 仅前端 IPC 相关：`ipc_router.cpp` 的处理器注册与参数校验、`database.cpp` 的 schema/migration
- `apps/desktop/src`（features / components / stores / hooks / lib / app）、`apps/desktop/tests`、`apps/desktop/design`
- `packages/shared`（src / fixtures / schemas / ipc / protocol / events / naming）、`packages/agent-sdk`、`packages/provider-sdk`

**方法**：

1. 先读 `packages/shared` 的 schema 与契约（`schemas/ipc.ts`、`ipc/index.ts`、`events/index.ts`、`consistency.ts`、各领域 schema），再读 `native/desktop_host` 的 IPC 路由，逐条做**契约漂移对照**；不依赖 grep 结论，全部命中点回到文件用 read 逐段确认。
2. 生产者/消费者两侧分别做**穷尽字面量扫描**：事件名在 `native/**/*.cpp|hpp`、`agent/src/**/*.py`、`packages/shared/src/**/*.ts` 中的出现次数；86 个 `IPC_COMMANDS` 在 `ipc_router.cpp` 中的字面量覆盖。
3. 每条发现都给出 `file:line` + 可复核代码片段；置信度标注为 高/中/低（高 = 源码逐行确认且两侧对齐；中 = 一侧确认、另一侧只能推断；低 = 需要运行期复现才能定性）。

**总体结论**：C++ 宿主在**输入校验**这一层做得比预期扎实 —— `only_fields` 白名单、`valid_*` 边界、SQL 全绑定参数（动态 SQL 只拼列名，且列名来自 `table_columns` 常量）、密钥 scrubber、迁移前 `VACUUM INTO` 备份 + 清单 + 回滚，均未发现可利用缺陷。真正的问题不在单点实现，而在**跨语言契约只有一侧活着**：`packages/shared` 里有一整套精确的 zod 契约、一套事件词汇表和三组断言测试，但它们断言的是已不存在的 Rust 侧（`crates/core/src/ipc.rs`、`apps/desktop/src-tauri/src/commands/mod.rs`、`apps/agent/src/agent-loop.ts` 全部不存在）。因此 19 个可订阅通道（`SUBSCRIBABLE`）里有 **6 个**在 C++/Python 里**没有任何生产者**（`activity.created`、`agent.usage`、`agent.thinking`、`server.auth_challenge`、`terminal.output_lost`、`investigation.schedule_notification`），它们全都挂着活的 UI 订阅者；另有 **3 个**只声明、本就无订阅者的名字（`server.connected`、`server.disconnected`、`server.updated`）同样没有生产者 —— 这两类性质不同，报告内分开列出；`agent_status.restart`、MCP 的 `lastExit`/`restart`/`pid` 同属此类，自动恢复整套界面因此永久显示「无」。最严重的一条是 SSH keyboard-interactive（TOTP/二次验证）认证链路：`native/src/ssh.cpp` 会发出 `auth_prompt`，`desktop_terminal.cpp` 直接丢弃，`ipc_router.cpp` 无条件回答 `accepted:false`，即**交互式认证在当前桌面宿主上不可能完成**。另有多条契约漂移会在运行期直接把 UI 打崩（verbatim 转发进 `z.strictObject`、MCP 列表把运行中的服务器报成未启动）。测试侧的风险是它把这些漂移**当作正确**钉住了（`event-vocabulary.test.ts:5-13`、`consistency.ts:133-141` 都写着「Rust emits them」），所以这不是「暂时没实现」，而是「测试在为一个不存在的实现对账」。

---

## 发现

### Critical

#### C-1 SSH keyboard-interactive（TOTP / 二次验证）认证链路端到端不可达

**位置**：
- `native/src/ssh.cpp:1493-1498`（生产者：发出 `auth_prompt`）
- `native/desktop_host/src/desktop_terminal.cpp:377-406`（消费者：丢弃）
- `native/desktop_host/src/ipc_router.cpp:8766-8792`（回执：无条件 `accepted:false`）
- `apps/desktop/src/features/servers/ServerAuthChallengeModal.tsx:26`（订阅一个无生产者的通道）
- `apps/desktop/src/features/servers/ServerAuthChallengeModal.tsx:76`（把 `accepted:false` 渲染成「已过期」）

**证据**：

生产者在 PTY worker 的 stdout 上发帧（`native/src/ssh.cpp`，逐字引文）：

```cpp
// native/src/ssh.cpp:1493-1498
  if (!emit_terminal_message({
          {"type", "auth_prompt"},
          {"name", name_length == 0 ? std::string{} : std::string(name, static_cast<std::size_t>(name_length))},
          {"instructions", instruction_length == 0 ? std::string{} : std::string(instruction, static_cast<std::size_t>(instruction_length))},
          {"prompts", std::move(prompt_values)},
      })) {
```

终端的帧解析器只认 `ready` / `output` / `error`，其它类型**静默丢弃**：

```cpp
// native/desktop_host/src/desktop_terminal.cpp:390-405（逐字引文，`…` 为省略的表达式）
    if (type == "ready") {
      emit("terminal.opened", {{"terminalSessionId", session_id}, …});
      return;
    }
    if (type == "output" && frame.contains("data") && frame.at("data").is_string()) {
      emit("terminal.data", {{"terminalSessionId", session_id}, {"data", utf8_lossy(bytes)}});
      return;
    }
    if (type == "error") {
      record_diagnostic(session, frame.value("message", std::string("terminal error")));
      return;
    }
  }        // 没有 else / 没有 auth_prompt 分支：帧走到这里就被丢掉
```

宿主对 `server_auth_respond` / `server_auth_cancel` 的回答是硬编码的：

```cpp
// native/desktop_host/src/ipc_router.cpp:8766-8792（节选，`⋯` 表示此处省略的行）
  if (method == "server_auth_respond" || method == "server_auth_cancel") {
    // Keyboard-interactive prompts are owned by the C++ PTY worker. Until a PTY
    // registers a challenge there is nothing to accept, which is the correct
    // answer for an idle desktop.
    ⋯
    if (respond) { ⋯校验 responses 为 ≤16 条、每条 ≤4096 字节，否则 INVALID_PARAMS⋯ }
    return {true, {{"accepted", false}}};   // 没有任何分支能返回 true
  }
```

反向输入路径在 `native/src/ssh.cpp:1378-1399` 是**存在且已实现**的（解析 `auth_response`、校验条数与 `expected_auth_responses` 匹配、用完擦除密钥），但宿主侧从未发送过该帧：`native/desktop_host` 全目录内不存在 `auth_prompt` / `auth_response` 字面量。

UI 侧订阅的通道也没有任何生产者（全仓库生产者为 0，见 C-2 扫描表），并且把「什么都不接受」解释为过期：

```tsx
// apps/desktop/src/features/servers/ServerAuthChallengeModal.tsx:76
  // accepted === false 分支
  setError("本次认证挑战已过期或已被处理，请重新连接。");
```

**影响**：目标机启用了 keyboard-interactive（TOTP、短信、RSA SecurID、部分 PAM 策略）时，用户既看不到挑战弹窗（通道无生产者），也无法提交验证码（无通道）。终端连接会停在认证阶段直到 `ssh.cpp:1511` 的 125 秒等待超时，用户只会看到连接失败，且没有任何可执行的补救说明。这可被判定为「一类认证方式完全不可用」，不是降级。

**建议修复**（按成本从低到高，任选其一并同步契约）：
1. 在 `desktop_terminal.cpp` 的 `handle_frame` 增加 `auth_prompt` 分支，转成 `server.auth_challenge` 事件（字段与 `EVENT_SCHEMAS["server.auth_challenge"]` 对齐：`authId`/`prompts`/`expiresAt`），并把 `server_auth_respond` 接到 `TerminalManager::write_frame(session, {{"type","auth_response"},{"responses",...}})`，由 `accepted` 反映写帧是否成功；
2. 或（更小改动、但需明确产品决策）在 UI 侧删除该弹窗与通道，并让连接失败信息明确写出「当前桌面宿主不支持交互式认证，请改用密钥认证」，避免用户等待一个永不出现的弹窗。
无论哪条，`ipc_router.cpp:8767-8769` 的注释都必须改掉 —— 现状是「PTY worker 拥有提示」这句话没有对应的实现。（`native/desktop_host/tests/test_database.cpp:1151-1161` 断言 `accepted:false` 是 idle 桌面的正确回答，这条断言本身在「无人注册挑战」的前提下成立，问题在于**没有任何路径能让它变成 true**，因此不要把它当成测试 bug，而要把它当成本发现的一部分。）

**置信度**：高。

#### C-2 `SUBSCRIBABLE` 的 19 个通道里有 6 个没有任何生产者；另有 3 个**声明名**（本就没有订阅者）同样没有任何生产者，而测试与注释断言它们有

**计数口径**（两类不同，分开算，不合并）：

- **A 类 —— 在 `SUBSCRIBABLE`（= 有 gate 的可订阅通道）内，但没有生产者**：**6 个**。这些是真正的「界面能订阅、后端从不发」的死通道。
- **B 类 —— 在 `DECLARED_WITHOUT_A_SUBSCRIBER`（声明名，UI 本来就没有订阅者）内，且也没有生产者**：**3 个**（`server.connected`、`server.disconnected`、`server.updated`）。该清单共 4 项，第 4 项 `terminal.opened` **有**生产者（`desktop_terminal.cpp:391`）。
- 因此 `EVENT_NAMES`（23 项）中「既无生产者又无订阅者」的名字是 **6 + 3 = 9**，但这 9 个分属上述两类，性质不同（A 类是断线，B 类里 3 个是从未被接线的预留名）。
- 反向对照：`SUBSCRIBABLE` 19 项中**有**生产者的 13 项 —— `agent.started` / `agent.text` / `agent.tool_call` / `agent.tool_result` / `agent.waiting_approval` / `agent.approval_expired` / `agent.completed` / `agent.failed`（Python 侧共 8 项）、`terminal.data` / `terminal.closed`（C++ 侧 2 项）、`mcp.oauth_device_code`、`file.local_dropped`、`file.transfer_updated`（C++ 侧 3 项）。

**位置**：
- `packages/shared/src/schemas/event-vocabulary.test.ts:29-49`（`SUBSCRIBABLE` 19 项）、`:55-60`（`DECLARED_WITHOUT_A_SUBSCRIBER` 4 项）
- `packages/shared/src/schemas/event-vocabulary.test.ts:5-13`（断言 `Rust emits server.updated and terminal.opened`）
- `packages/shared/src/schemas/consistency.ts:133-141`（`server.updated` / `terminal.opened`「on the wire and Rust emits them」）
- 消费者：`apps/desktop/src/features/activity/ActivityFeed.tsx:70`、`apps/desktop/src/features/overview/InfrastructureDashboard.tsx:35`、`apps/desktop/src/features/overview/ServerOverview.tsx:397`、`apps/desktop/src/features/workpages/useWorkpageOperations.ts:48`、`apps/desktop/src/features/investigations/InvestigationsPane.tsx:106`、`apps/desktop/src/features/terminal/TerminalPane.tsx:168`、`apps/desktop/src/features/agent/useAgentRun.ts:205,214`

**证据**：按事件名字面量对 `native/**/*.cpp|hpp`、`agent/src/**/*.py` 逐个穷尽扫描（生产者视角）：

**A 类（在 `SUBSCRIBABLE` 内 = 有 gate、有订阅者，但生产者为 0）—— 6 个：**

| 通道 | native 生产者 | Python 生产者 | UI 订阅者 |
| --- | --- | --- | --- |
| `activity.created` | 0 | 0 | 4 处（见"位置"） |
| `agent.usage` | 0 | 0 | `useAgentRun.ts:214` |
| `agent.thinking` | 0 | 0 | `useAgentRun.ts:205` |
| `server.auth_challenge` | 0 | 0 | `ServerAuthChallengeModal.tsx:26` |
| `terminal.output_lost` | 0 | 0 | `TerminalPane.tsx:168` |
| `investigation.schedule_notification` | 0 | 0 | `InvestigationsPane.tsx:106` |

**B 类（在 `DECLARED_WITHOUT_A_SUBSCRIBER` 内 = 声明名，本就无订阅者）—— 该清单 4 项，其中 3 项生产者为 0、1 项有生产者：**

| 声明名 | native 生产者 | 说明 |
| --- | --- | --- |
| `server.connected` | 0 | 从未接线；但测试/注释断言「Rust emits」 |
| `server.disconnected` | 0 | 同上 |
| `server.updated` | 0 | 同上 |
| `terminal.opened` | 1（`desktop_terminal.cpp:391`） | 唯一的「有生产者」项；无 UI 订阅者 |

**对照（确认扫描没有漏掉生产者）：**

| 通道 | native 生产者 | Python 生产者 | UI 订阅者 |
| --- | --- | --- | --- |
| `terminal.data` / `terminal.closed` | 1 / 1 | 0 | 有 |
| `file.transfer_updated` / `file.local_dropped` | 2 / 2 | 0 | 有 |
| `mcp.oauth_device_code` | 1 | 0 | 有 |
| `agent.started` / `text` / `tool_call` / `tool_result` / `waiting_approval` / `approval_expired` / `completed` / `failed` | 0 | 有（见下） | 有 |

Python 侧实际发出的事件类型只有 8 种（`agent/src/yukinal_agent/sidecar.py:881,905,947,949,959,970,981,1060,1088` 与 `agent/src/yukinal_agent/model_agent.py:794,811`，其中 `agent.completed`/`agent.failed` 各出现在多个分支）：`agent.started` / `text` / `tool_call` / `tool_result` / `waiting_approval` / `approval_expired` / `completed` / `failed`，**不含 `agent.thinking`、`agent.usage`**。C++ 侧的 `sink(...)` / `emit(...)` 调用点只有这些：`ipc_router.cpp:5948`（`file.transfer_updated`）、`:6003`（`file.local_dropped`）、`:8990`（转发 sidecar 的 `type`）、`:9576`（`mcp.oauth_device_code`），以及 `desktop_terminal.cpp:364`（`terminal.closed`）、`:391`（`terminal.opened`）、`:399`（`terminal.data`）。

而 `event-vocabulary.test.ts` 的头部把「差集只有 Rust-only 事件」当作已核实事实：

```ts
// packages/shared/src/schemas/event-vocabulary.test.ts:6-13
 * The overlap is real and intentional — Rust emits `server.updated` and
 * `terminal.opened` and the UI has no subscriber yet — ...
 * The gap is now only Rust-only events (server.updated / terminal.opened). Every
 * agent member has a producer, a channel, and a consumer; `agent.text` and
 * `agent.usage` complete the provider-to-UI path.
```

`:90-112` 的「the agent channels the loop emits are all subscribable」还把生产者清单注释成「transcribed from `apps/agent/src/agent-loop.ts`」——该文件不存在。

同样的自证出现在 `consistency.ts`：

```ts
// packages/shared/src/schemas/consistency.ts:137-141
 * not automatically a defect — `server.updated` and `terminal.opened` are on the
 * wire and Rust emits them, the UI just has no subscriber yet
```

`TerminalPane.tsx:167-171` 的注释把不存在的能力写成已完成：

```tsx
// apps/desktop/src/features/terminal/TerminalPane.tsx:166-171
      // 宿主广播队列溢出时会明确告知丢帧，而不是静默停止转发（性能 P0）。
      subscribeDesktop("terminal.output_lost", (payload) => { ... });
```

`desktop_terminal.cpp` 没有任何队列/溢出/丢帧计数逻辑，`publish_event`（`native/desktop_host/src/main.cpp:67-89`）也是每帧一次 `webview_eval`，不排队也就永不丢帧——通道与「P0 背压」这套说法都没有对应实现。

**影响 —— A 类（6 个死通道，用户可见的功能缺失）**：
- `agent.usage` 无生产者 ⇒ `useAgentRun.ts:214-217` 的 `setTokenUsage` 永不执行（全文件只有 `:363`、`:494` 的置空），`AgentHeader.tsx:115-117` 的独立令牌 HUD 因此永不显示（它要求 `tokenUsage && !contextUsage`，而 `contextUsage` 在 `useAgentRun.ts:269` 的 `agent.completed` 处理里几乎同时被赋值），`:112` 的上下文 tooltip 里那段「本次运行输入/输出 tokens」也永远是空的。用户在运行中拿不到任何令牌用量反馈。
- `agent.thinking` 无生产者 ⇒ 推理过程（`appendReasoningDelta`）永不显示。
- `activity.created` 无生产者 ⇒ `ActivityFeed.tsx:70-78`、`InfrastructureDashboard.tsx:35-40`、`ServerOverview.tsx:397-405`、`useWorkpageOperations.ts:48` 的四个 `setQueryData`/`refresh` 永不触发，活动流只在初次拉取时出现内容；排障时会被误判为「轮询坏了」。
- `terminal.output_lost` 无生产者 ⇒ 终端在高输出下既没有节流也没有丢帧提示，而 `TerminalPane.tsx:166-171` 的注释声称这段能力已经存在（性能 P0）。
- `server.auth_challenge` 无生产者 ⇒ 见 C-1（认证链路不可达）。
- `investigation.schedule_notification` 无生产者 ⇒ `InvestigationsPane.tsx:106-111` 的定时任务通知与三个 `invalidateQueries` 永不触发。

**影响 —— B 类（3 个从未接线的声明名）**：`server.connected` / `server.disconnected` / `server.updated` 没有生产者，本身不会造成功能缺失（UI 没有订阅者），但它们是**测试与注释的事实依据**：`event-vocabulary.test.ts:6-8` 与 `consistency.ts:138-139` 都用「Rust emits `server.updated`」来解释「差集是刻意保留的」。这条依据不成立，因此整个「差集已被审查并接受」的论证链也是空的。`terminal.opened` 是 B 类清单里唯一有生产者的项（`desktop_terminal.cpp:391`），属于「有生产者但无订阅者」——这是第三类状态，当前也被混在 `DECLARED_WITHOUT_A_SUBSCRIBER` 里。

更根本的是：**这些漂移被测试钉成了「正确」**。`event-vocabulary.test.ts:67-80` 的分区断言只校验三张清单的并集等于 `EVENT_NAMES`，不校验任何一条是否真有生产者，所以任何未来的契约加固都会先撞上这些测试。

**建议修复**：
1. 把事件名按**两个独立维度**分档并分别断言，而不是继续用一张 `SUBSCRIBABLE` 混装：
   - 维度一（有没有 gate）：`EVENT_SCHEMAS` 的键集；
   - 维度二（有没有生产者）：从 C++/Python 侧扫描出的集合；
   - 维度三（有没有订阅者）：从 `apps/desktop/src` 扫描出的集合。
   由此自然得到本次的 A 类（有 gate + 有订阅者 + 无生产者）、B 类（有 gate + 无订阅者 + 无生产者）、以及 `terminal.opened` 所在的第三类（有 gate + 无订阅者 + **有**生产者）。引入显式名字（例如 `PRODUCER_LESS_CHANNELS` / `RESERVED_WITHOUT_A_PRODUCER`），让每一档都在测试里可核对。
2. 逐条决策 A 类 6 条：`agent.usage` / `agent.thinking` 要么在 `sidecar.py` 的 `_execute_run` 里补发（用量数据已经在 `result.context` 里有），要么从契约与 UI 中移除；`activity.created` 要么在宿主写 activity 行后 `sink("activity.created", ...)`（`ipc_router.cpp:8838` 的 `record_connection_activity` 就是一个天然发射点），要么让 UI 退回轮询；`terminal.output_lost` 要么补背压与计数，要么删掉通道与那条注释。
3. 把所有「Rust emits …」注释改成可验证的表述或直接删除（见 L-1），别让注释继续承担契约文档的角色。

**置信度**：高（扫描是穷尽的；`agent.thinking`/`agent.usage` 另有 grep 复核，`agent/src` 内不存在这两个字面量）。

---

### High

#### H-1 `agent_status.restart` 与整套自动恢复：契约、UI、fixture 齐全，宿主侧没有生产者也没有 supervisor

**位置**：
- 契约：`packages/shared/src/schemas/ipc.ts:144-159`（`restart: RestartRecordSchema.optional()`）、`packages/shared/src/ipc/index.ts:481-485`
- 消费者：`apps/desktop/src/features/agent/AgentPanel.tsx:117,476`、`apps/desktop/src/features/agent/AgentNotices.tsx:42-54,56-65`、`apps/desktop/src/features/agent/useAgentRun.ts:312-335`、`apps/desktop/src/features/settings/RuntimeSettings.tsx:89-99`
- 宿主：`native/desktop_host/src/ipc_router.cpp:9010-9036`（不回 `restart`）、`native/desktop_host/include/yukinal/desktop_host/sidecar.hpp:63-72`（无该字段）、`native/desktop_host/src/sidecar.cpp:383-415`（只记录退出，无重启/退避）
- fixture/测试：`packages/shared/fixtures/ipc/agent_status_restarted.json`、`apps/desktop/tests/agent-notices-restart.test.tsx`

**证据**：宿主 `agent_status` 的响应对象是显式构造的，字段清单里没有 `restart`，也没有任何分支会产生它：

```cpp
// native/desktop_host/src/ipc_router.cpp:9012-9035（`⋯` 为省略的取值表达式）
    nlohmann::json response = {
        {"running", status.running},
        {"pid", ⋯},
        {"protocolVersion", ⋯},
        {"agentVersion", ⋯},
        {"toolCount", ⋯},
        {"entry", ⋯},
        {"startedAt", ⋯},
        {"lastExit", nullptr},
    };
    if (status.last_exit.has_value()) { response["lastExit"] = {⋯}; }
    return {true, std::move(response)};      // 没有 "restart"
```

`SidecarStatus` 里也确实没有这个字段：

```cpp
// native/desktop_host/include/yukinal/desktop_host/sidecar.hpp:63-72（`⋯` 为省略的字段）
struct SidecarStatus {
  bool running = false;
  std::optional<int> pid;
  ⋯
  std::optional<SidecarExitInfo> last_exit;   // 只有退出记录，没有 restart
};
```

全 `native/` 内 `"restart"` 字面量出现次数为 **0**；`sidecar.cpp` 的 `monitor_process`（`:383-415`）在进程退出后只做 `running = false; last_exit = ...`，没有重启循环、没有退避、没有 `attempt`/`maxAttempts`/`exhausted` 的任何计算。契约注释却把缺失字段的语义归因于 Rust：

```ts
// packages/shared/src/schemas/ipc.ts:153-158
   * Optional rather than nullable: the field is *omitted* when there is nothing to
   * report (`skip_serializing_if` on the Rust side), so the status of an idle supervisor
   * stays byte-identical to the fixture that predates automatic recovery.
   */
  restart: RestartRecordSchema.optional(),
```

**影响**：`RuntimeSettings.tsx:89-99` 的「自动恢复」行永远显示「无」；`AgentNotices.tsx:42-54` 的「重启失败 N/M，自动恢复已停止」永不出现；`useAgentRun.ts:312-335` 那条**专门用来修 `running === true` 但进程已死的窗口**的信号永不触发（其注释明确说轮询 1.5 秒一次、可能整个错过重启窗口），因此在这种情况下 run 会永久停在「运行中」——这正是该 effect 存在的理由。ADR 0010 描述的自动恢复在当前宿主上是**文档存在、实现不存在**。

**建议修复**：二选一并保持一致。要么在 `SidecarClient` 实现有界监督循环（`kMaxRestartAttempts` + 退避 + `exhausted`），把 `SidecarStatus` 扩一个 `std::optional<RestartRecord>` 并在 `ipc_router.cpp:9012` 输出；要么承认自动恢复未实现：删除 `AgentStatusSchema.restart`、`ipc/index.ts:481-485`、`RuntimeSettings.tsx:89-99`、`AgentNotices.tsx:42-65`、`useAgentRun.ts:312-335` 与 `agent_status_restarted.json`，并把 UI 的兜底交回 `sidecarRunning === false`（`useAgentRun.ts:296-310`，那条是真的能工作的）。无论选哪条，`packages/shared/src/schemas/ipc.ts:153-157` 与 `packages/shared/src/ipc/index.ts:480-485` 的「Rust 侧 `skip_serializing_if` / ADR 0010」注释都要同步。

**置信度**：高。

#### H-2 verbatim 转发进 `z.strictObject` 边界：sidecar 多一个字段就把 UI 打崩

**位置**：
- 转发点：`native/desktop_host/src/ipc_router.cpp:9119-9122`（`agent_run_command` 的 `agent_run_start` 等）、`:9656-9658`（`provider_command` 全部 6 条）
- 接收边界：`apps/desktop/src/lib/ipc.ts:113`（`IPC_SCHEMAS[command].response.parse(raw)`）
- 严格 schema：`packages/shared/src/schemas/ipc.ts:435-441`（`agent_run_start` 响应）、`:635`（`provider_test`）

**证据**：

```cpp
// native/desktop_host/src/ipc_router.cpp:9119-9122
  if (method != "agent_approval_respond") {
    try {
      auto& client = ensure_sidecar();
      return {true, client.request(found->second, params, timeout)};   // 原样透传
```

```ts
// packages/shared/src/schemas/ipc.ts:435-441
    response: z.strictObject({
      runId: z.string().min(1),
      started: z.boolean(),
      duplicate: z.boolean().optional(),
      resumed: z.boolean().optional(),
      result: AgentRunResultSchema.optional(),
    }),
```

`strictObject` 对未知键是**报错**而不是剥离，`callDesktop` 会把 ZodError 直接抛给调用方。同一模式适用于 `provider_*`（`:9656-9658` 一行完成转发，`schemas/ipc.ts:521-537` 全是 `strictObject`）。Python 侧 `_run_start` 的返回体是 `sidecar.py:594-646` 手工拼的字典，加字段不需要改任何 TS 契约即可上线。

**影响**：任何一次 sidecar 侧「多加一个诊断字段」的改动，都会让桌面 UI 的发送/供应商设置整块抛异常，而不是忽略该字段。因为两侧没有共同的生成器或投影层，这类破坏**不会在 CI 里被发现**——`ipc.test.ts` 只拿 fixture 对 TS schema 解析（`packages/shared/src/schemas/ipc.test.ts:31-43`），fixture 是手写的，sidecar 的真实返回从不参与。`AgentRunResultSchema`（`schemas/agent.ts:59-70`）里已经有 `optional` 的 `traceId` 并明确写着「an older sidecar build does not [set it]」，说明团队已经知道这条边界的脆弱性，但没有系统性解决。

**建议修复**：在宿主侧对转发响应做**字段投影**（白名单 `only_fields` 风格的 `project_response(command, json)`），只放行契约里声明的键；这比把 TS 全改成 `.passthrough()` 更安全，因为后者会让 UI 处理未经校验的字段。同时给 `agent_run_start`/`provider_*` 加一个「sidecar 返回体 → 契约」的运行期校验点（宿主启动自检或首次调用时记录一次偏差），把漂移变成可观测事件而不是 UI 崩溃。

**置信度**：高。

#### H-3 审批 decision 在宿主入口没有白名单：未知 approvalId 被当成成功，`approve_session` 契约允许但所有后端都拒绝

**位置**：
- 入口：`native/desktop_host/src/ipc_router.cpp:9134-9148`（`valid_request_text(decision, 32)`，无枚举校验）
- 账本：`native/desktop_host/src/desktop_approval.cpp:304-334`，关键是 `:309`（未知 approvalId 返回 `std::nullopt`）与 `:326-330`（`approve_session` 抛错）
- 契约：`packages/shared/src/schemas/permission.ts:140-145`（`decision: z.enum(APPROVAL_DECISIONS)`，含 `approve_session`）
- sidecar：`agent/src/yukinal_agent/sidecar.py:783`（只接受 `approve_once`/`reject`）
- UI 后果：`apps/desktop/src/features/agent/useAgentRun.ts:466-475`

**证据**：宿主只校验长度，不校验取值：

```cpp
// native/desktop_host/src/ipc_router.cpp:9137-9141
  if (!params.contains("approvalId") || !params.contains("runId") ||
      !params.contains("decision") ||
      !valid_request_text(params, "approvalId", 256, true, approval_id) ||
      !valid_request_text(params, "runId", 256, true, approval_run_id) ||
      !valid_request_text(params, "decision", 32, true, decision)) {   // 任意 ≤32 字符
```

`begin_response` 对「账本里没有这张票」返回 `nullopt`，而调用方把 `nullopt` 视为**无事发生**，继续把请求转发给 sidecar：

```cpp
// native/desktop_host/src/desktop_approval.cpp:307-309
  const auto found = impl_->entries.find(approval_id);
  if (found == impl_->entries.end()) return std::nullopt;      // 不是错误
```

```cpp
// native/desktop_host/src/ipc_router.cpp:9152-9158
  const auto finish = [this, &token](bool accepted) {
    if (!token.has_value()) return;                            // 静默 no-op
```

于是 `approvalId` 只要在 sidecar 侧存在，UI 就会拿到 `accepted:true` 并显示「已批准一次」，而宿主账本里根本没有这张票 —— 该 host action 稍后会在 `consume`（`desktop_approval.cpp:359-360`）以 `TicketError::Missing` 失败关闭，用户看到的是「批准了但操作没执行」。同时 `approve_session` 在宿主票上必然抛错（`:326-328`，这条设计是对的：host action 逐次批准），但在非 host 票上会被原样转发，由 `sidecar.py:783` 拒绝，UI 再把它渲染成「审批已失效」（`useAgentRun.ts:468-469`），把后端不支持的一种决策说成用户的批准过期了。

**影响**：三类不一致 —— (a) 账本缺失被当成成功（真正的可观测性缺口）；(b) 契约承认 `approve_session`，但它对 host 票和 sidecar 都不可用；(c) 失败原因被 UI 误译。安全上当前**不能**借此绕过审批（`finish_response` 要求 `token.approve_once` 且过期校验仍在 `:346`），所以不是提权，而是状态机和错误语义的缺陷。

**建议修复**：在 `ipc_router.cpp:9137` 的入口加 `is_one_of(decision, {"approve_once", "approve_session", "reject"})`（与 `APPROVAL_DECISIONS` 同步，并加一条 TS↔C++ 对账断言）；把 `begin_response` 的 `nullopt` 从「静默」改为可区分的返回值（例如 `ResponseToken` 或 `TicketError::Missing`），宿主据此直接回答 `APPROVAL_NOT_TRACKED` 而**不**转发；UI 的 `:466-475` 需要第四个分支区分「已失效」与「已批准但宿主未跟踪」。若产品上确认 `approve_session` 永远不可用，则从 `APPROVAL_DECISIONS` 中移出（这也会连带修正 M-3 的按钮）。

**置信度**：高。

#### H-4 `mcp_server_list` 对运行中的服务器报 `running:false`，且 MCP 状态对象有两个不同形状

**位置**：
- 列表：`native/desktop_host/src/ipc_router.cpp:9186-9214`（`view_for` + `mcp_server_list`）
- 真实状态：`native/desktop_host/src/desktop_mcp.cpp:1138-1156`（`status_for`）、`:1174-1192`（`stop` 的另一个形状）
- 契约：`packages/shared/src/schemas/mcp.ts:113-127`、`:152-157`
- 消费者：`apps/desktop/src/lib/mcp.ts:282-290,303-307,350-358`、`apps/desktop/src/features/settings/McpSettings.tsx:367-376`

**证据**：`mcp_server_list` 完全从数据库配置合成状态，从不查询 supervisor：

```cpp
// native/desktop_host/src/ipc_router.cpp:9186-9204
  const auto view_for = [](const nlohmann::json& config) {
    nlohmann::json view = {
        {"config", config},
        {"status",
         {{"serverId", config.at("id")},
          {"running", false},                 // 常量：永远 false
          {"toolCount", 0},
          {"stderrTail", nlohmann::json::array()},
          {"diagnostics", nlohmann::json::array()}}},
        {"tools", nlohmann::json::array()}};
```

```cpp
// native/desktop_host/src/ipc_router.cpp:9210-9214
      const auto servers = database_->list_mcp_servers().at("servers");
      for (const auto& config : servers) views.push_back(view_for(config));   // 不碰 mcp_
```

`lib/mcp.ts:70` 的每个 action 都在 `onSettled` 里 `invalidateQueries(MCP_SERVERS_QUERY_KEY)`，即**启动成功之后列表立刻重新读一次，然后把刚启动的服务器显示回「未启动」**。真实状态只在 `start`/`stop` 的单次响应里出现（`ipc_router.cpp:9466,9468`），而那个响应只用于 mutation 的返回值，列表不使用它。

同时 `status_for` 与 `stop` 的手写 status 是两个形状：

```cpp
// native/desktop_host/src/desktop_mcp.cpp:1174-1187
  nlohmann::json status = {{"serverId", server_id},
                           {"running", false},
                           {"toolCount", 0},
                           {"stderrTail", nlohmann::json::array()},
                           {"diagnostics", nlohmann::json::array()}};   // 缺 pid/program/startedAt/protocolVersion/serverName/serverVersion
```

`status_for`（`:1138-1156`）虽然字段齐全，但 `pid`/`program`/`startedAt` 是**硬编码 null**。其中 `pid` 是纯粹的空转：子进程句柄在 `desktop_mcp.cpp:983` 确实存进了 `session->pid`，`:1138` 却从不读它。`program` 与 `startedAt` 则连 `Session` 里都没有对应字段（`Session` 结构见 `:303-344`，只有 `pid` 与进程/管道句柄），因此这两个字段是「契约里有、宿主侧连数据都没有」。`lastExit`（契约 `mcp.ts:123`）与 `restart`（`:124`）同样从未出现。消费者却在读这些字段：`lib/mcp.ts:284-288` 用 `restart`/`lastExit` 决定徽标文案，`:303-307` 渲染退出记录，`:350-358` 渲染恢复进度，`McpSettings.tsx:367-376` 用 `pid` 决定是否显示进程号。

**影响**：(a) MCP 列表的状态列系统性错误 —— 运行中的服务器显示「未启动」，用户会重复点击「启动」；(b) `lastExit`/`restart` 的分支永不执行，`mcp_preview_stopped` fixture（`apps/desktop/src/lib/preview-fixtures.ts:252`）里那套「恢复已停止」文案在真实宿主上无法出现；(c) `pid` 永远为 null，`McpSettings.tsx:374-376` 的降级分支成为唯一实际路径。

**建议修复**：让 `mcp_server_list` 在组装 view 时合并 supervisor 的真实状态（给 `McpSupervisor` 加一个只读 `status(server_id)` 查询，避免为列表启动进程）；把 `stop` 的 status 改为复用 `status_for`（`:1171` 的 `start` 已经这么做，`:1183` 应跟上）；`status_for` 里把 `session->pid` 真正填上，并给 `Session` 补 `program`/`started_at` 两个字段（否则 `mcp.ts:117-118` 应从契约中删除）；`lastExit`/`restart` 按 H-1 的同一决策统一处理。注意无需过度设计：`mcp_server_list` 保持只读、不启动进程这一点（`lib/mcp.ts:39` 的注释）是对的，要保留。

**置信度**：高（`view_for` 的常量 `running:false` 与 `status_for` 的 `pid:null` 都是逐行确认的）。

---

### Medium

#### M-1 `HostApprovalLedger::consume` 的等待循环会占用宿主 worker 直到票过期

**位置**：`native/desktop_host/src/desktop_approval.cpp:355-380`；相关上限 `:24`（`kMaxApprovalTtlSeconds = 180`）；worker 池 `native/desktop_host/src/sidecar.cpp:36-42`（`kHostWorkerCount = 4`）

**证据**：

```cpp
// native/desktop_host/src/desktop_approval.cpp:357-371
  std::unique_lock lock(impl_->mutex);
  for (;;) {
    const auto found = impl_->entries.find(request.approval_id);
    if (found == impl_->entries.end()) return TicketError::Missing;
    auto& entry = found->second;
    if (entry.binding.expires_at <= now()) { impl_->entries.erase(found); return TicketError::Expired; }
    switch (entry.phase) {
      case Impl::Phase::Pending: return TicketError::NotApproved;
      case Impl::Phase::Responding:
        impl_->changed.wait_for(lock, std::chrono::seconds(1));   // 每次醒来重新查
        continue;
```

**影响**：循环本身是**有界**的（每轮都重查 `expires_at`，最长 180 秒后返回 `Expired`），所以不是死锁；但它以 1 秒粒度空转，并且是在 sidecar 的 host worker 线程上阻塞（`kHostWorkerCount = 4`）。4 个并发审批等待就能把 host 工具调用通道占满 180 秒，期间其它 host 请求排不上队。定位上是**有界但不可取消**：`consume` 没有接收取消标志（对比 `ipc_router.cpp:8955-8957` 的 `cancel` 参数是被一路传下去的），用户点「停止 run」也不会让这个线程提前退出。

**建议修复**：让 `consume` 不要把 worker 线程绑在等待上 —— 要么把等待改成事件循环/回调式（`begin_response` 时唤醒一次，`consume` 只做一次非阻塞查询），要么给它一个 `std::atomic<bool>` 取消标志并让 `wait_for` 的谓词同时监视它，使用户点「停止 run」或进程关闭时能立即返回 `Missing`/`Expired`；同时把单次等待上限与 `kMaxApprovalTtlSeconds = 180`（`:24`）的关系写进注释，避免以后有人在不知情的情况下调大 TTL。注意不要用「N 秒无响应即判失败」替代 TTL：TTL 内的审批是合法交互，缩短它会把正常用户按下按钮前的思考时间变成失败。

**置信度**：高（有界性已由代码确认；worker 占用的影响是基于 `kHostWorkerCount = 4` 的结构推断，未做运行期测量）。

#### M-2 同一概念（serverId）在四层有四种校验规则，其中一条能被另一条拒绝

**位置**：
- `native/desktop_host/src/ipc_router.cpp:330-333`（`valid_server_id`：`^srv_[a-z0-9]+$`，≤256）
- `native/desktop_host/src/main.cpp:416-419`（workpage：`[A-Za-z0-9_-]+`，≤256）
- `native/desktop_host/src/desktop_approval.cpp:211`（审批绑定：`starts_with("srv_")`，大小写不限）
- `native/desktop_host/src/ipc_router.cpp:9236,9451`（MCP server id：非空、≤160、无控制字符，**不要求 `srv_` 前缀**）

**证据**：

```cpp
// native/desktop_host/src/ipc_router.cpp:330-333
bool valid_server_id(const std::string& value) {
  static const std::regex pattern(R"(^srv_[a-z0-9]+$)");
  return value.size() <= 256 && std::regex_match(value, pattern);
}
```

```cpp
// native/desktop_host/src/main.cpp:416-419
      if ((kind != "files" && kind != "terminal" && kind != "logs" && kind != "services") ||
          server_id.empty() || server_id.size() > 256 ||
          !std::regex_match(server_id, std::regex("[A-Za-z0-9_-]+"))) {
```

`terminal_open` 走 `valid_server_id`（`ipc_router.cpp:8864`），workpage 走宽松正则，审批账本只查前缀。三者的接受集合互不包含：`srv_ABC` 通过 workpage 与审批（`starts_with("srv_")` 不管大小写），被 `terminal_open` 的 `^srv_[a-z0-9]+$` 拒绝；`srv_` 通过前两者，被第三条拒绝（`+` 要求至少一个字符）；反过来 MCP 的 `serverId` 完全不要求 `srv_` 前缀，所以 `mcp_server_start` 能接受一个 `terminal_open` 必然拒绝的 id。

**影响**：这不是提权路径（四条都在宿主侧执行、都要求调用者已经通过 UI 的 typed 边界），但它是**同类校验分散**的典型：某个入口的宽松校验会成为其它入口的信任前提，任何一处放宽都会以「另一个入口没同步」的形式静默扩大攻击面。同时错误信息不一致，排障时难以判断到底哪一层拒绝了一个 id。

**建议修复**：把 `valid_server_id`、workpage 校验、审批绑定收敛到一个共享实现（每个实体一个函数，例如 `valid_ssh_server_id` / `valid_mcp_server_id`），并加一条测试断言「审批绑定的 serverId 集合 ⊆ `valid_ssh_server_id` 通过的集合」。审批绑定应直接复用同一个正则，而不是 `starts_with`。

**置信度**：高。

#### M-3 `isSessionGrantable` 零生产调用者：`approve_session` 在真实宿主上从不渲染，一旦渲染又必然失败

**位置**：`packages/shared/src/types/risk.ts:90-101`（唯一调用者是 `risk.test.ts:32,42,43`）；`agent/src/yukinal_agent/sidecar.py:1076`（硬编码 `sessionGrantable: False`）；`apps/desktop/src/features/agent/AgentEntryView.tsx:151-153,164-168`；`apps/desktop/tests/agent-approval.test.tsx:47-56`

**证据**：

```ts
// apps/desktop/src/features/agent/AgentEntryView.tsx:164-168
              {entry.approval.sessionGrantable !== false ? (
                <button type="button" ... onClick={() => void onApproval(entry.approval, "approve_session")}>
                  本次运行批准
                </button>
              ) : null}
```

判定规则本身存在且完整，但没人调用：

```ts
// packages/shared/src/types/risk.ts:95-100
  if (decision.finalRisk === "critical") return false;
  if (decision.tier !== "dangerous") return true;
  return (decision.target.host === "remote" &&
    SESSION_GRANTABLE_DANGEROUS_ENVIRONMENTS.includes(decision.target.environment));
```

生产侧硬编码：

```py
# agent/src/yukinal_agent/sidecar.py:1076
                "sessionGrantable": False,
```

**影响**：当前 sidecar 硬编码 `false`，所以真实宿主上这个按钮**从不渲染** —— 也就是说「本次运行批准」这套交互在正常路径上是死代码。而在 `sessionGrantable` 缺失（旧 sidecar、或任何绕过 `sidecar.py:1076` 的生产者）时 UI 会渲染它，`sidecar.py:783` 又只接受 `approve_once`/`reject` ⇒ 返回 `accepted:false` ⇒ UI 显示「审批已失效」（`useAgentRun.ts:468-469`），既不是用户点了拒绝，也不是批准过期。ADR 0072 里分级授权（`development`/`staging` 上 high 可记住、`critical` 永不可记住）这套规则在运行时完全没有生效路径。

**建议修复**：把 `isSessionGrantable` 接进真正产生 `sessionGrantable` 的地方 —— Python 侧 `_await_approval`（`sidecar.py:1063-1079`）已经有 `toolName`/`target`/风险等级（`factsSummary` 里写着 `Risk level:`），把 tier/environment 一起算出来即可；TS 侧作为同一规则的镜像保留。若确定短期不做 session grant，则删除按钮与 `AgentEntryView.tsx:151-153` 的说明以外的一切（含 `agent-approval.test.tsx:47-56` 那条「缺失就显示按钮」的断言），并让 `APPROVAL_DECISIONS` 与 `sidecar.py:783` 的单侧清单对齐。

**置信度**：高（调用者扫描为穷尽：全仓库 `isSessionGrantable` 仅出现在定义与 `risk.test.ts`）。

#### M-4 跨语言一致性只由 TS 单侧断言，且断言强度低于注释声明

**位置**：`packages/shared/src/schemas/ipc.test.ts:1-7,25-43`；`packages/shared/src/schemas/consistency.ts:146-148,224-408,426-432`

**证据**：

```ts
// packages/shared/src/schemas/ipc.test.ts:1-7
 * Contract tests for the IPC command schemas (one side of the pair; the other lives
 * in `crates/core/src/ipc.rs` and asserts the *same* fixtures against serde output).
```

```ts
// packages/shared/src/schemas/ipc.test.ts:32-33
  const commands = Object.keys(IPC_SCHEMAS) as IpcCommandName[];
  assert.ok(commands.length >= 14, "the command map must not silently shrink");
```

实际规模是 86 个 `IPC_COMMANDS`（`packages/shared/src/ipc/index.ts:99-240`）与 82 个 `IPC_SCHEMAS`（`packages/shared/src/schemas/ipc.ts:227-743`），下界 14 起不到防收缩作用。`consistency.ts:426-432` 的 `_EveryCommandHasAParamsContract` / `_EveryCommandHasAResponseContract` 只把 `IPC_SCHEMAS` 与 `IpcCommandMap` 互相对齐，**从不涉及 C++**。而 `native/` 全目录对 `packages/shared/fixtures` 的引用次数为 **0** —— fixture 只有 TS 一侧在消费（`ipc.test.ts`、`version.test.ts`）。作为对比，宿主侧的 86 个命令字面量是齐的（穷尽扫描：86/86 在 `ipc_router.cpp` 内出现），说明名字对齐是人工维持的，没有机器校验。

**影响**：跨语言契约（本次审查的核心假设）实际上没有守护者。fixture 被描述为「双方都必须接受/发出的单一 JSON」，但只有一方在读它；`>= 14` 让「命令表静默收缩」这句注释失去意义；参数/响应契约只做 TS 内部自洽。

**建议修复**：
1. `commands.length >= 14` 改为 `assert.equal(commands.length, 82)`（或改用显式快照 + 允许注释化增减），把「静默收缩」真正挡住；
2. 让宿主侧消费同一批 fixture：`native/desktop_host/tests` 里加一个测试，用 `ipc_router.cpp` 的校验函数逐个解析 `packages/shared/fixtures/ipc/*.json`（至少 params 侧），任何新命令必须同时补 fixture 与 C++ 校验；
3. 把 `ipc.test.ts:1-7` 的 Rust 对偶描述改成事实（见 L-1），并让 `consistency.ts` 的对账目标包含 C++ 侧导出的一份名字清单。

**置信度**：高。

#### M-5 可变长列表使用 index 作为 React key

**位置**：`apps/desktop/src/features/agent/AgentFeed.tsx:55`、`apps/desktop/src/features/logs/LogsPane.tsx:89`、`apps/desktop/src/components/MarkdownText.tsx:34,69,99,107,109,145,147,158,170,173,179,185,191,200,208,211,216`、`apps/desktop/src/features/workpages/WorkpageArtifactView.tsx:42,62`、`apps/desktop/src/features/overview/ServerNetworkSection.tsx:9`、`apps/desktop/src/features/agent/AgentHistoryRow.tsx:37,39`、`apps/desktop/src/features/files/RemoteFilesPane.tsx:254`

**证据**：

```tsx
// apps/desktop/src/features/logs/LogsPane.tsx:88-89
          {response.lines.map((line, index) => (
            <li className="log-row" key={`${index}:${line.text}`}>
```

```tsx
// apps/desktop/src/features/agent/AgentFeed.tsx:53-55
        entries.map((entry, index) => (
          <AgentEntryView
            key={index}
```

**影响**：`LogsPane` 的列表在每次 `refetch` 时整体重建（`logsQuery.data` 来自一次 `callDesktop(IPC_COMMANDS.serverLogs, ...)`，见 `LogsPane.tsx:42`），行内容随远端日志增长而**整体位移**：同一行文本在第 N 与第 N+1 次读取中的 index 不同，重复文本（空行、`Waiting…`）无法区分，于是 React 会复用错节点而不是移动它们。`AgentFeed` 的条目持续追加，`transcript.ts` 的 `MAX_TRANSCRIPT_ENTRIES = 500` 只在超限时从头部截断，一旦发生截断所有 index 前移一位，整段子树重新挂载（展开状态与滚动位置随之丢失）。`MarkdownText` 的解析结果在一次渲染内稳定，风险低得多，但同一文件里 `key={index}`（`:99`）与 `key={rowIndex}`/`key={cellIndex}`（`:107,109`）两种写法并存，说明约定不统一。`WorkpageArtifactView`（`:42,62`）与 `ServerNetworkSection`（`:9`）同属按位置渲染的清单。

**建议修复**：为会增长/重排的列表生成稳定 id —— `transcript.ts` 的条目已有 `callId`（`transcript.ts:112-130` 就是按它配对的），日志可用后端行号或一个随响应递增的序列号；并把「跨渲染稳定的列表 key，不用 index」写进 `apps/desktop/design`。`MarkdownText` 这类一次解析、一次渲染的静态树可保留 index，但要在设计文档里显式标注为例外，避免它成为其它地方的先例。

**置信度**：中（模式与渲染语义已确认；具体视觉后果需要运行期复现）。

---

### Low

#### L-1 57 个 ts/tsx 文件共 248 处仍在引用已不存在的 Tauri/Rust 路径，其中若干处在充当契约文档

**位置**（示例，非穷尽）：`packages/shared/src/ipc/index.ts:1-7`、`packages/shared/src/events/index.ts:1-10`、`packages/agent-sdk/src/transport.ts:4-5`、`packages/shared/src/schemas/agent.ts:88-90`、`packages/shared/src/schemas/ipc.test.ts:1-7`、`packages/shared/src/schemas/event-vocabulary.test.ts:5-13,91`、`apps/desktop/src/lib/ipc.ts:219`、`apps/desktop/src/lib/mcp.ts:295-296`、`apps/desktop/src/features/settings/McpSettings.tsx:55`

**证据**：

```ts
// packages/shared/src/ipc/index.ts:2-5
 * Tauri IPC contract shared by React and the Rust command layer.
 * React may only call these commands and listen to these events. Rust mirrors the
 * names 1:1; the agent sidecar is reached through `@yukinal/agent-sdk`, never here.
```

```ts
// packages/agent-sdk/src/transport.ts:4-5
 * The client never touches child_process or WebSocket directly: Tauri/Rust owns
 * process lifecycle, and tests need a loopback.
```

已核实不存在的路径：`crates/`、`apps/desktop/src-tauri/`、`apps/desktop/src-tauri/src/commands/mod.rs`、`apps/agent/src/agent-loop.ts`。`packages/agent-sdk` 也没有生产导入者（只有 `scripts/check.mjs:68` 构建它）。

**影响**：审查者与后来的开发者会被引导去核对一个不存在的第二实现，正如 H-1/C-2 里的注释那样把缺失能力描述成已实现。这是本次审查里所有「注释与实现不一致」问题的共同根因。

**建议修复**：一次机械替换：`Tauri IPC contract` → `Desktop host IPC contract`、`Rust mirrors the names 1:1` → `native/desktop_host/src/ipc_router.cpp mirrors the names 1:1`，并删掉/改写所有「Rust emits …」「owned by … Rust」式断言（能验证的换成 `file:line`，不能验证的删掉）。

**置信度**：高（计数与缺失路径均已核实）。

#### L-2 `tauriEventName` 这个纯 `.`→`:` 替换函数仍以 Tauri 命名，且是唯一的通道名映射

**位置**：`packages/shared/src/events/index.ts:53-55`；被断言于 `packages/shared/src/schemas/event-vocabulary.test.ts:82-88`

**证据**：

```ts
// packages/shared/src/events/index.ts:53-55
export function tauriEventName(name: string): string {
  return name.replace(/\./g, ":");
}
```

**影响**：功能正确（`:82-88` 的碰撞断言是好的），只是命名误导 —— 该映射实际由 `native/desktop_host/src/main.cpp:70-71` 的 `dispatchEvent(event, payload)` 消费，与 Tauri 无关。

**建议修复**：重命名为 `channelEventName`（或 `hostEventName`），保留 `:82-88` 的碰撞测试。

**置信度**：高。

#### L-3 fixture 与 schema 的存在性依赖不显式：`ipc.test.ts` 靠文件名匹配，新增命令漏 fixture 时的失败信息不指向动作

**位置**：`packages/shared/src/schemas/ipc.test.ts:27-42`

**证据**：

```ts
// packages/shared/src/schemas/ipc.test.ts:34-36
  for (const command of commands) {
    const payload = fixture(command);            // 文件不存在 → ENOENT
    const parsed = IPC_SCHEMAS[command].response.safeParse(payload);
```

**影响**：不漏检（缺 fixture 会 ENOENT 失败），但失败信息是文件系统错误而不是「命令 X 缺少 fixture」。属于体验问题。

**建议修复**：在 `fixture()` 里 `existsSync` 后抛出带命令名与期望路径的断言信息。

**置信度**：高。

---

## 已排查但未发现问题的高风险区域

以下区域按「最容易被怀疑出问题」的优先级逐个读过，**未发现缺陷**，列出以便后续审查不再重复投入：

1. **XSS / 动态代码执行**：`apps/desktop/src` 全目录对 `dangerouslySetInnerHTML|innerHTML|eval(|new Function|document.write` 的扫描只有 2 处命中，且两处都是说明性注释（`components/MarkdownText.tsx:10`、`lib/markdown.ts:11`），没有任何真实调用点；`lib/markdown.ts:5-12` 明确记录了「解析器只产出数据结构、渲染层只创建自有元素」的设计约束，`MarkdownText.tsx` 走的是 React 元素树。
2. **凭据泄漏**：`localStorage` 的唯一使用点是 `agent-nist` 相关 hook，只存角色（`yukinal.agent-nist.v1`），不含密钥；宿主侧 `IpcSecretParamScrubber`（`ipc_router.cpp:876-893`）与 `NetworkProxyCredentialScrubber`（`:858-874`）在 `server_add/update/delete`、`network_proxy_save` 路径上把 `authentication.password/privateKeyPem/passphrase` 与 `input.credential` 清零（析构函数里用 `volatile char*` 写零，不是 `memset` 被优化掉的写法）；`agent.logs` 的环形缓冲只存两类内容 —— 宿主自己写的固定文案（`append_log` 调用点仅 `sidecar.cpp:175`、`:186`）与 sidecar 的 stderr 按行截断后的原样转发（`append_stderr_chunk`，`:143-161`，单行上限 `kLogLineBytes = 4000`）。后者是设计选择（stderr 是排障信息的主要来源），且凭据不经过 sidecar 的 stdout/stderr —— API key 走 `host.credentials.*`（`ipc_router.cpp:10366-10393`）进系统凭据库，因此未判为缺陷；但它是「如果 Python 侧哪天开始把密钥写进日志就会泄漏」的路径，值得在 `docs/security.md` 里记一条约束。
3. **SQL 注入**：UI 触达的所有 SQL 都走绑定参数；动态拼接只出现在 `write_server_row`（`database.cpp:1009-1051`）与 `insert_identity_row`（`:1053-1087`），列名取自 `table_columns` 常量集合，**不是**用户输入。
4. **迁移安全**：`create_pre_migration_backup`（`database.cpp:710-739`）用 `VACUUM INTO` + 清单（`sourceVersion`/`targetVersion`/`sha256`/`bytes`/`createdAt`）；`apply_schema_migrations`（`:741-767`）每个版本一个事务 + `PRAGMA user_version=N`，并在当前版本高于支持版本时直接拒绝（`:744`）；`DesktopDatabase` 构造函数（`:1365-1430`）按顺序设置 PRAGMA（`:1398-1400`）、备份（`:1403-1406`）、失败时拷贝回滚并清理 `-wal`/`-shm`（`:1413-1428`）。`journal_mode=WAL`、`foreign_keys=ON`、`trusted_schema=OFF`、`SQLITE_DBCONFIG_DEFENSIVE` 均在位。
5. **React 生命周期**：`WindowFrame.tsx`、`AppShell.tsx`、`hooks/usePresence.ts`（清理在 `:63-76`）、`hooks/useDismissOnOutsidePointer.ts`、`ServerAuthChallengeModal.tsx:36-58`（`requestAnimationFrame` 与 `setTimeout` 都在 cleanup 里取消）、`lib/ipc.ts` 的 `subscribeDesktop`（`:239-257`，含 disposed 标志、`ready` promise 与迟到订阅的拆除）均正确释放。
6. **`transcript.ts` 的状态归约**：`appendEntries`（`:70-72`）、`appendTextDelta`（`:78-85`）、`settleAssistantText`（`:98-106`）、`settleToolResult`（`:123-130` 按 `callId` 配对）在乱序/重复事件下行为正确，`MAX_TRANSCRIPT_ENTRIES = 500` 的截断不破坏配对。
7. **命令名覆盖（单向）**：86 个 `IPC_COMMANDS` 全部能在 `ipc_router.cpp` 中找到对应字面量（86/86，无遗漏）。反向（宿主是否存在契约外的命令名）未做断言，因为宿主同时承载 `host.*` 内部方法，两者本来就不同集合。
8. **窗口外壳契约**：`packages/shared/src/window-shell.ts:5-26` 与 `native/desktop_host/src/window_context.cpp:23-57`（4 字段 + 6 个 section 名）、`windows_window_frame.cpp:351-393`（`update_caption_surface` 恰好 7 字段）一致。
9. **本地 UI 服务器边界**：`main.cpp:156-182` 的 CSP（`default-src 'self'`、`object-src 'none'`、`frame-ancestors 'none'`）+ `Host`/`Origin` 校验 + 仅允许 `GET`/`HEAD`；`valid_dev_url`（`:221-231`）把 `--dev-url` 限制在 `http://127.0.0.1:<port>`。
10. **SSH 认证响应解析（协议层实现完备）**：`native/src/ssh.cpp:1378-1399` 对 `auth_response` 做条数（≤16）、类型、长度（≤4096）与「与期望条数一致」校验，失败即 `wipe` 并标记流无效；`terminal_keyboard_interactive_callback`（`:1454-1547`）在成功与失败两条退出路径上都擦除密钥（`:1534`、`:1546`），`wait_for` 有 125 秒上限（`:1511`）。**实测依据（与 C-1 不矛盾的原因）**：这条解析路径虽然存在且完整，但宿主侧从未发送过该帧 —— 对 `native/desktop_host` 全目录扫描 `auth_prompt` 与 `auth_response` 两个字面量，命中数为 **0**（`grep -r` 结果为 "No matches found"）。也就是说协议层（`native/src/ssh.cpp` 的输入队列与擦除逻辑）是完备的，缺的是宿主接线（`desktop_terminal.cpp` 无 `auth_prompt` 分支、`ipc_router.cpp:8792` 无条件 `accepted:false`）。这里的「未发现问题」只针对校验与密钥擦除本身，不针对端到端可达性 —— 后者是 C-1。
11. **外链打开**：`ipc_router.cpp:10490-10514` 的 `is_external_url` 只放行 `http/https/mailto` 且拒绝空主机，`open_external_url`（`:10516`）先校验再 `ShellExecuteW`；UI 侧 `lib/external.ts:4-11` 是同一条规则。
12. **`packages/provider-sdk/src/name-index.ts`**：`createProviderNameIndex`（`:32-70`）在重名时抛错（`:40-44`），索引构建与查询无隐藏状态。

---

## 建议新增的测试

按「能挡住本次发现」排序，前 5 条建议优先：

1. **跨语言 fixture 对账测试**（挡 H-2、H-4、M-4）：在 `native/desktop_host/tests` 新增一个用例，逐个读取 `packages/shared/fixtures/ipc/*.json` 并喂给 `IpcRouter` 的真实校验/投影函数；任何新增的 sidecar 响应字段在宿主侧投影后必须仍在契约内，否则测试失败。这条同时把「fixture 只有一侧在消费」变成双方约束。
2. **事件生产者/消费者对账测试**（挡 C-2、H-1）：在 `packages/shared/src/schemas/event-vocabulary.test.ts` 里把 `SUBSCRIBABLE` 拆成三档（有生产者有订阅者 / 有生产者无订阅者 / 无生产者），并对「有生产者」一档声明一个可核对的宿主侧锚点（`file:line` 或其等价清单）；同时加一条脚本级测试，扫描 `native/**/*.cpp|hpp` 与 `agent/src/**/*.py` 的字面量集合，断言与清单一致。这样 C-2 那类漂移会在 CI 出现而不是在注释里被断言为正确。
3. **SSH 交互式认证的端到端契约测试**（挡 C-1）：在 `native/desktop_host/tests` 里用一个假的 PTY 帧流（直接调用 `TerminalManager` 的帧解析路径或注入 `auth_prompt` 帧），断言「`auth_prompt` 帧 ⇒ 产生 `server.auth_challenge` 事件 ⇒ `server_auth_respond` 返回 `accepted:true` ⇒ 向 worker 写出的帧是 `auth_response`」。并把 `test_database.cpp:1151-1161` 的 `accepted:false` 断言改成「在无挑战注册时」的条件断言，明确它只覆盖 idle 分支。
4. **`agent_status` 的 supervisor 语义测试**（挡 H-1）：断言在「进程退出并成功重启」与「重启次数耗尽」两种情形下 `agent_status` 的 `restart` 字段取值，并把 `agent_status_restarted.json` 作为宿主侧的输出 fixture（当前它只是 TS 的解析输入）。若产品决定不做自动恢复，这条测试应改为断言「响应中**不存在** `restart` 键」并要求删除契约字段。
5. **审批状态机的穷尽测试**（挡 H-3、M-1）：覆盖 (a) 未知 `approvalId`（宿主账本无票）不得返回成功；(b) `decision` 不在白名单时在宿主入口被拒；(c) `approve_session` 在 host 票上被拒且 UI 展示正确原因；(d) `consume` 在取消标志置位时立即返回而不是等满 TTL。
6. **`mcp_server_list` 与 supervisor 的一致性测试**（挡 H-4）：启动一个（桩）MCP 会话后调用 `mcp_server_list`，断言该服务器的 `status.running` 为 `true` 且 `pid` 与 `start` 响应一致；再断言 `mcp_server_stop` 与 `mcp_server_start` 返回的 `status` 字段集合相同。
7. **命令表规模与校验规则收敛测试**（挡 M-2、M-4）：`assert.equal(Object.keys(IPC_SCHEMAS).length, 82)` 与 `IPC_COMMANDS` 数量断言；加一条「所有接受 `serverId` 的入口共用同一校验函数」的属性测试（对若干个边界值，如 `srv_ABC`、`srv_`、`srv_a-b`、超长 id，断言各入口的接受/拒绝集合一致）。
8. **列表渲染的稳定性测试**（挡 M-5）：对 `LogsPane` 的滚动窗口与 `AgentFeed` 的截断分别构造「重复行 + 追加」序列，断言 DOM 节点身份在追加后保持（例如断言输入框/展开状态不被重置，这正是 index key 的真实症状）。
