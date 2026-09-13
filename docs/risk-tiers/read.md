# 权限档位：`read`（只读档）

一句话：这一档的调用不改变目标上的任何状态，所以它在四张内建策略表里都是 `auto`，也是唯一能在 `plan` / `readonly` 运行里活下来的档位。

同一族的三份文档：[`write`](./write.md) · [`dangerous`](./dangerous.md)。共同规则（三层事实怎么合成一个档位、票据怎么复核）写在仓库的 [执行与授权模型](../execution-model.md#执行与授权模型)；这里只写只读档自己的规则。

## 这一档由哪些风险等级构成

档位不是风险等级本身，是五个等级折成三档的结果（`packages/shared/src/types/risk.ts:37`）：`read` 与 `low` → `read`，`medium` → `write`，`high` 与 `critical` → `dangerous`。所以这一档覆盖**两个**等级，而不是一个 —— 「低风险」与「只读」在策略层面是同一件事。

## 它怎么落进来

引擎按固定顺序合成三层事实，再取最大值（`apps/agent/src/permissions/permission-engine.ts:84-152`）：

1. **工具声明**：`declaration.risk`（第 1 层）。
2. **命令分析**：`analyzeCommand()` 的结果（第 2 层）。它只在命中规则时才作为一条事实出现，而 16 条规则里没有一条是 `read` 或 `low` —— 最低的也是 `medium`（`apps/agent/src/permissions/command-risk.ts:19-36`）。
3. **目标环境的下限**：`ENVIRONMENT_RISK_FLOOR`（第 3 层，`permission-engine.ts:39`）。

**这一档是唯一不被环境抬高的档位。** 内在风险正好是 `read` 时，引擎把环境事实的等级改写成 `read`，于是生产环境上的只看不动仍然是只读档、仍然自动执行（`permission-engine.ts:122-127`）。这不是为了省一次点击：如果生产环境把读类也抬高，「生产环境只读自动执行」这条策略就永远无法成立。

**这条豁免的边界必须写清楚。** 豁免条件是 `intrinsicRisk === "read"`，也就是「工具声明 `read`，且这次调用的参数里没有可分析的命令文本」。如果参数里带了 `command`（或 `argv`）而一条规则都没命中，命令分析的等级是 `low`（`command-risk.ts:55`），内在风险于是变成 `low` —— 而 `low` **不在**豁免范围内，生产与未标注环境的下限（`high`）会把它抬进危险档。今天没有任何内置工具带 `command` / `argv` 输入（`apps/agent/src/tools/` 下的输入 schema 里没有这两个键），所以这条路径目前只由测试用假想的 `ssh.execute` 覆盖（`permission-engine.test.ts` 的「layer 2 raises risk for the concrete command」与「critical actions always require a direct user approval」）。

## 什么工具落在这一档

- 不写 `risk` 的工具按 `read` 处理（`apps/agent/src/tools/builtin/host-backed.ts:27` 的 `spec.risk ?? "read"`），这也是 `docker.ps`、`docker.logs`、`docker.inspect`、`server.info`、`filesystem.read` 的实际取值。
- `system.echo` 显式声明 `risk: "read"`（`apps/agent/src/tools/builtin/system-echo.ts:21`）。
- 没有任何 MCP 工具在这一档：每个 MCP 工具声明 `critical`（见 [`dangerous`](./dangerous.md)）。

## 各环境策略表里的取值

四张内建策略的 `read` 一栏**全是** `auto`（`packages/shared/src/types/risk.ts:98-129`）：`policy.local`、`policy.development`、`policy.staging`、`policy.production`。未标注环境走生产策略（`defaultPolicyFor()`，同文件 `:151-165`），而它的只读栏同样也是 `auto`。

## 谁点头

这一档只有一种自动来源：环境策略。

| 票据来源 | 这一档会不会出现 | 依据 |
| --- | --- | --- |
| `policy_auto` | 会。`approvedBy` 是 `policy`，loop 依它生成票据 | `apps/agent/src/runtime/agent-loop.ts:385-391`；复核见 `apps/agent/src/tools/registry.ts:294` |
| `agent_auto` | 不会。它要求档位是 `write` 且目标是开发或预发布 | `registry.ts:300-309` |
| `session_auto` | 不会。它只在决策结果是 `ask` 时产生，而这一档在策略表里是 `auto` | `agent-loop.ts:386-391`、`registry.ts:310` |
| `user_approved` | 不会，同上 | `registry.ts:313` |

这一段的前提是策略来自那四张内建表：引擎本身接受调用方传入的策略对象（`permission-engine.ts:86`），但运行请求里能选到的只有这四个 id —— `policy-registry.ts` 对未知 id 报 `INVALID_PARAMS`，不会退回环境默认值（`apps/agent/src/permissions/policy-registry.ts:54-63`）。一张把只读栏写成 `ask` 的策略会让这一档出现审批，而那样的策略今天没有入口。所以在今天这套入口下，只有运行模式能打断这一档，而且方向是反的（见下）。

## 什么会拒绝它，什么不会

- **`plan` / `readonly` 不会拒绝它。** 只读运行模式拒绝的是**非** `read` 档位，读类照常执行（`permission-engine.ts:147-151`；用例「read-only modes still allow reads」）。
- **`ask` 批准方式不会打断它。** `ask` 只把「策略说 `auto` 且档位不是 `read`」的决策改回 `ask`（`permission-engine.ts:171-175`；用例「ask mode pauses before writes while keeping reads automatic」）。
- **`deny` 到不了它。** 这一档的 `outcome` 由策略表给出，四张内建策略都不在这一栏写 `deny`；引擎里唯一会把它变成 `deny` 的是非只读运行模式下的判定，而那一条只针对非只读档位。

## 审计与界面里能看到的

- 事件与执行行记的是**最终风险**，不是档位：`agent.tool_call` 带 `riskLevel: decision.finalRisk` 与 `decision`（`agent-loop.ts:371-383`），宿主把它写成 `tool_executions` 一行，`risk_level` 的 CHECK 允许 `read|low|medium|high|critical`（`crates/database/src/schema.rs:182`）。**库里没有档位列**，所以档位是 Agent 侧的中间量：事后要判断「这次为什么自动执行了」，看的是 `risk_level` + `environment`。
- 界面显示的是风险等级徽标与决策来源（「只读 / 低 / 中 / 高 / 严重」、`自动批准 / 需审批 / 策略禁止`、`用户批准 / 策略批准 / Agent 自主批准`，`apps/desktop/src/lib/labels.ts:89-108`），**不显示档位这个词**。审批弹窗里给用户看的是引擎写下的 `reason` 与事实摘要（`agent-loop.ts:395-404`）。
- 一次读**不是没有后果**：它照样走宿主、照样落一行审计加一条活动，并被计入命令输出、文件读取那些上限（见 [安全与数据边界](../security.md#安全与数据边界)）。

## 实现与钉住它的东西

| 位置 | 内容 |
| --- | --- |
| `packages/shared/src/types/risk.ts:37` | 五个风险等级折成三档的唯一实现 |
| `apps/agent/src/permissions/permission-engine.ts:84-152` | 三层事实合成、环境抬高与只读档豁免 |
| `apps/agent/src/permissions/command-risk.ts` | 16 条命令规则的等级表（最低 `medium`） |
| `apps/agent/src/tools/builtin/host-backed.ts:27` | 未声明风险的默认值 |
| `apps/agent/src/permissions/permission-engine.test.ts` | 「layer 1 only: a read tool on staging is automatic」「read-only modes still allow reads」「ask mode pauses before writes while keeping reads automatic」 |
