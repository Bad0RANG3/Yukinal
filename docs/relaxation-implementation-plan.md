# 限制放宽与完善：实施手册

> 内部工程文档，供实施者（人或 AI）直接照做。它描述**要做的改动**，不代表已经实现；完成后以 [当前限制](./limitations.md#当前限制) 为准。
> 日期：2026-09-28。基线提交：`acbffd7`。基线门禁：`pnpm test` 全绿（agent 259 项），`cargo check --workspace --all-targets` 通过。

## 0. 总则（所有工作包都必须遵守）

1. **五个工作包互相独立**，可以并行给不同实施者；但 §7 的文档/ADR 统一最后做，工作包里**不要**改 `docs/limitations.md`、`docs/adr.md`、`docs/changelog.md`、`docs/boundaries/*.md`，只在完成报告里写一段中文行为说明。
2. 本项目的规矩：放宽授权边界**必须新增 ADR**（ADR 0009 原文：“放宽它需要一条新的 ADR 而不是一次配置改动”）。ADR 编号预分配见 §7。
3. 每一条放宽都要同时改**所有副本**：Rust 常量、TS 常量/Zod schema、UI 文案、Agent 工具 description、测试里钉死的数字。本手册给出了已知副本位置，但实施者仍须 `grep` 旧数字确认没有遗漏（很多 Rust 检查是裸字面量）。
4. 失败关闭原则不变：查不到、解析不了、旧版本对端没发字段 → 走旧的、更严格的路径。
5. 不使用真实服务器/Provider/第三方 MCP；只做本地编译、单测、fixture、回环。
6. 完成前每个工作包必须跑：
   ```bash
   pnpm build:libs
   pnpm typecheck
   pnpm test
   cargo fmt --all --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace -- --test-threads=1
   ```
   最后由集成者跑一次完整 `pnpm check`（它包含上面全部 + 文档卫生、密钥扫描、打包契约、sidecar 冒烟）。
7. 分支建议：`relax/wp1-session-grant`、`relax/wp2-auto`、`relax/wp3-mcp-trust`、`relax/wp4-limits`、`relax/wp5-backup-rotation`。可能冲突的文件已在各包“冲突提示”里列出。

---

## WP1：high 风险动作的“本次运行批准”（ADR 0072）

### 目标
过去 `dangerous` 档位（`high`/`critical`）永远不能被会话授权记住。放宽为：
- **`high` + 远程目标 + development/staging** → 用户点“本次运行批准”后，**同一工具 + 同一目标 + 同一输入指纹**在本次运行内不再询问。
- `critical`：任何环境都不能记住（不变）。
- 生产、unknown、本机（`host: "local"`）上的 dangerous 档位：仍逐次批准（不变）。
- 计划步骤 `requiresApproval: true` 可以被**用户的会话授权**满足（它本身就是用户对该精确动作的点击）；policy/agent 自动批准仍不能满足它。
- 审批卡片只在授权确实会被记住时显示“本次运行批准”按钮，否则显示说明文字（修复旧 UX 谎言：以前按钮对 dangerous 也显示，但引擎静默忽略）。

### 当前状态：**大部分已在工作树里完成（未提交）**，实施者从这里接手
已完成的改动：

| 文件 | 改动 |
|---|---|
| `packages/shared/src/types/risk.ts` | 新增 `SESSION_GRANTABLE_DANGEROUS_ENVIRONMENTS = ["development","staging"]` 与 `isSessionGrantable({tier, finalRisk, target:{host, environment}})`：critical→false；非 dangerous→true；dangerous→`host==="remote"` 且环境在列表中 |
| `apps/agent/src/permissions/permission-engine.ts` | 会话授权分支改用 `isSessionGrantable({tier, finalRisk, target})`；`grantSession` 改为 `if (!isSessionGrantable(decision)) return;`；注释已更新 |
| `apps/agent/src/tools/registry.ts` `checkTicket` | dangerous 档位允许 `session_auto` 当且仅当 `isSessionGrantable(decision)`；另加一条：`session_auto` 而不可授权 → `denied_by_policy` |
| `apps/agent/src/runtime/agent-loop.ts` | ① `planCheck.requiresApproval && outcome==="auto"` 降级为 ask 的条件增加 `&& decision.approvedBy !== "user"`；② 构造 `ApprovalRequest` 时加 `sessionGrantable: isSessionGrantable(decision)`；import 已加 |
| `packages/shared/src/types/chat.ts` | `ApprovalRequest` 增加可选 `sessionGrantable?: boolean` |
| `packages/shared/src/schemas/agent.ts` | `ApprovalRequestSchema` 增加 `sessionGrantable: z.boolean().optional()` |
| `apps/desktop/src/features/agent/AgentEntryView.tsx` | `sessionGrantable !== false` 才渲染“本次运行批准”；`=== false` 时显示 `<p className="approval-note">此操作每次都需要单独批准，不能记住到本次运行。</p>` |
| `apps/desktop/src/styles/workspace-motion.css` | 新增 `.approval-note` |
| `apps/agent/src/permissions/permission-engine.test.ts` | 旧测试 “a session grant never covers the dangerous tier…” 已替换为两条：staging 上 high 精确授权可记住（换输入/clearGrants 后重新询问）；production/unknown/critical 不可记住 |

### 剩余工作
1. **`apps/agent/src/tools/registry.test.ts`**（约 427–480 行）：
   - “a session grant on a dangerous-tier target asks…”（production）保持不变，应仍通过。
   - “a session grant never covers an intrinsically dangerous tool” 使用 `{host:"local", environment:"development"}`：因为本机被排除，仍应通过；**把标题改成** “…on the local machine”，并**新增**一条：远程 staging 上 `risk:"high"` 的工具，`grantSession` 后第二次 `evaluate` 为 `auto/user`，用 `{kind:"session_auto"}` 票据 `registry.execute` 成功；再构造一个 production 的伪造 `session_auto` 票据 → `denied_by_policy`。
2. **`apps/agent/src/runtime/agent-loop.test.ts`**：新增一条：带 durable task 的运行，planCheck 返回 `requiresApproval: true`，同一 high 动作第一次 `approve_session`，第二次同输入调用**不再发 `agent.waiting_approval`**；换输入则再次询问。参考该文件 ~348、~480、~788 行已有的 approval 测试写法。另加断言：`agent.waiting_approval` 事件里 `approval.sessionGrantable` 对 critical（MCP）为 `false`、对 staging write 为 `true`。
3. **`packages/shared` 测试**：`schemas/*.test.ts` 里给 `agent.waiting_approval` 事件加一个带 `sessionGrantable` 的合法样例，以及 `sessionGrantable: "yes"` 被拒绝的样例；给 `isSessionGrantable` 写一组表驱动单测（critical×各环境、high×local/remote×各环境、medium）。
4. **桌面 UI 测试**：在 `apps/desktop/tests/` 找渲染 `AgentEntryView` 审批卡片的测试（`grep -rn "本次运行批准" apps/desktop/tests`），补：`sessionGrantable:false` 时没有该按钮且出现说明；缺省时按钮存在。
5. **宿主侧**：已确认 Rust 不解析 `ApprovalRequest` 字段（`grep factsSummary` 在 `.rs` 中无结果），无需改 Rust。若 `packages/shared/fixtures/ipc/` 中有 waiting_approval 相关 fixture，可选择加上字段。
6. 行为说明段落（供 §7）：会话授权现在可覆盖 dev/staging 远程目标上的 high 动作，严格绑定输入指纹、只在本次运行有效；critical 与生产/未知/本机上的 dangerous 仍逐次批准。

### 验收
- `pnpm --filter @yukinal/agent test`、`pnpm --filter @yukinal/shared test`、`pnpm --filter @yukinal/desktop test` 全绿。
- 手工推演：staging `docker.restart {container:"api"}` 批准本次运行 → 同输入第二次自动、审计 `approvedBy:"user"`；`{container:"db"}` 再问；production 同动作按钮不出现。

### 冲突提示
WP2 也改 `agent-loop.ts` 的 prompt 无关区域，冲突小；WP3 改 `apps/agent/src/mcp/tool.ts`，不冲突。

---

## WP2：受限 auto 扩面（ADR 0073）

### 现状（精确位置）
- **Agent 侧委托**：`apps/agent/src/permissions/permission-engine.ts`，`agentMayAutoApprove = tier === "write" && env ∈ {development, staging}`；`registry.ts` `checkTicket` 里对 `agent_auto` 有同样的硬校验（“Agent auto approval is limited to write-tier development and staging targets”）。
- **宿主侧计划校验**：`apps/desktop/src-tauri/src/commands/host/plan.rs`
  - ~142 行：`step.kind == Action && !step.requires_approval && !task_allows_auto_medium_action(&task, step)` → `denied_by_policy`，错误文案 “action step … may omit approval only for an auto executable goal on a remote development or staging target, and only at medium risk”。
  - ~1055 行 `task_allows_auto_medium_action`：要求 `permission_mode==Auto && mode==Goal && automation_level==Execute && target.host==Remote && env∈{Development,Staging} && risk==Medium && allowed_tools==[FILESYSTEM_BACKUP 或 FILESYSTEM_EDIT]`（单工具）。
  - ~1033 行：High/Critical 步骤必须 `requires_approval`（**保持不变**）。
- **任务 prompt**：`apps/desktop/src-tauri/src/commands/investigation/policy.rs` ~163 行 `autonomous_task_prompt` 中受限 auto 的中文说明。
- **UI**：`apps/desktop/src/features/investigations/auto-delegation.ts`、`apps/desktop/src/features/agent/run-policy.ts`（展示/解释委托范围的文案与判断）。
- **Playbook**：`apps/agent/src/tools/builtin/investigation-playbook.ts` 中 `config_edit` 等模板 `requiresApproval: false`（~181–246 行）。
- 常量：`apps/desktop/src-tauri/src/commands/host.rs` ~100–118 行（`FILESYSTEM_WRITE`、`FILESYSTEM_BACKUP_CLEANUP` 等）。

### 改动
1. **宿主 `task_allows_auto_medium_action`** 扩面：
   - 允许的单工具集合从 `{backup, edit}` 扩到 `{FILESYSTEM_BACKUP, FILESYSTEM_EDIT, FILESYSTEM_WRITE}`。**不加** `FILESYSTEM_BACKUP_CLEANUP`（删除类，保持逐项批准）、`FILESYSTEM_RESTORE`、重启、包安装（都是 high）。
   - 也允许 `allowed_tools.len() == 2` 且恰为 `{FILESYSTEM_BACKUP, FILESYSTEM_EDIT}` 或 `{FILESYSTEM_BACKUP, FILESYSTEM_WRITE}`（先备份后改写的常见组合）；其余组合仍拒绝。
   - 其余条件（Auto + Goal + Execute + Remote + dev/staging + Medium）**不变**。
   - 同步修改 ~142 行错误文案，列出允许的工具。
   - 重构为具名常量 `AUTO_MEDIUM_TOOLS: &[&str]`，便于测试。
2. **Agent 侧 `agentMayAutoApprove`**：保持 `write` 档位 + dev/staging；**新增** `host === "remote"` 条件（以前 `{host:"local", environment:"development"}` 也能 agent_auto，这是一个漏洞式的宽松，与宿主不一致）。`registry.ts` 的 `agent_auto` 校验同步加 `target.host === "remote"`。——注意这是**收紧**一处不一致，写进 ADR。
3. **prompt**（`policy.rs` ~163）：把“medium 风险的配置备份/编辑”改成“medium 风险的配置备份、编辑与整文件写入（可先备份再写）”；其余句子不变。
4. **UI**：`auto-delegation.ts` / `run-policy.ts` 中描述受限 auto 覆盖范围的文案与任何工具白名单同步（`grep -n "备份\|编辑\|filesystem" apps/desktop/src/features/investigations/auto-delegation.ts apps/desktop/src/features/agent/run-policy.ts`）。
5. **测试**：
   - Rust：`apps/desktop/src-tauri/src/commands/host/tests.rs`（或 plan 相关测试模块，`grep -rn task_allows_auto_medium_action apps/desktop/src-tauri/src`）新增表驱动：write 单工具允许；backup+write 组合允许；backup.cleanup 拒绝；restore 拒绝；production 拒绝；local 拒绝；Ask 模式拒绝；High 风险拒绝。
   - TS：`permission-engine.test.ts` 新增 local+development 的 write 在 `permissionMode:"auto"` 下为 `ask`；`registry.test.ts` 伪造 local 的 `agent_auto` 票据被拒。
   - 检查 `apps/agent/src/runtime/investigation-acceptance.test.ts` 与 `apps/desktop/tests/*auto*` 是否钉了旧范围。

### 验收
`cargo test -p yukinal-desktop -- --test-threads=1` 与 `pnpm test` 全绿；plan 保存 `filesystem.write` medium 无审批步骤在 auto/goal/execute/staging 任务下成功，production 下 `denied_by_policy`。

### 冲突提示
`host/plan.rs` 也会被 WP5 改（`validate_playbook_step`），不同函数，合并时注意。

---

## WP3：MCP 信任分级（ADR 0074）

### 现状
- `apps/agent/src/mcp/tool.ts`：`MCP_TOOL_RISK = "critical"` 写死，模块注释 rule 1 解释为什么不采信注解；所有 MCP 工具 `effectful: true`。
- **宿主完全丢弃了 MCP 注解**：`crates/core/src/mcp/descriptor.rs`、`catalog.rs` 中无 `annotations`/`readOnlyHint` 字段（`grep -ri annotation crates/core/src/mcp` 无结果）。
- `apps/desktop/src-tauri/src/commands/host.rs` `is_effectful_host_tool`：`|| mcp::is_mcp_tool_name(tool_name)`，所以所有 `mcp.*` 都要求 durable plan + 幂等账本。注释引用 ADR 0014。
- 配置：`crates/core/src/mcp/config.rs`（服务器配置、`MAX_HTTP_AUTH_HEADERS` 等）、`apps/desktop/src-tauri/src/commands/mcp/configuration.rs`（保存/列出/审阅投影）、`packages/shared/src/schemas/mcp.ts` + `types/mcp.ts`、`apps/desktop/src/features/settings/McpSettings.tsx`、fixtures `packages/shared/fixtures/ipc/mcp_server_{save,list,review}.json`。
- 目录类型：共享 `HostMcpCatalogTool`（`packages/shared/src/types/mcp.ts` / schemas），agent 侧 `apps/agent/src/mcp/catalog.ts` 注册。

### 设计
- 每台 MCP 服务器新增配置 `annotationTrust: "none" | "trusted"`，**默认 `none`**（serde `#[serde(default)]`，旧配置原样加载，行为不变）。
- 解析 MCP `tools/list` 中每个工具的 `annotations`：`readOnlyHint`、`destructiveHint`、`idempotentHint`、`openWorldHint`，均为可选 bool；类型不对视为缺省。
- **宿主是有效风险的唯一来源**，在目录项里下发 `risk`：
  | 条件 | risk | 档位 |
  |---|---|---|
  | 服务器 `none` | `critical` | dangerous，永不记住 |
  | trusted 且 `readOnlyHint === true` | `low` | read（非副作用） |
  | trusted 且 `destructiveHint === false` | `medium` | write |
  | trusted 其余情况（含缺省注解） | `high` | dangerous（dev/staging 远程可会话授权，见 WP1；MCP 目标通常是 local，因此实际仍逐次） |
- 宿主执行：MCP 工具**仅当**当前目录解析为 `low` 时视为非 effectful（不要求 durable plan、不进幂等账本）；目录查不到/锁失败 → effectful（失败关闭）。
- Agent：`tool.ts` 使用目录项 `risk`，只接受 `low|medium|high|critical`，缺失或非法 → `critical`（旧宿主）；`effectful = risk !== "low"`。
- 切换 `annotationTrust` 属于配置变更：应让已运行的服务器重新拉取目录（沿用现有“保存即重启/重新审阅”路径，查看 `configuration.rs` 中哪些字段变化会触发重启，把新字段加入）。

### 改动清单
1. `crates/core/src/mcp/descriptor.rs`：新增 `McpToolAnnotations { read_only_hint: Option<bool>, destructive_hint: Option<bool>, idempotent_hint: Option<bool>, open_world_hint: Option<bool> }`，宽松解析（不因注解错误拒绝整个工具）；挂到工具描述上。
2. `crates/core/src/mcp/config.rs`：服务器配置加 `annotation_trust: McpAnnotationTrust`（`#[serde(default, rename_all="camelCase")]`，枚举 `None`/`Trusted`，序列化为 `"none"`/`"trusted"`）。
3. `crates/core/src/mcp/catalog.rs`：新增纯函数 `effective_risk(trust, annotations) -> &'static str`（上表），目录项带 `risk`。
4. `apps/desktop/src-tauri/src/commands/mcp/configuration.rs`：保存/列出/审阅投影加 `annotationTrust`；该字段变化纳入“需要重启/重新审阅”判断。
5. `apps/desktop/src-tauri/src/commands/host.rs`：`is_effectful_host_tool` 需要 `state` 才能查目录——改为 `fn is_effectful_host_tool(state: &AppState, tool_name: &str) -> bool`，MCP 分支调用一个新函数 `mcp::tool_effective_risk(state, tool_name) -> Option<String>`，`Some("low")` 才返回 false。所有调用点（`requires_host_tool_idempotency`、`effectful_tool_requires_durable_plan` 等）同步。更新 ~150 行关于 ADR 0014 的注释。
6. 共享 TS：`HostMcpCatalogTool` 加 `risk?: "low"|"medium"|"high"|"critical"`；MCP 服务器配置 schema/type 加 `annotationTrust: z.enum(["none","trusted"]).default("none")`（注意 strictObject 与 fixture）。
7. `apps/agent/src/mcp/tool.ts`：删去写死常量用法，改为 `resolveMcpRisk(tool.risk)`；保留导出 `MCP_TOOL_RISK` 作为“未信任/缺省值”以免破坏引用；重写模块注释 rule 1：默认不采信，用户按服务器显式开启后由**宿主**把注解映射为有界档位，critical 仍是默认。
8. `McpSettings.tsx`：每台服务器一个开关“信任此服务器声明的只读/破坏性注解”，下方说明：“开启后，服务器自称只读的工具会像内置只读工具一样自动执行，自称非破坏性的工具按写入档位审批。只对你信任的服务器开启——注解是服务器自己的说法，不是证据。”默认关闭。
9. 测试：Rust（注解解析含错误类型、配置默认/往返、`effective_risk` 表、effectful 判定含目录缺失失败关闭）；TS（`apps/agent/src/mcp/catalog.test.ts` 风险映射与 effectful、`packages/shared` schema、`apps/desktop/tests/mcp-settings.test.tsx` 开关保存）；IPC fixtures 更新后 `pnpm check` 的契约检查需通过（`apps/desktop/src-tauri/src/commands/fixture_contracts.rs`）。

### 验收
默认配置下所有现有 MCP 测试行为不变；trusted 服务器的只读工具在 `readonly` 运行中可执行且不要求 taskId/planId；非只读仍需计划绑定与审批。

### 冲突提示
`host.rs` 也会被 WP5 改（新增工具常量与分发）；`config.rs` 也会被 WP4 改（header 上限常量）。

---

## WP4：数值上限上调（ADR 0075）

每项都要：改常量 → `grep` 旧数字清理副本 → 更新测试 → 更新 UI/description 文案。

### 4.1 filesystem.backup：512 KiB → 1 MiB
- `crates/filesystem/src/limits.rs:40` `MAX_AGENT_BACKUP_BYTES = MAX_AGENT_EDIT_BYTES` → 解耦为独立 `1024 * 1024`。编辑/写入保持 512 KiB（`:21`、`:34`）。
- 编译期断言（`:100-120`）：新增 `MAX_AGENT_BACKUP_BYTES <= MAX_AGENT_READ_BYTES`（读上限 1 MiB）；固定值测试 524_288 → 1_048_576。
- 执行点：`crates/filesystem/src/service/remote_file_service.rs:103-107,144-148,178-182,241-245` 核对使用的是 backup 常量而不是 edit 常量（恢复 restore 也要能处理 1 MiB 备份——检查 restore 走的是哪个上限，必须 ≥ backup）。
- `apps/desktop/src-tauri/src/commands/host/tools.rs:68-72` 错误 detail `maxBackupBytes`；`host/tests.rs:559,566` 断言值。
- `packages/shared/src/schemas/file.ts:120` 账本 `bytesBackedUp` 最大 1 MiB：刚好够，不改。
- Agent 描述 `apps/agent/src/tools/builtin/filesystem-backup.ts` 中如有 “512 KiB” 同步改。

### 4.2 server.logs：24 h → 7 天；120 行 → 500 行
- `apps/desktop/src-tauri/src/commands/logs.rs:16` `MAX_LOG_SINCE_SECONDS = 86_400` → `604_800`；测试 `:249`（86_401）改成 604_801。
- `logs.rs:15` `MAX_LOG_LINES` → 500；**`:13` 和 `:58` 命令字符串里写死的 `-n 120` / `tail -n 120` 必须改成用常量拼接**（`format!`），`:116` 注释、`.take`（`:161`）同步。
- `packages/shared/src/schemas/log.ts:11` `.max(86_400)` → `604_800`；`types/log.ts:11` 注释；`schemas/log.test.ts:8`。
- `apps/agent/src/tools/builtin/server-logs.ts:10` description “last 24 hours” → “last 7 days”。
- `apps/desktop/src/features/logs/LogsPane.tsx:51,75` “120 行”文案。
- 注意证据落库上限 1 MiB（`crates/database/src/models/investigation.rs:686`、`INVESTIGATION_LIMITS.maxEvidenceSerializedBytes`）：500 行 × 常见行长通常 < 1 MiB；若超限，宿主应截断并标 `truncated`，确认现有路径如此处理，否则加截断。

### 4.3 investigation.evidence.triage：16 → 32，有界并发
- `apps/agent/src/tools/builtin/investigation-evidence-triage.ts:17` `MAX_EVIDENCE = 16` → 32；`:85` description “up to 16”。
- `:98` 当前 `Promise.all` 一次性并发全部取回 → 改为每批 8 个的有界并发（写一个小的 `mapWithConcurrency(items, 8, fn)` 本地函数，保留顺序、遇 abort 立即停止）。
- 测试：32 条接受、33 条被 schema 拒绝；并发不超过 8（用计数 mock 验证）。

### 4.4 多模态附件（需要加大 sidecar 帧）
**帧**：
- Rust：`crates/core/src/mcp/wire.rs:54` `MAX_FRAME_BYTES = 8 MiB` 被 sidecar 与 MCP 共用。新增 sidecar 专用 `MAX_SIDECAR_FRAME_BYTES = 24 * 1024 * 1024`（放在 `crates/core/src/sidecar/`），`sidecar/mod.rs:340 ensure_frame_size`、`:171`、`:223`、`:417`、`:461` 改用它；`read_frame` 需要接收上限参数（或新增带上限的变体）。**MCP 保持 8 MiB。** 测试 `sidecar/mod.rs:712-720`、`wire.rs:420` 同步。
- TS：`packages/shared/src/protocol/ndjson.ts:9` `MAX_FRAME_BYTES` 保留，另导出 `MAX_SIDECAR_FRAME_BYTES = 24 MiB`；`NdjsonDecoder` 构造参数接受 `maxFrameBytes`；`apps/agent/src/transport/stdio.ts:28` 与 `host-client.ts` 使用 sidecar 值。
- **TS 写侧目前没有大小检查**：在 `encodeFrame`（`ndjson.ts:18`）加可选上限参数，超限抛出明确错误；`stdio.ts:34,38`、`host-client.ts:306,350` 传入 sidecar 上限。测试 `ndjson.test.ts:26` 扩充。
**预算**（`packages/shared/src/types/chat.ts:127-152` 与 `apps/desktop/src-tauri/src/commands/agent_run.rs:58-72` 必须一一对应）：
| 项 | 旧 | 新 |
|---|---|---|
| 图片 | 4 张 × 4 MiB | 8 张 × 5 MiB |
| PDF | 2 个 × 3 MiB | 4 个 × 8 MiB |
| 音频 | 2 段 × 4 MiB | 4 段 × 8 MiB |
| 图片/PDF/音频原始字节总预算 | 5 MiB | 12 MiB |
| 文本文件 | 4 个 × 256 KiB，总 512 KiB | 8 个 × 512 KiB，总 2 MiB |
- 最坏帧：12 MiB base64 ≈ 16.0 MiB + 文本 2 MiB JSON 转义最坏 ≈ 4 MiB + prompt/parts 最坏 ≈ 0.6 MiB + 包络 < 21 MiB < 24 MiB。把这段算式写进 `chat.ts:134` 附近注释。
- 派生检查：`packages/shared/src/schemas/permission.ts:145-148,223-224` 的 base64 长度上限、`:271-332` superRefine；Rust `agent_run.rs:104-267`。
- UI：`apps/desktop/src/features/agent/image-attachments.ts:29-166`、`AgentComposer.tsx:726-749`；顺手修正过时文案 `image-attachments.ts:45,118`（“图片与 PDF”→“图片、PDF 与音频”）与 `agent_run.rs:256`。
- 测试字面量：`apps/desktop/tests/ui-logic.test.ts:591,669,681,740`；`packages/shared/src/schemas/ipc.test.ts:537-538,570-578`（`"A".repeat(5_592_404)` / `1_677_216` 需按新预算重算：base64 长度 = `4 * ceil(bytes/3)`），`:672`；`agent_run.rs:948,1112,1136,1210,1219-1222`。

### 4.5 任务 guardrails
- `apps/desktop/src-tauri/src/commands/investigation.rs:48` `MAX_GUARDRAIL_WINDOW_SECONDS = 365*86_400` → `1095*86_400`（检查点 `investigation/policy.rs:248-249`）。
- 工具黑名单、路径前缀数量 64 → 128：Rust `investigation.rs:44-45`（检查点 `policy.rs:198,220`），TS `INVESTIGATION_LIMITS.maxGuardrailTools/maxGuardrailPathPrefixes`（`packages/shared/src/types/investigation.ts:669-670`，被 `schemas/investigation.ts:219,222,427,430` 使用）。
- 补测试：`investigation/tests.rs:197-225` 附近新增数量上限与时间窗上限的边界测试（当前没有）。

### 4.6 其他
- KRL 签名公钥 8 → 16：`crates/ssh/src/backend/hostkey.rs:205` 与 `apps/desktop/src-tauri/src/commands/server/mod.rs:443` 的裸字面量 `8` 改为共享具名常量 `MAX_KRL_SIGNERS`；`packages/shared/src/schemas/server.ts:47` `.max(8)`；测试 `server.test.ts:219`（9 项→17 项）、`hostkey.rs:666`；UI `AddServerModal.tsx:280`。
- MCP 静态认证头 16 → 32：`crates/core/src/mcp/config.rs:49` `MAX_HTTP_AUTH_HEADERS`（`:492,:521`，测试 `:866`）；`commands/mcp/configuration.rs:176,287`；`packages/shared/src/schemas/mcp.ts:96,166`。

### 不在本包范围（有意不动）
观察窗口 86_400 上限（TS 常量被 schedule schema 复用，Rust 另有字面量，牵连过大）；evidence search 64；docker logs 500 尾行；KRL 16 MiB。

### 冲突提示
`config.rs`（WP3）、`chat.ts`（WP1 改了 `ApprovalRequest`，不同区域）。

---

## WP5：备份生命周期——跨任务批量轮换与保留策略（ADR 0076）

### 现状
- 账本：`crates/database/src/repositories/filesystem_backups.rs`（状态 `available`/`restored`，记录服务器、原路径、backupPath、revision、taskId）。
- 宿主工具：`apps/desktop/src-tauri/src/commands/host/tools.rs`（`filesystem_backup`、`filesystem_backup_cleanup` ~620 行起、restore、backup.list）；分发在 `host.rs` ~1208；常量 `host.rs:113-118`；`is_effectful_host_tool` 列表 `host.rs` ~210。
- `filesystem.backup.cleanup` 目前单项：精确 `path`、`backupPath`、`expectedRevision`，要求同一任务（或同一无任务会话）的可用记录，删除前做远端内容复核（`AgentCleanupBackupRequest::check`）。
- Playbook：`apps/agent/src/tools/builtin/investigation-playbook.ts` 的 `backup_cleanup` 模板（`:24,32,93-118`）；宿主 `host/plan.rs` `validate_playbook_step` 校验。
- 共享 schema：`packages/shared/src/schemas/file.ts`（`backup.list` 上限 128，`:110`）。

### 设计
1. **新只读工具 `filesystem.backup.retention`**（risk `read`，非 effectful，不访问远端）：
   - 输入：`pathPrefix?`（绝对路径）、`keepLatest?`（1..64，按原路径保留最新 N 份）、`olderThanDays?`（1..3650），二者至少一个；目标服务器取自调用 target。
   - 宿主查询该服务器**所有任务**的 `available` 记录，按原路径分组、`created_at` 降序；超出 keepLatest 的 或 早于 olderThanDays 的 为候选（两个都给时取并集？——**取交集更保守**：既超出保留份数又足够旧。实现交集，并在 description 写明）。
   - 输出：`candidates`（≤32，字段 path、backupPath、revision、taskId、createdAt、bytesBackedUp）、`truncated`、`keptCount`、`scannedCount`。
   - 新 repository 查询 + DB 单测。
2. **`filesystem.backup.cleanup` 批量模式**：输入 `items: [{path, backupPath, expectedRevision}]`（1..32）与现有单项形态二选一（Zod `union`，Rust `untagged` 或显式字段判断；单项向后兼容）。
   - 宿主逐项顺序执行：每项重新读账本 → 复用现有单项的远端内容复核与删除 → 记录每项结果 `removed | skipped{reason} | failed{error}`；每项之间检查 `cancel`，取消后剩余项标 `skipped{cancelled}`。
   - 未验证的项绝不报告 removed；整体 status：全部 removed → success；否则 success 但带 `partial: true` 与明细（或按现有惯例返回 failed + detail——与现有错误模型保持一致即可，但**不能**把部分成功报成全部成功）。
   - **跨任务**：批量模式允许删除同一服务器其他任务的记录，**前提是**该项精确出现在已批准计划步骤的 input binding 中（宿主在执行时读 `request.plan_step_id` 对应步骤核对）；单项模式保持“同任务”规则。
3. **Playbook 模板 `backup_rotation`**：输入为 retention 结果中的精确 items（≤32），生成**一个** action 步骤：`allowedTools: ["filesystem.backup.cleanup"]`、`riskLevel: "medium"`、**`requiresApproval: true`（永远）**、input binding 为 items 的精确序列化；宿主 `validate_playbook_step` 接受该模板（≤32 项、绝对路径、同一服务器）。**不要**把它加入 WP2 的 `task_allows_auto_medium_action`。
4. 注册：新工具常量加入 `host.rs`、observation 列表（若作为计划只读步骤需要，`is_plan_bound_observation_tool`）；agent 工具注册处（`grep -rn "filesystem-backup-list" apps/agent/src` 找注册表）；若 IPC/fixture 契约脚本要求（`scripts/check-*.mjs`、`fixture_contracts.rs`），补 fixture。
5. 测试：Rust repository（跨任务、分组、keepLatest/olderThanDays 交集、32 截断）；宿主批量清理（逐项账本复核、跨任务仅批量+计划绑定时允许、取消中途、部分失败明细）；plan 校验（模板接受/拒绝超 32、相对路径、requiresApproval=false 被拒）；TS schema、agent 工具、playbook 模板。

### 验收
`cargo test -p yukinal-database`、`cargo test -p yukinal-desktop -- --test-threads=1`、`pnpm test` 全绿；推演：retention 返回 3 个跨任务候选 → backup_rotation 生成单步骤 → 用户批准 → 3 项逐一复核删除，账本状态更新；中途取消剩余项未删除。

### 冲突提示
`host.rs`（WP3）、`host/plan.rs`（WP2）。

---

## §7 文档与 ADR（集成者最后统一做）

1. `docs/adr.md`：标题计数 `ADR 0001–0070` 实际已到 0071，改为 `0001–0076`；新增：
   - **ADR 0072** high 风险精确动作可在 dev/staging 远程目标上会话授权；在 ADR 0009 与“有意为之的边界”记录里标注被部分取代。决定/为什么/代价/备选四栏齐全（代价：长任务中重复重启同一容器不再每次打断，但批准一次后同指纹动作在本次运行内不再可见地确认；用审计 `approvedBy:"user"` + 指纹 + 运行结束清除来界定）。
   - **ADR 0073** 受限 auto 扩到整文件写入与“备份+写/编辑”组合；同时修正 agent_auto 对本机目标的宽松（收紧）。
   - **ADR 0074** MCP 注解信任按服务器显式开启，宿主映射有效风险；ADR 0014 rule 1 标注“默认仍成立，可按服务器显式放宽”。
   - **ADR 0075** 数值上限上调与 sidecar 独立帧上限（含帧预算算式）。
   - **ADR 0076** 备份保留策略与跨任务批量清理（始终逐步骤审批，无后台删除）。
2. `docs/limitations.md`：逐条改写受影响条目（MCP 一律 critical、多模态数字、server.logs、triage、filesystem.edit/backup 段的 512 KiB、受限 auto、备份生命周期、guardrails、KRL 签名者数、“有意为之的边界”第一条）。保持该文件“只写做不到什么 + 为什么”的风格；已解决的部分移入 changelog。
3. `docs/changelog.md`：新增一个版本段（例如 `1.1.0` 未发布），逐条列出。
4. `docs/security.md`（:35-37 附件/备份数字）、`docs/architecture.md:32`（帧）、`docs/boundaries/mcp.md`、`docs/boundaries/provider.md:40-44`、`docs/risk-tiers/dangerous.md`（会话授权规则）同步。
5. 跑 `node scripts/check-publication.mjs` 与完整 `pnpm check`。

## §8 集成者检查清单（我来复查时逐项核对）

- [ ] 五个工作包各自测试全绿，合并后 `pnpm check` 全绿。
- [ ] `grep -rn "512 \* 1024\|86_400\|MAX_FRAME_BYTES\|\.max(16)\|\.max(8)"` 没有遗漏的旧副本（与本手册列出的“有意不动”对照）。
- [ ] 失败关闭：旧 sidecar 缺 `sessionGrantable`、旧宿主缺 MCP `risk`、目录查不到 → 均走严格路径，有测试。
- [ ] critical 在任何路径都不可会话授权、不可 auto（engine、registry、UI 三处测试）。
- [ ] 批量清理不会把部分成功报为全部成功；跨任务删除只在批量 + 计划绑定下发生。
- [ ] ADR 编号连续、旧 ADR 标注被取代关系；limitations.md 与代码一致。
