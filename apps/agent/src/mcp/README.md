# MCP 边界

**MCP 已接入（ADR 0014），但边界与之前写下的那份约束完全一致。** 这个目录里有代码，而且只有两类：把宿主交来的目录变成一个 `Tool`（`tool.ts`），以及把它注册进 `ToolRegistry`（`catalog.ts`）。**这个目录里没有任何进程、没有任何 MCP 协议实现**：进程、`initialize` 握手、`tools/list`、`tools/call`、超时、退出记录、stderr 尾部都在 `crates/core/src/mcp/`，配置与目录在 `apps/desktop/src-tauri/src/commands/mcp.rs`。

本文件说明三件事：**现在到底有什么**，**那些约束是怎么被满足的**，以及**哪些仍然没有做**。第 45 行以下那十条约束没有变，它们是这个功能的设计依据，不是历史。

## 当前已存在的东西

| 位置 | 内容 |
| --- | --- |
| `crates/core/src/mcp/` | MCP stdio 客户端：`initialize` / `tools/list` / `tools/call`、每请求超时、退出记录、stderr 尾部（截断并脱敏） |
| `crates/core/tests/mcp_stdio.rs` | 针对已提交 Node fixture（`crates/core/tests/fixtures/mcp-server.js`）的集成测试 |
| `apps/desktop/src-tauri/src/commands/mcp.rs` | `mcp_server_list` / `mcp_server_save` / `mcp_server_delete` / `mcp_server_start` / `mcp_server_stop`；`host.mcp.catalog`；`mcp.` 前缀的执行分流 |
| `packages/shared/src/types/host.ts` | `HOST_METHODS.mcpCatalog`、`HostMcpCatalog*`、`MCP_CATALOG_FAILURE_CODES` |
| `packages/shared/src/types/mcp.ts` | 设置界面看到的形状（`McpServerView` / `McpServerStatus` / `McpToolDescriptor`…） |
| `apps/agent/src/mcp/tool.ts` | 目录条目 → `Tool`：风险、超时、本地输入 schema、`origin` |
| `apps/agent/src/mcp/catalog.ts` | 取目录（`loadCatalogFromHost`）与注册（`registerCatalog`） |
| `apps/agent/src/tools/registry.ts` | `Tool.origin` → `ToolDeclaration.origin`，并强制 `mcp.` 命名空间与 `origin: mcp` 双向一致 |
| `packages/shared/src/types/tool.ts` | `ToolOrigin` 包含 `{kind: "mcp", serverId}`（本来就有，现在真的被填上了） |

## 现在到底有什么（运行期事实）

- 界面能配置、启动、停止、删除 stdio MCP 服务器，并看到 pid、协议版本、工具数量、退出记录与脱敏后的服务器输出。
- agent 启动时向宿主取一次目录，把每个工具注册成 `mcp.<服务器段>.<工具段>`（ADR 0004），失败与拒绝逐条记日志。
- 每个 MCP 工具的 `risk` 是 `critical`，`origin` 是 `{kind: "mcp", serverId}`；调用走 `host.tool.execute`，与内置工具同一条路径。

明确**没有**的东西：

- 没有 `http` / SSE 传输：`McpStdioConfig` 在类型层面拒绝它，保存一个 `http` 行会被拒绝，表里已有的 `http` 行在所有路径上都得到同一句理由（出站网络策略不存在）。
- 没有崩溃自动重启：崩掉的服务器只被报告，`mcp_server_start` 是唯一的重启路径（ADR 0014）。
- 没有信任评审流程，因此 `allowedTools` 与 `trustLevel` 仍然只是被存储、没有被读取。
- 没有真实第三方 MCP 服务器的验证：全部测试跑在已提交的 Node fixture 上。

## 握手会报告什么

`initialize` 的结果里包含四项能力：

```json
{
  "streaming": true,
  "toolCalling": true,
  "cancellation": true,
  "mcp": true
}
```

`mcp` 的取值**必须来自注册表的实际内容**（是否真的注册了 `origin.kind === "mcp"` 的工具），而不是来自意图：拿不到目录（老宿主、宿主不可达）时必须保持 `false`。

有一处顺序上的事实要写清楚：目录是在 stdio RPC 起来**之后**取的（宿主在完成握手前不会转发 sidecar 请求），所以 `initialize` 回答时目录通常还没到，该字段可能仍为 `false`。它是「此刻这件事是否成立」的实时读数，`agent.list_tools` 与 `system.describe` 是它的补充。

## 接入时必须遵守的约束（逐条对照现在的实现）

以下十条是这个功能落地**之前**就写在这里的设计依据。它们没有变。每一条后面补的是现在的实现怎么满足它 —— 以及哪一条被有意按更严的方向读。

### 1. 外部工具必须先变成 Yukinal 的工具声明

```text
MCP Server → MCP Client → 适配器 → ToolRegistry（外部工具变成 ToolDeclaration）
          → Permission Engine → ExecutionTicket → 执行 → 事件与审计
```

远端服务不能替代本地的名称、风险、schema、目标或授权决策。适配器负责把外部工具翻译成 `Tool` 实现，此后它走的是与内置工具完全相同的路径：`ToolRegistry.register()` 会校验名称合法性、描述非空、`timeoutMs > 0`，并在注册时就拒绝不合规的声明。

**现在：** `apps/agent/src/mcp/tool.ts` 是唯一的适配器，`mcpToolFromCatalog()` 产出一个普通的 `Tool`。除 `origin` 之外它没有任何特殊待遇：权限、票据、超时竞赛、取消、trace、审计全部走既有路径。

### 2. 命名空间与名称冲突

- 内部名称使用 `mcp.<serverId>.<tool>`，再按 [ADR 0004](../../../../docs/adr/0004-tool-name-mapping.md) 映射成 Provider 侧的双下划线形式。
- 名称必须满足内部规则：各段匹配 `^[a-z][a-z0-9]*(-[a-z0-9]+)*$`、至少两段、不含双下划线。这意味着 `serverId` 与外部工具名都必须先被规范化，不能直接沿用远端拼写。
- 映射后的 Provider 名称不得超过 64 字符。
- 注册前必须运行冲突检查。外部工具可能带来 `docker__get` 这样的拼写，它与内部的 `docker.get` 会投影到同一个 Provider 名称——这类遮蔽必须在注册期被拒绝，而不是等到调用时才发现。

**现在：** 规范化在宿主侧完成（`crates/core/src/mcp/descriptor.rs` 的 `normalize_segment()` / `internal_tool_name()`），内部名由 `host.mcp.catalog` 交过来，agent 不重新拼。两条 id 会规范化成同一个段时，宿主只让其中一个进目录，另一个报告成 `invalid_config` 并说明原因（`commands/mcp.rs` 的段撞车检查）。注册冲突（例如某个服务器声明了一个与内置工具同名的工具）由 `registry.register()` 拒绝，并被 `registerCatalog` 记成 `rejected`，只影响那一个工具。

### 3. 风险等级由本地决定

- 外部工具默认至少为 `medium`，不采信远端声明的风险等级。远端可以声称自己是只读的，但那只是远端的一面之词。
- `allowedTools` 为空表示**什么都不自动信任**：用户需要先看到工具描述，再逐个选择启用；`trustLevel` 为 `unreviewed` 的工具不应被自动注册。
- 危险档位的规则不变：`high`/`critical` 永远不会被自动批准，也永远不会被会话授权记住。

**现在：** 比上面写的更严，而且第二点被**有意按另一条路读**，值得说清楚：

- 档位不是 `medium` 而是 `critical`（`MCP_TOOL_RISK`）。`McpToolDescriptor` 里根本没有风险字段或注解，适配器也不读 —— MCP 的工具注解是服务器对自己的一面之词，用降档来「利用」它等于把授权交给被授权方。`critical` → 档位 `dangerous` → 权限引擎在**任何** `permissionMode`（包括 `auto`）下都要求用户逐项批准，`grantSession()` 拒绝它，`plan` / `readonly` 直接拒绝。
- **`unreviewed` 的工具确实被注册了**，但它们没有被授信：注册一个工具只让它出现在模型可用的工具列表里，而每一次调用都要用户逐项批准。也就是说「不自动信任」在这套模型里由**权限引擎**保证，而不是由「不注册」保证 —— 后者在没有评审流程的今天等于让 MCP 完全不可用（`allowedTools` 永远是空表，`trustLevel` 永远是 `unreviewed`，没有任何界面会改它们）。这是一个显式的取舍，不是遗漏：**如果将来出现了一个「用户看过描述并降低某个工具档位」的流程，这里就是它该接入的地方**，而那一天到来之前，任何 MCP 工具都不该有一个比 `critical` 更低的值。

### 4. 输入、输出与超时由本地强制

- 工具的输入 schema 必须由本地 Zod schema 定义（`Tool.input`），模型看到的 JSON Schema 由它派生。远端声明只能作为翻译来源，不能直接成为校验依据；校验失败一律得到 `invalid_input`。
- `timeoutMs` 必须为正数。注册表会把工具执行与取消信号做竞速，忽略取消的工具会被判为超时或已取消——外部进程因此不能把一次运行永久挂住。
- 输出在回传模型、界面和审计之前要经过敏感值清理，摘要上限 4000 字符，命中敏感标记时整体替换为「已省略」。
- `plan` 与 `readonly` 运行模式下，任何非只读档位的外部工具调用都会被权限引擎直接拒绝。

**现在：** `Tool.input` 是 `z.record(z.string(), z.unknown())`（`MCP_TOOL_INPUT`）：本地 schema 只保证「是一个对象」，服务器的 `inputSchema` 被追加到**描述**里作为文档（有长度上限）—— 拿模型或服务器能影响的文档去校验模型输入是一个带额外步骤的验证漏洞。`timeoutMs` 是 45 秒，比宿主自己的每请求上限（30 秒）更长，这样先放弃的是宿主而不是注册表（否则 agent 会报超时而服务器还在跑）。`retry.maxAttempts` 是 1：一次超时的 MCP 调用可能已经产生了副作用，重试它等于把副作用做第二遍。输出摘要仍然走 `summarize()` 的 4000 字符上限与敏感值清理。

### 5. 目标必须在本地解析

- 每次调用都携带一个已解析的 `ToolTarget`（host、serverId、workspaceId、environment），工具不得自己重新猜测服务器。
- 如果外部工具需要访问远端资源，它必须经由宿主的 `host.tool.execute` 路径：宿主只接受 `host: "remote"` 且 `serverId` 以 `srv_` 开头的目标，并核对目标环境与工作区归属。
- 远端描述文本不能改变目标，也不能把一次调用重定向到别的服务器。

**现在：** 目标照旧由调用方解析后传进来，MCP 适配器原样转发给 `host.tool.execute`，自己不看也不改。宿主侧的 `mcp.` 分流发生在**目标校验之前**，因为一次 MCP 调用打给的是本机派生的服务进程，不是某个 SSH 服务器 —— 这一点在 `commands/host.rs` 里写明了。

### 6. 描述文本一律视为不可信数据

工具描述、服务端返回的文本和错误信息都可能包含针对模型的指令。它们可以作为工具说明展示给用户，但不得直接拼接进系统提示词充当可信上下文。

**现在：** 描述文本（服务器自己的话 + 它的 `inputSchema`）确实进入了工具描述，而工具描述会被模型读到 —— 这一条约束的含义是「视为不可信数据」，不是「不许出现」。当前的做法：描述有长度上限（防止一个敌意 schema 挤掉整个上下文），`tools/call` 返回的文本走 `ToolFailure` 与摘要清理，工具级错误以 `execution_failed` 回给模型并保留服务器原话（那是可行动的），一行都不拼进 system prompt。

### 7. 进程生命周期仍然归 Rust

`McpServerConfig` 里有 `command` 与 `args`，说明 `stdio` 传输意味着要派生进程。当前架构里**唯一**派生进程的地方是 Rust 宿主（见 [ADR 0001](../../../../docs/adr/0001-agent-runtime-as-node-sidecar.md) 与 [ADR 0008](../../../../docs/adr/0008-sidecar-launch-and-lifecycle.md)）：sidecar 只通过 `host.tool.execute` 请求宿主执行操作。因此：

- 由 sidecar 自行派生 MCP 服务进程会打破「Rust 拥有进程」这条边界，需要先写一条新的 ADR 说明理由与该进程的回收、日志和崩溃语义。
- 无论由谁派生，都必须有明确的单实例、超时、回收与退出诊断规则；孤儿进程是不可接受的。
- `http` 传输意味着 sidecar 需要主动对外发起网络请求。目前没有任何出站网络策略，接入前必须定义允许的目标范围、TLS 要求与失败语义。

**现在：** 这三条全部成立，而且**没有**新写一条 ADR（ADR 0014 是 MCP 接入本身的那条，不是「打破 Rust 拥有进程」的那条 —— 它没有打破任何东西）：进程由 `crates/core/src/mcp/` 的 `McpSupervisor` 派生与回收，`AppState.mcp` 持有它，agent 侧一行 `spawn` 都没有。`http` 被类型层面拒绝，出站网络策略仍然不存在，所以那一条仍然是「接入前必须先定义」的状态（也就是说：没有接入）。

**已知缺口（已修）：** 宿主正常退出路径（`lib.rs` 的 `RunEvent::ExitRequested`）现在会调用 `state.mcp.shutdown_all()`，一台服务器关不掉不影响其余几台；`kill_on_drop` 只作为兜底。见 `CHANGELOG.md` 的「修复」一节。

### 8. 不要暗示已经可用

- 不要添加「兼容调用」的占位实现、假的工具列表或只读的界面入口，让用户以为 MCP 已经连上。
- 只要 `capabilities.mcp` 仍是 `false`，文档与界面就应该照实说。能力报告必须是事实，而不是意图。

**现在：** 界面上的每一句话都只描述「这台进程现在在不在」；没有工具列表就不显示工具数量，崩了就显示退出原因与时间，并且明说不会自动重启（「启动」是用户的动作）。能力报告按注册表实况取值，取不到目录就保持 `false`。

### 9. 数据库变更走新迁移

`mcp_servers` 表在初始迁移里就已建立。已发布的迁移不得修改，新增字段必须追加一条新迁移（`crates/database/src/schema.rs` 顶部的规则），并同步更新 Rust 模型与 `McpServerConfig`。

**现在：** 本次没有改表结构，所以没有新迁移。数据库侧新增的唯一一样东西是 `McpServersRepository::get()`（`crates/database/src/repositories/providers.rs`）。

### 10. 新增界面要补齐契约

如果将来要让用户配置 MCP 服务器，需要同时增加：`IPC_COMMANDS` 中的命令、`packages/shared/src/schemas/` 下的 Zod schema、`fixtures/ipc/` 下的契约样例（Rust 与 TypeScript 两侧都会解析它们），以及 Rust 侧的命令与仓库方法。只加一侧会让这个契约在运行时不成立。

**现在：** 这条已经补齐，而且两侧都补齐了。Rust 侧五个命令与仓库方法、Zod schema（`packages/shared/src/schemas/mcp.ts`）、`IPC_COMMANDS` / `IpcCommandMap` / `IPC_SCHEMAS` 里的五条 MCP 条目、`fixtures/ipc/` 下的五份样例（`mcp_server_list/save/delete/start/stop.json`），以及界面代码（`apps/desktop/src/lib/mcp.ts`、`apps/desktop/src/features/settings/McpSettings.tsx`）都已就位，设置面板已挂进设置页（`RuntimeSettings.tsx`）。
