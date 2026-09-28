# 权限档位：`dangerous`（危险档）

一句话：**这一档默认永远不会自动执行。** 无论策略表怎么配、用户怎么委托，一次危险档位的调用只有一条路：用户看到它、然后批准。唯一的例外是 [ADR 0072](../adr.md#adr-0072high-动作可记住到本次运行限定远程开发与预发布)：在**远程开发/预发布**目标上，用户对一个精确的 `high` 动作点「本次运行批准」后，**同一工具 + 同一目标 + 同一输入指纹**在本次运行内不再询问。`critical`、以及生产/未知/本机目标上的危险档，仍然每次都要单独批准。

此外，所有会改变远端状态或由第三方 MCP 服务器执行的 effectful 调用，都必须先绑定 durable task、ChangePlan 和具体 plan step；没有完整绑定时，连逐项审批后的 Agent 请求也不会进入宿主分派。交互式终端仍是用户直接操作的人工旁路。

同一族的三份文档：[`read`](./read.md) · [`write`](./write.md)。共同规则（三层事实怎么合成一个档位、票据怎么复核）写在仓库的 [执行与授权模型](../execution-model.md#执行与授权模型)。

## 这一档由哪些风险等级构成

`high` 与 `critical` 折进这一档（`packages/shared/src/types/risk.ts` 的 `tierOf`）。它是最严的一档，也是唯一一个「形态不同」的档位：只读档与写入档的差别在策略表里，这一档的差别在引擎里 —— 它有自己的强制分支，不受策略表约束。

## 它怎么落进来：三条路径

1. **工具声明 `high` 或 `critical`。**
   - `filesystem.restore`、`docker.restart`、`systemd.restart` 与 `package.install` 声明 `high`。
   - **MCP 工具默认 `critical`**：宿主对一台没有被用户显式信任的服务器，无论它自述什么注解都取 `critical`（`apps/desktop/src-tauri/src/commands/mcp.rs` 的 `tool_effective_risk`，`crates/core/src/mcp/catalog.rs` 的 `effective_risk`）。理由不是「保守一点」：服务器可以在工具注解里自称只读，采信它等于把授权级别交给被授权方，所以默认根本不读注解。用户在设置页显式开启「信任此服务器声明的只读/破坏性注解」后，宿主才把 `readOnlyHint === true` 映射为 `low`、`destructiveHint === false` 映射为 `medium`，其余仍是 `high`（[ADR 0074](../adr.md#adr-0074mcp-注解按服务器显式信任宿主映射有效风险)、[外部工具（MCP）](../boundaries/mcp.md#边界外部工具mcp)）。被降级的工具因此可能不再落进这一档。
2. **命令分析命中 `high` / `critical` 规则。** 规则表在 `apps/agent/src/permissions/command-risk.ts`，本轮以下没有内置工具会触发 —— `apps/agent/src/tools/` 下没有任何输入 schema 带 `command` 或 `argv`，规则由测试用假想的 `ssh.execute` 覆盖。
3. **环境抬高。** 生产与未标注环境的下限是 `high`（`ENVIRONMENT_RISK_FLOOR`，`apps/agent/src/permissions/permission-engine.ts`），所以任何**内在风险不是 `read`** 的调用在生产上都落进这一档 —— 包括一次普通的文件写入。代价写在 [ADR 0005](../adr.md#adr-0005permission-engine-是唯一的执行授权决策者) 里：生产上的普通写入不再能通过「批准本会话」免除确认。

## 不可绕过的约束

1. **策略表说 `auto` 也不算。** 引擎把「这一档 + 策略给了 `auto`」强制改成 `ask`，并把来源清空，理由改写为「dangerous or critical action cannot be auto-approved」（`permission-engine.ts` 的强制分支）。用例：「critical actions always require a direct user approval」——它连「把策略对象改成 `dangerous: "auto"`」这种情况也一起钉住了。
2. **闸口只接受两类票据。** ToolRegistry 对 `decision.tier === "dangerous"` 的调用，只接受逐项批准（`user_approved`），或者——当且仅当该决策可被会话记住时——一张有用户来源的 `session_auto` 票据。其余一律 `denied_by_policy`。用例：「a critical call rejects an Agent delegation ticket and needs user approval」「a session grant on a dangerous-tier target asks, and the engine says so too」「a session grant covers an exact high-risk action on a remote staging target end to end」。
3. **会话授权只在允许的目标上覆盖它。** 判定集中在 `isSessionGrantable`（`packages/shared/src/types/risk.ts`）：`critical` 在任何环境都返回 `false`；`tier !== "dangerous"` 返回 `true`；危险档只有在 `target.host === "remote"` 且环境是 `development`/`staging` 时才返回 `true`。引擎的查 grant 分支、`grantSession()`、registry 以及审批卡片全部调用同一个函数，所以「引擎说能记住、执行层拒绝」这种不一致不会再发生。用例：「a session grant never covers dangerous work on production/unknown, nor critical anywhere」「a session grant never covers an intrinsically dangerous tool on the local machine」。
4. **运行模式先于一切。** `plan` / `readonly` 下任何非 `read` 档位直接 `deny`，判断发生在策略、委托和会话授权之前（用例「no delegation, grant or permission mode can widen a read-only run」）。

## 谁点头

这一档的票据来源最终仍是用户：

- **逐项批准**：与挂起审批的 `approvalId` 完全一致的 `user_approved`（`apps/agent/src/tools/registry.ts`）。审批请求由 loop 在 `agent.waiting_approval` 事件里发出，2 分钟没有响应按「已过期」处理并拒绝，不会让运行永久挂着。
- **本次运行批准**：在远程开发/预发布目标上，对一个 `isSessionGrantable` 为真的精确动作，用户点「本次运行批准」后引擎记录 grant，下一次同指纹调用走 `session_auto`。审批卡片的 `sessionGrantable` 字段由 loop 写入；为 `false` 时界面不渲染这个按钮，而是显示「此操作每次都需要单独批准，不能记住到本次运行。」——按钮只在真的会记住时出现。
- 计划步骤 `requiresApproval: true` 只能由上面两种用户动作满足；策略与 Agent 委托不能满足它。
- 运行级委托 `auto` 与它无关：那条路径要求档位是 `write` 且目标是**远程**开发或预发布，危险档一律回落 `ask`。

策略表也不会替它点头：见上面第 1 条。

## 代价：默认没有「记住一次」这条路

- `critical`（含默认的每一个 MCP 工具）**没有**任何「总是允许」的实现，也不打算有：一次「以后都别问了」的委托等于把危险动作交回给模型，而模型文本不能成为授权的来源（[有意为之的边界](../limitations.md#有意为之的边界不是待办)）。
- `high` 在远程开发/预发布上多了一条**有界**的记住路径：它是一次真实用户点击、绑定输入指纹、只在本次运行有效，且生产/未知/本机不适用。直接后果：一次长任务里对同一容器/服务的完全相同重启不再每次打断，但批准一次后同一指纹动作在本次运行内不再可见地确认。
- 生产上任何非只读动作仍要逐项确认；默认情况下的只读 MCP 工具也要点一次，这是 `critical` 的代价，不是漏配（见 [当前限制](../limitations.md#当前限制)）。
- 这一档的「多一次确认点击」是可见的失败，而被放行的错误自动批准是不可见的 —— ADR 0005 收敛到收紧引擎而不是放宽闸口，用的就是这条理由。

## 审计与界面里能看到的

- 执行行由宿主在拿到工具结果时写下：`risk_level` 是 `high` / `critical`，`decision` 是 `ask`（被会话授权放行时是 `auto`）、`approved_by` 是 `user`。被拒绝的调用照样留下执行行：`decision` 是 `deny`、`approved_by` 为空。
- **没有档位列**，也没有内在风险列：库里能回答「这次有多危险」的是 `risk_level`，能回答「谁批的」的是 `approved_by`，能回答「为什么停下来」的是界面上的 `reason` 与事实摘要。
- 界面把风险等级、决策与来源分开显示（`apps/desktop/src/lib/labels.ts`：`高` / `严重`、`需审批`、`用户批准`），**不出现「档位」这个词** —— 档位是 Agent 侧的中间量。

## 实现与钉住它的东西

| 位置 | 内容 |
| --- | --- |
| `packages/shared/src/types/risk.ts` | `high` / `critical` → `dangerous` 的映射，以及 `isSessionGrantable` 的唯一规则 |
| `apps/agent/src/permissions/permission-engine.ts` | 环境下限、这一档不接受 `auto` 的强制分支、按最终档位记录/匹配会话授权 |
| `apps/agent/src/tools/registry.ts` | 闸口；危险档只接受逐项批准或允许的 `session_auto` |
| `apps/desktop/src-tauri/src/commands/mcp.rs` | 宿主对 MCP 工具的有效风险（默认 `critical`，显式信任后才按注解） |
| `crates/core/src/mcp/catalog.rs` | `effective_risk` 的有界映射 |
| `apps/agent/src/permissions/permission-engine.test.ts` | 「critical actions always require a direct user approval」「a session grant covers an exact high-risk action on a staging target」「a session grant never covers dangerous work on production/unknown, nor critical anywhere」 |
| `apps/agent/src/tools/registry.test.ts` | 「a critical call rejects an Agent delegation ticket and needs user approval」「a session grant covers an exact high-risk action on a remote staging target end to end」 |
| `apps/agent/src/mcp/catalog.test.ts` | 宿主风险映射与 effectful 判定 |
| `apps/desktop/tests/agent-approval.test.tsx` | `sessionGrantable === false` 时不渲染「本次运行批准」 |
