# 权限档位：`dangerous`（危险档）

一句话：**这一档永远不会自动执行。** 无论策略表怎么配、用户怎么委托、本会话里批准过什么，一次危险档位的调用只有一条路：用户看到它、然后逐项批准。

同一族的三份文档：[`read`](./read.md) · [`write`](./write.md)。共同规则（三层事实怎么合成一个档位、票据怎么复核）写在仓库的 [执行与授权模型](../execution-model.md#执行与授权模型)。

## 这一档由哪些风险等级构成

`high` 与 `critical` 折进这一档（`packages/shared/src/types/risk.ts:37-41`）。它是最严的一档，也是唯一一个「形态不同」的档位：只读档与写入档的差别在策略表里，这一档的差别在引擎里 —— 它有自己的强制分支，不受策略表约束。

## 它怎么落进来：三条路径

1. **工具声明 `high` 或 `critical`。**
   - `docker.restart` 声明 `high`（`apps/agent/src/tools/builtin/docker-restart.ts:11`）。
   - **每一个 MCP 工具都声明 `critical`**（`apps/agent/src/mcp/tool.ts:42` 的 `MCP_TOOL_RISK`）。理由不是「保守一点」：MCP 服务器可以在工具注解里自称只读，采信它等于把授权级别交给被授权方，所以适配器根本不读注解，一律取最严的那一档（见 [外部工具（MCP）](../boundaries/mcp.md#边界外部工具mcp)）。
2. **命令分析命中 `high` / `critical` 规则。** 16 条规则里 7 条 `critical`（`rm -rf`、`rm /`、`mkfs`、`dd of=/dev/`、重定向到块设备、`drop database`、`chmod -R /`），6 条 `high`（`truncate`、`shutdown`/`poweroff`/`halt`、`reboot`、`kubectl delete`、`docker system prune`、`curl|sh`）（`apps/agent/src/permissions/command-risk.ts:20-32`）。这一层今天没有内置工具会触发 —— `apps/agent/src/tools/` 下没有任何输入 schema 带 `command` 或 `argv`，规则由测试用假想的 `ssh.execute` 覆盖。
3. **环境抬高。** 生产与未标注环境的下限是 `high`（`ENVIRONMENT_RISK_FLOOR`，`apps/agent/src/permissions/permission-engine.ts:39-46`），所以任何**内在风险不是 `read`** 的调用在生产上都落进这一档 —— 包括一次普通的文件写入。代价写在 [ADR 0005](../adr.md#adr-0005permission-engine-是唯一的执行授权决策者) 里：生产上的普通写入不再能通过「批准本会话」免除确认。

## 三条不可绕过的约束

1. **策略表说 `auto` 也不算。** 引擎把「这一档 + 策略给了 `auto`」强制改成 `ask`，并把来源清空，理由改写为「dangerous or critical action cannot be auto-approved」（`permission-engine.ts:136-140`）。用例：「critical actions always require a direct user approval」——它连「把策略对象改成 `dangerous: "auto"`」这种情况也一起钉住了。
2. **闸口不接受任何非逐项批准的票据。** ToolRegistry 对 `decision.tier === "dangerous"` 且票据不是 `user_approved` 的调用一律拒绝，得到 `denied_by_policy`，理由固定为「Dangerous and critical actions require an explicit user approval」（`apps/agent/src/tools/registry.ts:277-283`）。用例：「a critical call rejects an Agent delegation ticket and needs user approval」。
3. **会话授权不能覆盖它。** `grantSession()` 在这一档直接返回、不记 grant（`permission-engine.ts:236-240`），而查 grant 的那条分支也明确排除 `dangerous` 档位与最终风险 `critical`（`:191-205`）。判定用**最终风险**，所以被环境升级上来的普通写入与本身就危险的工具待遇相同。用例：「a session grant never covers the dangerous tier, however it was reached」。

除此之外，**只读与计划运行模式先于一切**：`plan` / `readonly` 下任何非 `read` 档位直接 `deny`，判断发生在策略、委托和会话授权之前（`permission-engine.ts:147-151`；用例「no delegation, grant or permission mode can widen a read-only run」）。

## 谁点头

这一档只有 `user_approved` 一种票据：与挂起审批的 `approvalId` 完全一致的逐项批准（`apps/agent/src/tools/registry.ts:313-315`）。审批请求由 loop 在 `agent.waiting_approval` 事件里发出，2 分钟没有响应按「已过期」处理并拒绝，不会让运行永久挂着（`apps/agent/src/runtime/agent-loop.ts:392-407`；README 的 [今天真正可用的能力](../../README.md#今天真正可用的能力)）。

运行级委托 `auto` 与它无关：那条路径要求档位是 `write` 且目标是开发或预发布，这一档一律回落 `ask`（`permission-engine.ts:157-167`）。策略表也不会替它点头：见上面第 1 条。

## 代价：没有「记住一次」这条路

- 这一档**没有**任何「总是允许」的实现，也不打算有：一次「以后都别问了」的委托等于把危险动作交回给模型，而模型文本不能成为授权的来源（[有意为之的边界](../limitations.md#有意为之的边界不是待办)）。
- 直接后果一：一次长任务里若需要重启容器，用户一定会被打断，并且必须在看到具体命令之后再点一次。
- 直接后果二：每个 MCP 工具都要逐项批准，所以**一个只读的外部工具也要点一次**。这是 `critical` 的代价，不是漏配（见 [当前限制](../limitations.md#当前限制)）。
- 这一档的「多一次确认点击」是可见的失败，而被放行的错误自动批准是不可见的 —— ADR 0005 收敛到收紧引擎而不是放宽闸口，用的就是这条理由。

## 审计与界面里能看到的

- 执行行由宿主在拿到工具结果时写下（`apps/desktop/src-tauri/src/commands/mod.rs:361-382`）：`risk_level` 是 `high` / `critical`，`decision` 是 `ask`（只读运行模式下则是 `deny`），`approved_by` 在实践中**只可能是 `user`** —— schema 允许 `user` / `policy` / `agent` 三种（`crates/database/src/schema.rs:184`），但另外两种来源的票据在这一档会被闸口拒绝（`registry.ts:277`）。被拒绝的调用照样留下执行行：`decision` 是 `deny`、`approved_by` 为空，活动记录的状态是「已拒绝」（`commands/mod.rs:342` 与 `:348-353`）。
- **没有档位列**，也没有内在风险列：库里能回答「这次有多危险」的是 `risk_level`，能回答「谁批的」的是 `approved_by`，能回答「为什么停下来」的是界面上的 `reason` 与事实摘要（`agent-loop.ts:395-404`）。
- 界面把风险等级、决策与来源分开显示（`apps/desktop/src/lib/labels.ts:89-108`：`高` / `严重`、`需审批`、`用户批准`），**不出现「档位」这个词** —— 档位是 Agent 侧的中间量。

## 实现与钉住它的东西

| 位置 | 内容 |
| --- | --- |
| `packages/shared/src/types/risk.ts:37-41` | `high` / `critical` → `dangerous` 的唯一映射 |
| `apps/agent/src/permissions/permission-engine.ts:39-46` | 环境下限（生产与未标注是 `high`） |
| `apps/agent/src/permissions/permission-engine.ts:136-140` | 这一档不接受 `auto` 的强制分支 |
| `apps/agent/src/permissions/permission-engine.ts:191-240` | 会话授权不覆盖这一档（判定用最终风险） |
| `apps/agent/src/tools/registry.ts:277-283` | 闸口只接受逐项批准 |
| `apps/agent/src/mcp/tool.ts:36-50` | 每个 MCP 工具声明 `critical` 的理由 |
| `apps/agent/src/permissions/permission-engine.test.ts` | 「critical actions always require a direct user approval」「a session grant never covers the dangerous tier, however it was reached」 |
| `apps/agent/src/tools/registry.test.ts:268` | 「a critical call rejects an Agent delegation ticket and needs user approval」 |
| `apps/agent/src/mcp/catalog.test.ts:75-109` | MCP 工具声明 `critical` 且最终风险也是 `critical` |
