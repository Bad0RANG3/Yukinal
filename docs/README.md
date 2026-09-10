# Yukinal 文档

本目录保存跨模块、跨语言、跨版本仍然成立的说明：架构边界、协议契约、安全模型，以及做出这些选择的理由。

## 什么该写在这里，什么不该

写进 `docs/` 的内容需要满足至少一条：

- 它约束**两个以上模块**，读者无法只读一份代码就理解全貌（例如「Docker 工具名如何在 Provider 边界变形」）。
- 它是一个**公开契约**，改动会让另一侧编译或运行失败（例如 Tauri IPC 命令表、sidecar 的 JSON-RPC 方法名）。
- 它属于**安全模型**，行为是否正确无法只靠单侧测试判断（例如「谁能生成执行票据」）。
- 它解释**为什么是这个形状**，而这个理由在代码里只能看到结果、看不到权衡。

留在代码里的内容：

- 某个函数、某个常量、某条正则为什么这样写 —— 写在它旁边，用注释说明当下的理由。
- 一次性的排查结论、临时开关、尚未落地的想法 —— 不要进 `docs/`，那会让读者把「打算做」误读成「已经做」。
- 复述类型的字段清单 —— 类型本身就是契约，重复一遍只会产生两份会漂移的真相。

一条硬规则：**文档里写的每一句话都必须能在仓库里找到对应的实现或测试。** 如果某个能力只完成了一半，就写清楚哪一半完成了；不要用「支持 X」这种无法验证的措辞盖过去。实现范围变化时，同步更新相关文档，尤其是[项目 README](../README.md) 的「当前限制」一节。

## 新读者按什么顺序读

1. [项目 README](../README.md) —— 先知道这是什么、现在能做什么、还差什么。
2. [ADR 0001](adr/0001-agent-runtime-as-node-sidecar.md) + [ADR 0008](adr/0008-sidecar-launch-and-lifecycle.md) —— Agent 为什么是一个独立进程，以及谁负责它的生死。
3. [ADR 0006](adr/0006-sidecar-transport-ndjson-jsonrpc.md) —— 两侧到底怎么说话；这是调试时最先需要的东西。
4. [ADR 0005](adr/0005-permission-engine-sole-decision-maker.md) + [ADR 0009](adr/0009-agent-permission-delegation.md) —— 一次工具调用凭什么被执行，用户委托与模型文本的区别在哪里。
5. [ADR 0004](adr/0004-tool-name-mapping.md) + [Provider 边界](../apps/agent/src/providers/README.md) —— 模型看到的世界和内部审计记录的世界如何对齐。
6. [ADR 0002](adr/0002-ssh-backend-russh.md)、[ADR 0003](adr/0003-openai-compatible-only-for-mvp.md)、[ADR 0007](adr/0007-monorepo-and-day-one-abstractions.md)、[MCP 边界](../apps/agent/src/mcp/README.md) —— 按需要查阅。
7. 准备改协议或跨层类型时，先读 `packages/shared/src/` 与 `packages/shared/fixtures/ipc/`，它们是契约本身。

## 必须保持的架构边界

这些边界一旦被跨过，就会同时破坏可解释性和可测试性。改动它们需要先写一条新的 ADR。

- **React 只能使用 `packages/shared` 声明的 Tauri IPC 命令与事件。** `IPC_COMMANDS` 与 `EVENT_NAMES` 是白名单：不在其中的原生能力对界面不存在，界面也不能自己启动进程、建立 SSH 连接或读取凭据。契约的两半都有运行期闸门，都在 `apps/desktop/src/lib/ipc.ts`：命令走 `callDesktop()`（参数与返回值用 `IPC_SCHEMAS` 解析），事件走 `listenDesktop()`（负载用 `EVENT_SCHEMAS` 解析）。两者都是解析而不是类型断言——`terminal.data` 携带的是远端主机读回来的字节，正是不能靠断言的地方。事件是通知而非请求，负载校验失败只能丢弃并留下一次警告，不能被「重新请求」。
- **Rust 拥有原生资源。** SSH 会话、PTY、SQLite、操作系统凭据库和 sidecar 进程句柄都由 Rust 持有。`apps/desktop/src-tauri` 只做参数编组与事件转发，逻辑应落在 `crates/*`，这样不打开窗口也能测试。
- **`packages/shared` 是跨语言契约的唯一来源。** 事件或字段发生变化时，schema、Rust 序列化结构、转发层、界面消费者和两侧的 fixture 测试必须同时更新。`packages/shared/fixtures/ipc/` 下的 JSON 被 Rust（`include_str!`）和 TypeScript 同时解析，这是防止两侧静默漂移的机制。
- **ToolRegistry 是唯一的执行入口，Permission Engine 是唯一的授权决策入口。** 工具声明、命令分析和目标环境只提供风险事实；只有 `PermissionEngine.evaluate()` 能生成执行票据，`ToolRegistry.checkTicket()` 会复核工具名、目标、决策来源与审批 ID。模型输出不能伪造其中任何一层。
- **Agent 的自动委托范围是封闭的。** `auto` 只能覆盖开发或预发布目标上的普通写入；高危与 critical 操作、本机、未知和生产目标始终等待用户。任何 `deny` 都不能被运行模式或会话授权重新打开。
- **凭据只在宿主侧解析，只以一次性参数跨进程传递。** Provider 的 key 不进 sidecar 配置、日志和审计；工具参数与输出的敏感值在离开内存边界前被清理；sidecar 的 stdout 只承载协议帧，日志走 stderr。
- **所有外部内容都是不可信数据。** 服务器名称、日志、命令输出和远端文件正文可能包含针对模型的指令，不得拼进系统提示词当作可信上下文。
- **有界性是一条设计约束，不是实现细节。** 帧大小、命令输出、文件读取、日志行数、审计条数、审批等待时长、单次运行的步数与墙钟时间都必须有明确上限，并且上限要写在文档里。

## ADR 索引

| 编号 | 决定 | 状态 |
| --- | --- | --- |
| [0001](adr/0001-agent-runtime-as-node-sidecar.md) | Agent Runtime 作为独立 Node.js sidecar，由 Rust 拥有其生命周期 | Accepted |
| [0002](adr/0002-ssh-backend-russh.md) | SSH 后端采用 russh，并封装在 `crates/ssh` 的 `SshBackend` 之后 | Accepted |
| [0003](adr/0003-openai-compatible-only-for-mvp.md) | 只实现一个 OpenAI-compatible Provider，两种请求方言覆盖兼容端点 | Accepted |
| [0004](adr/0004-tool-name-mapping.md) | 内部工具名用点号，Provider 边界用双下划线，映射集中在一处 | Accepted |
| [0005](adr/0005-permission-engine-sole-decision-maker.md) | Permission Engine 是唯一的执行授权决策者 | Accepted |
| [0006](adr/0006-sidecar-transport-ndjson-jsonrpc.md) | sidecar 通过 stdio 上的 NDJSON JSON-RPC 通信，协议版本 `1.0` | Accepted |
| [0007](adr/0007-monorepo-and-day-one-abstractions.md) | pnpm 与 Cargo 双 workspace，并优先稳定变化最频繁的边界 | Accepted |
| [0008](adr/0008-sidecar-launch-and-lifecycle.md) | Rust 负责 sidecar 的启动、握手、监督与回收 | Accepted |
| [0009](adr/0009-agent-permission-delegation.md) | Agent 权限使用显式的运行级委托，与运行模式互相正交 | Accepted |

## 更新文档

- 新增一条 ADR 时使用下一个编号（四位、零填充），文件名保持 `NNNN-短横线标题.md`，并在[架构决策记录索引](adr/README.md)与本文件上方表格中各补一行。
- 每条 ADR 必须包含 `Status:`、`Date:` 以及 `Context`、`Decision`、`Consequences` 三节；有真实取舍时补上 `Alternatives considered`。
- 已有 ADR 不应被改写来掩盖变化。决定变了，就新增一条 ADR，并在旧记录的状态里标明被哪一条取代。
- 文档中的相对链接必须指向仓库里真实存在的文件；`node scripts/check-publication.mjs` 会检查措辞，链接是否有效则需要人工确认。
