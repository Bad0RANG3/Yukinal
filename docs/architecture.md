# 架构总览

```text
┌───────────────────────────────────────────────────────────────────┐
│ React 19 + Vite（apps/desktop/src）                                │
│ 服务器 · 概览 · 终端 · 文件 · 日志 · 服务 · 活动 · Agent 面板        │
└──────────────────────────────┬────────────────────────────────────┘
                               │ 白名单 Tauri IPC：命令 + 事件
┌──────────────────────────────▼────────────────────────────────────┐
│ Rust 宿主（apps/desktop/src-tauri + crates/*）                     │
│ commands：服务器 · 终端 · 文件 · 日志 · 服务 · 活动 · Provider · MCP · Agent│
│ core：SSH · PTY · 采集 · SQLite · OS 凭据库 · sidecar 启动与监督      │
└───────┬───────────────────────────────────────┬───────────────────┘
        │ SSH / SFTP / PTY                      │ stdio 上的 NDJSON JSON-RPC
        ▼                                       ▼
┌───────────────────┐              ┌─────────────────────────────────┐
│ 远程服务器         │              │ Node.js Agent sidecar           │
│ systemd / Docker   │              │ agent loop · tools · 权限引擎    │
└───────────────────┘              │ providers/                       │
                                   └───────────────┬─────────────────┘
                                                   │ HTTPS + SSE
                                                   ▼
                                       ┌───────────────────────────┐
                                       │ OpenAI-compatible 端点     │
                                       │ api.anthropic.com          │
                                       │ generativelanguage...      │
                                       └───────────────────────────┘
```

**通信方式。** Rust 启动 sidecar 并持有句柄，双方通过 stdin/stdout 传 NDJSON 编码的 JSON-RPC 2.0，每行一个完整帧，协议版本常量 `1.0`（`packages/shared/src/protocol/jsonrpc.ts` 与 `crates/core/src/sidecar/mod.rs` 各有一份，必须一致）。`initialize` 必须是第一条请求，握手成功后 Rust 还会调用 `system.describe`，协议版本不匹配或报告存在工具名冲突时**拒绝发布这个 sidecar**。反向请求复用同一条流：`host.tool.execute`、`host.context.fetch`、`host.tool.cancel`、`host.mcp.catalog` 都由 Rust 处理。

sidecar 的 stdout 只承载协议帧，**所有日志写 stderr**——一条走错位置的 `console.log` 就会破坏协议。host 与 sidecar 之间的单帧上限是 24 MiB（多模态附件的 base64 可能很大；MCP 自己的帧上限仍是 8 MiB），超限帧被丢弃并记录；畸形帧不会杀死进程，只会被报告并跳过；`agent.run.start` 的响应帧必须先于该次运行的任何通知发出，否则界面会先看到事件后拿到 `runId`；带 `messageId` 的 `run.start` 记入一张最多 256 条的受理表，同消息重发且内容一致返回同一个 `runId`（`duplicate: true`），已有受理记录不会被淘汰。宿主侧还有第二道闸门：只转发白名单事件类型、要求 `runId` 非空且不超过 256 字符、单个负载限制 1 MB（比传输层更紧）。

**分层的实际约束。** 这些边界一旦被跨过，就会同时破坏可解释性和可测试性；改动它们需要先写一条新的 ADR。

- **React 只能使用 `packages/shared` 声明的 Tauri IPC 命令与事件。** `IPC_COMMANDS` 与 `EVENT_NAMES` 是白名单：不在其中的原生能力对界面不存在，界面也不能自己启动进程、建立 SSH 连接或读取凭据。契约的两半都有运行期闸门，都在 `apps/desktop/src/lib/ipc.ts`：命令走 `callDesktop()`（参数与返回值用 `IPC_SCHEMAS` 解析），事件走 `listenDesktop()`（负载用 `EVENT_SCHEMAS` 解析）。两者都是解析而不是类型断言——`terminal.data` 携带的是远端主机读回来的字节，正是不能靠断言的地方。事件是通知而非请求，负载校验失败只能丢弃并留下一次警告，不能被「重新请求」。
- **Rust 拥有原生资源，Tauri 命令层只做宿主编排。** SSH 会话、PTY、SQLite、操作系统凭据库、sidecar 与 MCP 子进程句柄都由 Rust 持有。`apps/desktop/src-tauri` 负责 IPC 参数校验、数据库与资源管理器之间的编排以及事件转发；可复用的协议、传输和领域算法落在 `crates/*`，这样不打开窗口也能测试——`yukinal-core` 里没有 Tauri 类型，sidecar 的启动、监督、崩溃与状态路径都能单测覆盖。
- **`packages/shared` 是跨语言契约的唯一来源。** 类型、Zod schema、IPC 映射、事件名、JSON-RPC 协议、工具命名规则都在这里；`packages/shared/fixtures/ipc/` 下的 JSON 被 Rust（`include_str!`）和 TypeScript 同时解析，这是防止两侧静默漂移的机制——类型只保证编译期一致，fixture 保证运行时一致。本地门禁的第一步就是构建这些契约库（消费方导入它们的 `dist/*.d.ts`），顺序不能颠倒。
- **ToolRegistry 是唯一的执行入口，Permission Engine 是唯一的授权决策入口。** 详见 [执行与授权模型](./execution-model.md#执行与授权模型)。
- **Agent 不直接执行远程操作。** sidecar 通过 `host.tool.execute` 向宿主提出请求，宿主会重新校验目标服务器 ID、环境、工作区归属和文件路径策略，然后才执行；副作用工具还必须带完整的 durable `taskId`、`planId`、`planStepId`，普通聊天不能降级成自动写入。sidecar 自己不连 SSH、不访问 SQLite 与凭据库。交互式终端是独立的用户人工路径，不是 Agent 工具。
- **Provider 差异被关在 Provider 边界内。** agent loop 只依赖 `LLMProvider` 与统一的 `StreamEvent`，不根据 Provider 身份分支；工具名的点号与双下划线转换也只发生在一个地方。
- **有界性是一条设计约束，不是实现细节。** 帧大小、命令输出、文件读取、日志行数、审计条数、审批等待时长、单次运行的步数与墙钟时间都必须有明确上限，并且上限要写在文档里（见 [安全与数据边界](./security.md#安全与数据边界)）。
- **决策摘要只能用显式 continuation 续接任务。** 选项省略 continuation 时宿主归一化为 `wait_user`；只有 `continue_readonly` 或 `start_plan` 的用户选择才会让桌面调用既有任务启动入口，`stop` 则由宿主封存任务并清除活动运行栅栏，任务详情页的 `investigation_task_stop` 也复用这条链路，不能由自然语言或旧摘要隐式启动或停止。

**Rust 生命周期契约。** 安全 Rust 排除了 use-after-free、double free 和数据竞争，但不会替业务决定对象何时释放、任务何时取消或外部句柄何时回收。需要跨网络等待、退避或长期运行的后台任务不能把所有者 `Arc` 作为隐形保活根：任务只持 `Weak`、独立状态快照或配置快照，进入同步临界区时才短暂升级；任务必须有取消令牌或关闭通知；所有者需要提供显式 `shutdown`/`close`，`Drop` 只负责同步兜底，不替代异步关闭协议。子进程、PTY、SSE 和 keepalive 都必须在所有者释放或显式停止时收口。释放本地内存、收到取消、停止本地任务和撤销远端副作用是四种不同事实，状态与错误文案不能混为一谈。

这条契约已落实在 MCP HTTP GET 流、MCP/sidecar supervisor、终端 forwarder、SSH keepalive、Collector 本地/SSH runner 与桌面后台任务中，并由对应的本地回归和 WSL OpenSSH 回环测试覆盖；远程公网主机、第三方服务取消语义和长时间资源曲线仍属于未验收边界，见 [当前限制](./limitations.md#当前限制)。

**动作重投保护。** 对文件写入、编辑、宿主生成备份、守卫恢复、容器重启和 MCP 调用，Rust 宿主在实际执行前以 traceId + callId 写入一次 SQLite 占位和请求指纹；绑定持久化计划时还比较不含 provider 身份的逻辑动作指纹。当前进程可以重放有界的已完成响应；运行中、响应丢失或结果不确定的调用只返回 duplicate_call 计划偏离，不能因为 sidecar 重启、换 callId 或界面重连而再次触达远端。文件备份另有只存元数据的归属账本，恢复必须命中同服务器、目标和任务的可用记录并在成功后消费它；账本不保存远端原文，外部网络和真实部署仍按未验证能力处理。

**调查上下文与调度预算。** 宿主给 sidecar 的 `host.context.fetch` 只投影证据元数据和阶段工件摘要，不跨边界携带正文；任务证据再由 `investigation.evidence.search` 返回有界的元数据摘要。需要先对齐同一轮的多来源资料时，`investigation.evidence.correlate` 只按同一宿主运行 ID，或旧证据的受限时间窗，返回同一任务/目标范围内的摘要、来源集合和警告；它明确标记匹配方式，不做语义根因推断。需要解释两份样本时，`investigation.evidence.compare` 只在同一任务、同一目标范围内读取两条已保存证据，返回宿主计算的 JSON 变化路径或文本行数，以及来源/新鲜度警告，不返回原始值；只有模型随后用真实 `evidenceId` 调用 `investigation.evidence` 才能取回单条脱敏正文。这些本地只读路径不会伪造现场采样或推进计划。所有宿主展示路径都按 `default-v1` 计算新鲜度（15 分钟后 `stale`、24 小时后 `expired`，非法/未来时间戳为 `unknown`），把评估时刻和边界一起返回；它是动态只读投影，旧证据仍保留，Agent 不能提交或覆盖这个字段。持久化调度器在启动 sidecar 前由 Rust 对任务预算和调度预算逐字段取小值，调度规则可以缩短一次运行，但不能通过配置扩大任务原本允许的步数、墙钟时间或尝试次数。

**迟到事件栅栏。** 事件转发到任务状态账本前，宿主要求事件的 `runId` 仍是任务的 `active_run_id`；恢复、终态收口或重试换 run 后，旧 sidecar 的迟到帧只能作为被丢弃的噪声。已终止的 run（包括 `interrupted`）也不接受任何后续事件，事件携带的 `taskId` 必须与持久化 run 归属相同。这样停止/恢复不会被传输时序反向打开，旧 run 仍保留供审计查询。

**失败后的恢复入口。** `investigation_task_recover` 先在宿主事务中关闭旧 run、步骤、计划审批、基线和观察窗口，再返回 `investigating`/`recovery` 或明确的等待/停止状态。桌面端只有收到 `investigating` 且没有 `activeRunId` 时，才自动调用统一的 `investigation_task_start`；重试、重新规划和继续恢复因此不会要求用户复制目标到聊天框，回退、停止和等待用户仍不会隐式启动。启动失败保持可见错误，不能把恢复请求伪装成已运行。

**异常后的只读复核。** 持久化调度器把上一轮宿主比较结果（`baseline`、`no_change`、`changed` 或 `insufficient_evidence`）转换成下一轮的有界提示。`changed` 要求 sidecar 先检索相邻证据、用宿主的 `investigation.evidence.compare` 查看无正文差异、再在原范围内采集新样本；它不会扩大工具、预算、目标或权限，也不会把差异升级成写入授权。复核仍通过同一条 sidecar、Permission Engine、计划和证据账本路径。

### 仓库地图

```text
apps/
  desktop/           Tauri 2 外壳：React 界面 + src-tauri 命令层
    src/             AppShell、features/*、components/*、stores/*、lib/ipc.ts、lib/labels.ts、lib/runtime.ts、lib/markdown/
    src-tauri/       Tauri 命令、AppState、窗口与事件转发
    tests/           UI 逻辑单元测试
  agent/             Node.js Agent sidecar
    src/runtime/     agent loop、运行状态机、提示词、运行时装配
    src/tools/       工具抽象、registry、内置工具
    src/permissions/ 权限引擎与命令风险规则
    src/providers/   三种协议的适配器（OpenAI-compatible / Anthropic / Gemini）
    src/context/     上下文集装（宿主数据源 + 空数据源）
    src/rpc/         JSON-RPC 方法分发与 Provider 装配
    src/transport/   stdio 传输、宿主 RPC 客户端
    src/security/    敏感数据清理
    src/mcp/         MCP 工具适配：目录客户端、命名空间与风险档位
    src/trace/       执行追踪账本
packages/
  shared/            跨层契约：types、schemas、ipc、events、protocol、naming、fixtures
  provider-sdk/      LLMProvider 抽象与 Provider 侧工具名映射
  agent-sdk/         sidecar JSON-RPC 类型化客户端
crates/
  core/              sidecar 启动与监督、宿主 IPC 类型、MCP 客户端与工具目录、Provider 选择与请求头消毒、身份命名规则、Docker 行解析、日志脱敏、采集编排、PTY 服务
  ssh/               russh 连接、认证、known_hosts、命令、PTY、SFTP
  terminal/          多会话 PTY 路由与事件归一
  collector/         7 个采集器与本地/SSH runner
  credentials/       OS 凭据库抽象（keychain 引用）
  database/          SQLite schema、迁移、model（按域拆开）与 repository
  filesystem/        远端文件策略与上限（凭据路径黑名单、有界读写、宿主备份/恢复），传输由桌面层注入
  time/              时间戳的唯一实现
docs/
  README.md          文档索引与文档治理规则
  architecture.md    本文：链路与仓库地图
  execution-model.md 授权与票据的共享规则
  risk-tiers/        三档权限文档（read / write / dangerous）
  boundaries/        三个跨模块边界（Provider / MCP / Markdown 渲染）
  security.md        安全与数据边界
  limitations.md     当前限制与有意为之的边界
  development.md     开始开发、验证命令与首次使用引导
  packaging.md       打包与分发
  adr.md             架构决策记录 0001–0070
  changelog.md       版本与发布历史
scripts/             校验、构建辅助、sidecar smoke、打包、桌面窗口检查
```
