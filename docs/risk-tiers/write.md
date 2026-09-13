# 权限档位：`write`（写入档）

一句话：这一档是会改变目标状态、但没落到最严那一档的动作。它是整条授权链上**唯一可以被委托**的档位，也是「批准本会话」唯一能记住的档位之一。

同一族的三份文档：[`read`](./read.md) · [`dangerous`](./dangerous.md)。共同规则（三层事实怎么合成一个档位、票据怎么复核）写在仓库的 [执行与授权模型](../execution-model.md#执行与授权模型)。

## 这一档由哪些风险等级构成

`tierOf()` 把 `medium` 折进这一档，且只有 `medium`（`packages/shared/src/types/risk.ts:37-41`）：`read` 与 `low` 是只读档，`high` 与 `critical` 是危险档。所以这一档正好落在「中间那一格」上，而中间那一格的含义是**最终**风险为 `medium`，不是工具自己声明的那个值。

## 它怎么落进来

1. **工具声明 `medium`。** 今天的两个：`filesystem.write`（`apps/agent/src/tools/builtin/filesystem-write.ts:16`）与 `filesystem.edit`（`filesystem-edit.ts:23`）。
2. **命令分析命中 `medium` 规则。** 三条：`sudo`、`systemctl stop|disable|mask`、`docker compose down` / `docker-compose down`（`apps/agent/src/permissions/command-risk.ts:33-35`）。这一层目前没有内置工具会触发 —— `apps/agent/src/tools/` 下没有任何输入 schema 带 `command` 或 `argv`，规则由测试用假想的 `ssh.execute` 覆盖。
3. **环境把它抬进这一档。** 预发布的下限是 `medium`，所以一个内在风险为 `low` 的动作（不是 `read`）在预发布上也是这一档（`ENVIRONMENT_RISK_FLOOR`，`apps/agent/src/permissions/permission-engine.ts:39-46`）。

**反过来也成立，而且更常被读错：环境会把这一档整个抬出去。** 生产与未标注环境的下限是 `high`，`high` 折进危险档。于是：

- 在**开发**（下限 `low`）与**本机**（下限 `low`）上，`medium` 的动作就是这一档；
- 在**预发布**（下限 `medium`）上，任何非只读动作至少是这一档；
- 在**生产与未标注**（下限 `high`）上，**这一档到不了** —— 任何内在风险不是 `read` 的调用都被抬进危险档。`policy.production` 表里 `write: "ask"` 那一栏因此不是「生产上的写入会问一次」，而是「生产上的写入根本不在这一档」。这正是 [ADR 0005](../adr.md#adr-0005permission-engine-是唯一的执行授权决策者) 记下的那次加严：档位由**最终**风险决定，不由工具声明决定。

## 各环境策略表里的取值

| 策略 | 环境 | `write` 一栏 | 实际效果 |
| --- | --- | --- | --- |
| `policy.development` | 开发 | `auto` | 普通写入自动执行，来源标记为策略 |
| `policy.staging` | 预发布 | `auto` | 同上 |
| `policy.local` | 本机 | `ask` | 本机写入也停下来问一次 |
| `policy.production` | 生产 | `ask` | 该栏在引擎的现有下限下不可达（见上），生产写入实际上是危险档 |

未标注环境走生产策略（`defaultPolicyFor()`，`packages/shared/src/types/risk.ts:151-165`）。表本身在 `risk.ts:98-129`，它是策略的唯一来源。

## 谁点头：这一档能用到全部四种票据来源

| 票据来源 | 出现的条件 | 决策上的来源标记 | 闸口复核 |
| --- | --- | --- | --- |
| `policy_auto` | 策略表说 `auto`（开发与预发布） | `policy` | 票据说 `policy_auto`，`approvedBy` 必须是 `policy`（`apps/agent/src/tools/registry.ts:294`） |
| `agent_auto` | 用户在**这次运行**里选了 `auto` 委托，且目标是开发或预发布 | `agent` | 还要求票据对应的决策档位是 `write`、环境是 `development` 或 `staging`（`registry.ts:297-309`） |
| `session_auto` | 用户对「同一工具 + 同一目标」点过「批准本会话」 | `user` | `registry.ts:310` |
| `user_approved` | 逐项批准，审批 ID 必须与挂起的那次完全一致 | —— | `registry.ts:313-315` |

四种票据都还要过同一批基础检查：工具名一致、目标四元组（host、serverId、workspaceId、environment）完全一致、`deny` 的决策不接受任何自动票据（`registry.ts:255-293`）。任一字段不匹配得到的是 `denied_by_policy` 的工具结果，而不是一条被忽略的日志。

## 运行级委托的边界正好画在这一档上

用户选 `auto`（「受限自动批准」）时，引擎只对**这一档**开一个口子：档位是 `write`、目标是开发或预发布、且策略本身没有拒绝，才变成自动并标记来源为 `agent`；其余一律回落 `ask`（`apps/agent/src/permissions/permission-engine.ts:157-167`）。这条判断硬编码在引擎里，放宽它需要一条新的 ADR 而不是一次配置改动（[ADR 0009](../adr.md#adr-0009agent-权限采用显式的运行级委托)）。

用例：「auto mode delegates only a write-tier action on development or staging」。

## 会话授权怎么覆盖它，又怎么不覆盖它

- 用户点「批准本会话」后，引擎记一条 grant，键是**工具名 + 目标**（host、serverId、workspaceId、environment 四元组，`grantKey()`，`permission-engine.ts:66-70`）。同一次批准因此不会在另一台服务器、另一个工作区或另一个环境上生效（用例：「grants are scoped per server…」「grants are also scoped per workspace…」「a grant is scoped to its environment…」）。
- 判定依据是**最终风险**，不是内在风险：`dangerous` 档位与最终风险 `critical` 的调用既不记录 grant 也不被 grant 覆盖（`permission-engine.ts:191-205` 与 `:236-240`）。这条正是 ADR 0005 的「危险档位的加严」。
- 作用域是**单次运行**：loop 在没有运行在飞时调用 `clearGrants()`（`permission-engine.ts:249`），否则一次批准会一直有效到 sidecar 进程退出，跨越多次无关运行。
- 用例：「a session grant covers a write-tier action」。
- **一次 grant 就是一次点击。** 它只对「同一工具 + 同一目标」有效，不绑定具体命令内容 —— 参数变了、目标没变，仍然算被批准。要更严的做法只有每次都逐项批准，那正是危险档位的形态。

## 什么会拒绝它

- **只读与计划运行模式。** `plan` / `readonly` 下任何非 `read` 档位直接 `deny`，判断发生在策略、委托与会话授权**之前**（`permission-engine.ts:147-151`），所以没有任何一条路径能把它打开（用例：「read-only and plan modes deny every non-read action outright」「no delegation, grant or permission mode can widen a read-only run」）。
- **`ask` 批准方式。** 用户选 `ask` 时，即使策略说 `auto`，这一档也会改成 `ask` 并清掉来源（`permission-engine.ts:171-175`）。
- **策略表里的 `deny`。** 四张内建策略在 `write` 一栏都不写 `deny`，但引擎对传入的策略对象是照读的；`deny` 在任何批准方式下都不可推翻。

## 审计与界面里能看到的

- 每次执行由宿主写成 `tool_executions` 一行：`risk_level`（这里是最终风险，通常是 `medium`）、`decision`（`auto` 或 `ask`）、`approved_by`（三种取值 `user` / `policy` / `agent`，`crates/database/src/schema.rs:184`）。**没有档位列**，也没有内在风险列：事后要解释「这次为什么问了」，看的是 `risk_level` + `environment`；要解释「谁点的头」，看 `approved_by`。
- 来源是如实记录的：Agent 自主批准不会被记成用户批准，loop 依票据种类回填 `approvedBy`（`apps/agent/src/runtime/agent-loop.ts:511`）。
- 界面把风险等级、决策与来源分开显示，措辞是三套不同的词（`apps/desktop/src/lib/labels.ts:89-108`：`中`、`自动批准 / 需审批 / 策略禁止`、`用户批准 / 策略批准 / Agent 自主批准`）。

## 实现与钉住它的东西

| 位置 | 内容 |
| --- | --- |
| `packages/shared/src/types/risk.ts:37-41` | `medium` → `write` 的唯一映射 |
| `packages/shared/src/types/risk.ts:98-129` | 四张内建策略表 |
| `apps/agent/src/permissions/permission-engine.ts:119-175` | 环境抬高、运行模式、委托与 `ask` 的先后顺序 |
| `apps/agent/src/permissions/permission-engine.ts:191-251` | 会话授权的判定、作用域与清除 |
| `apps/agent/src/tools/registry.ts:255-316` | 四种票据的复核 |
| `apps/agent/src/permissions/permission-engine.test.ts` | 「layer 3: production turns a medium write into an approval」「an unknown environment is treated like production」「ask mode pauses before writes…」「auto mode delegates only a write-tier action…」「a session grant covers a write-tier action」以及三条作用域用例 |
