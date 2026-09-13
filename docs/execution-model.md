# 执行与授权模型

```text
用户请求（React）
   │  agent_run_start：prompt · target · permissionMode · mode
   ▼
Rust 宿主解析 Provider 与凭据（SQLite 行 + OS 凭据库）
   │  agent.run.start：providerConfig 随这一次请求下发
   ▼
组装上下文（ContextEngine：工作区 / 服务器 / 最新快照）
   │
   ▼
调用模型（SSE 流式输出）
   │  文本增量 ────────────────────────────────► 界面 agent.thinking
   ▼
工具调用请求
   │
   ▼
Permission Engine 决策（工具风险 × 命令风险 × 目标环境）
   ├─ deny ─► 拒绝结果回灌模型，不执行
   ├─ ask ──► agent.waiting_approval ──► 用户批准 / 拒绝 / 超时过期
   │                                    └─ approve_session 授予本次会话
   └─ auto ─► 自动执行（记录来源：policy / agent / user）
   │
   ▼
ToolRegistry 校验 ticket（工具名 · 目标 · 决策来源 · 审批 ID）
   │  host.tool.execute（JSON-RPC 请求发给 Rust 宿主）
   ▼
Rust 宿主在已解析的目标上执行受限操作（SSH 命令 / SFTP / Docker）
   │
   ▼
结果回灌模型进入下一轮 · agent.tool_result → SQLite 审计 + 活动记录 + 界面
```

这条链路遵循三项原则：

1. **先说明影响，再执行会改变状态的操作。** 只读工具可以直接执行；写入、重启、部署类操作会暂停并等待明确批准。
2. **每一步都可见。** 模型文本、工具调用、风险事实、决策结果、审批与执行结果都以事件形式流到界面，并被写入审计表；没有「黑箱里已经做完了」的路径。
3. **Permission Engine 是唯一的授权决策入口。** 工具声明、命令分析、目标环境只生产风险**事实**；只有 `PermissionEngine.evaluate()` 能把事实加上用户在运行级给出的委托，变成一个可执行的决策。模型文本永远不能等价于一次授权。

决策由三层事实合成，过程是确定的：工具自身声明的静态风险（`ToolDeclaration.risk`）与命令分析的结果（`analyzeCommand()`，16 条规则，规则如 `rm-rf`、`drop-database`、`curl-pipe-shell`）取较大者作为内在风险；目标环境的风险下限（本机与开发 `low`、预发布 `medium`、生产与未知 `high`）再**只向上抬高**它——读类动作在生产仍保持读类，否则「生产环境只读自动执行」这条策略永远无法成立。合成后的档位折叠为 `read` / `write` / `dangerous` 三档，由目标环境选出一张内建策略表，最后施加不可绕过的约束。

**档位的规则不写在这一份里，各自成篇。** 三档的区别不在策略表的某一格，而在引擎的强制分支、闸口的票据复核和运行模式三处各自的形状，所以每一档写成一整份：

| 档位 | 由哪些风险等级构成 | 谁能让它执行 | 文档 |
| --- | --- | --- | --- |
| `read` | `read`、`low` | 只有环境策略（四张内建策略表在这一栏都是 `auto`）；`plan` / `readonly` 运行也放行它 | [权限档位：read](./risk-tiers/read.md) |
| `write` | `medium` | 策略自动、运行级委托（限开发与预发布）、会话授权、逐项批准四种都可能；`ask` 批准方式与只读运行会挡住它 | [权限档位：write](./risk-tiers/write.md) |
| `dangerous` | `high`、`critical` | **只有逐项批准**：策略说 `auto` 也会被改回 `ask`，闸口拒绝其余三种票据，会话授权既不记也不覆盖它 | [权限档位：dangerous](./risk-tiers/dangerous.md) |

三档共享的规则留在这里，它们是同一条链路的不同段。

**授权结果有四种 ticket 来源**，ToolRegistry 会逐一复核，任一字段不匹配就拒绝执行：`policy_auto`（环境策略自动批准，来源必须标记 `policy`）、`agent_auto`（用户在运行级委托 Agent，档位必须是 `write` 且环境是开发或预发布）、`session_auto`（用户在本会话批准过同名同目标的操作，来源必须标记 `user`）、`user_approved`（与挂起审批的 ID 完全匹配的逐项批准）。所有 ticket 还须满足工具名与目标四元组（host、serverId、workspaceId、environment）与决策完全一致。任一字段不匹配得到的是 `denied_by_policy` 的工具结果，而不是一条被忽略的日志。哪一种来源能用在哪个档位上，写在各自那份文档里。

**会话授权的作用域是单次运行**：引擎实例比单次运行长命，所以 loop 在没有运行在飞时调用 `clearGrants()`；没有这一步，「批准本会话」会一直授权到 sidecar 进程退出，跨越多次无关运行。它始终绑定具体的工具与目标。

**只读运行模式先于一切委托。** `plan` / `readonly` 下任何非 `read` 档位直接 `deny`，判断发生在策略、委托和会话授权**之前**，所以限制是被执行的，不是被请求的。运行级委托 `auto` 的范围是封闭的：只有档位是 `write` 且目标环境是 `development` 或 `staging` 时结果才变成自动，其余回落 `ask`。任何 `deny` 都不能被运行模式或会话授权重新打开。
