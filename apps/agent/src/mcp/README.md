# MCP 边界

**MCP 尚未实现。** 本目录下没有代码，仓库中也没有 MCP 客户端、MCP 工具注册或 MCP 服务进程管理。sidecar 在握手时报告的能力里 `mcp` 为 `false`（`apps/agent/src/rpc/router.ts`），这个值必须保持真实，直到真的有实现为止。

本文件说明两件事：**现在到底有什么**，以及**将来接入时必须遵守哪些已存在的约束**。它不是一份计划，也不代表 MCP 可用。

## 当前已存在的东西

只有数据契约与存储位，没有任何执行路径：

| 位置 | 内容 |
| --- | --- |
| `packages/shared/src/types/provider.ts` | `McpServerConfig` 类型：`id`、`label`、`transport`（`stdio` 或 `http`）、`command`、`args`、`url`、`enabled`、`allowedTools`、`trustLevel`（`reviewed` 或 `unreviewed`） |
| `crates/database/src/models.rs` | 同一结构的 Rust 模型 |
| `crates/database/src/schema.rs` | `mcp_servers` 表（随初始迁移建立） |
| `crates/database/src/repositories/providers.rs` | `McpServersRepository`：`upsert` / `list` / `delete` |
| `crates/database/tests/persistence.rs` | 一次读写往返测试 |
| `packages/shared/src/types/tool.ts` | `ToolOrigin` 已经包含 `{kind: "mcp", serverId}`，用于标记工具来源 |
| `packages/shared/src/schemas/permission.ts` | `ToolOriginSchema` 接受同样的 `mcp` 来源 |
| `packages/shared/src/naming/tool-name.ts` | `assertUniqueProviderNames()` 的存在理由之一，就是防止外部来源的工具名遮蔽内置工具 |

明确**没有**的东西：

- 没有 MCP 客户端，也没有任何 `initialize` / `tools/list` / `tools/call` 的实现。
- 没有把外部工具注册进 ToolRegistry 的代码。
- 没有启动、监督或回收 MCP 服务进程的代码。
- **没有 Tauri 命令。** `IPC_COMMANDS` 里没有任何 MCP 相关命令，因此界面既不能配置 MCP 服务器，也看不到它们——`McpServersRepository` 目前只被数据库测试使用。
- 没有任何信任评审流程，`allowedTools` 与 `trustLevel` 只是被存储，没有被读取。

## 握手会报告什么

`initialize` 的结果里包含四项能力：

```json
{
  "streaming": true,
  "toolCalling": true,
  "cancellation": true,
  "mcp": false
}
```

`system.describe` 另外会报告已注册工具数量、可用策略 ID、已实现的方法表，以及 Provider 侧工具名冲突列表。前两项与 MCP 无关；`mcp: false` 是目前唯一与 MCP 有关的对外事实。宿主会在启动时检查协议版本与工具名冲突，并拒绝发布握手不通过的 sidecar。

## 未来接入必须遵守的约束

以下每一条都来自仓库中已经存在的机制。违反它们不会报错，只会让权限模型、审计或命名空间出现空洞。

### 1. 外部工具必须先变成 Yukinal 的工具声明

```text
MCP Server → MCP Client → 适配器 → ToolRegistry（外部工具变成 ToolDeclaration）
          → Permission Engine → ExecutionTicket → 执行 → 事件与审计
```

远端服务不能替代本地的名称、风险、schema、目标或授权决策。适配器负责把外部工具翻译成 `Tool` 实现，此后它走的是与内置工具完全相同的路径：`ToolRegistry.register()` 会校验名称合法性、描述非空、`timeoutMs > 0`，并在注册时就拒绝不合规的声明。

### 2. 命名空间与名称冲突

- 内部名称使用 `mcp.<serverId>.<tool>`，再按 [ADR 0004](../../../../docs/adr/0004-tool-name-mapping.md) 映射成 Provider 侧的双下划线形式。
- 名称必须满足内部规则：各段匹配 `^[a-z][a-z0-9]*(-[a-z0-9]+)*$`、至少两段、不含双下划线。这意味着 `serverId` 与外部工具名都必须先被规范化，不能直接沿用远端拼写。
- 映射后的 Provider 名称不得超过 64 字符。
- 注册前必须运行冲突检查。外部工具可能带来 `docker__get` 这样的拼写，它与内部的 `docker.get` 会投影到同一个 Provider 名称——这类遮蔽必须在注册期被拒绝，而不是等到调用时才发现。

### 3. 风险等级由本地决定

- 外部工具默认至少为 `medium`，不采信远端声明的风险等级。远端可以声称自己是只读的，但那只是远端的一面之词。
- `allowedTools` 为空表示**什么都不自动信任**：用户需要先看到工具描述，再逐个选择启用；`trustLevel` 为 `unreviewed` 的工具不应被自动注册。
- 危险档位的规则不变：`high`/`critical` 永远不会被自动批准，也永远不会被会话授权记住。

### 4. 输入、输出与超时由本地强制

- 工具的输入 schema 必须由本地 Zod schema 定义（`Tool.input`），模型看到的 JSON Schema 由它派生。远端声明只能作为翻译来源，不能直接成为校验依据；校验失败一律得到 `invalid_input`。
- `timeoutMs` 必须为正数。注册表会把工具执行与取消信号做竞速，忽略取消的工具会被判为超时或已取消——外部进程因此不能把一次运行永久挂住。
- 输出在回传模型、界面和审计之前要经过敏感值清理，摘要上限 4000 字符，命中敏感标记时整体替换为「已省略」。
- `plan` 与 `readonly` 运行模式下，任何非只读档位的外部工具调用都会被权限引擎直接拒绝。

### 5. 目标必须在本地解析

- 每次调用都携带一个已解析的 `ToolTarget`（host、serverId、workspaceId、environment），工具不得自己重新猜测服务器。
- 如果外部工具需要访问远端资源，它必须经由宿主的 `host.tool.execute` 路径：宿主只接受 `host: "remote"` 且 `serverId` 以 `srv_` 开头的目标，并核对目标环境与工作区归属。
- 远端描述文本不能改变目标，也不能把一次调用重定向到别的服务器。

### 6. 描述文本一律视为不可信数据

工具描述、服务端返回的文本和错误信息都可能包含针对模型的指令。它们可以作为工具说明展示给用户，但不得直接拼接进系统提示词充当可信上下文。

### 7. 进程生命周期仍然归 Rust

`McpServerConfig` 里有 `command` 与 `args`，说明 `stdio` 传输意味着要派生进程。当前架构里**唯一**派生进程的地方是 Rust 宿主（见 [ADR 0001](../../../../docs/adr/0001-agent-runtime-as-node-sidecar.md) 与 [ADR 0008](../../../../docs/adr/0008-sidecar-launch-and-lifecycle.md)）：sidecar 只通过 `host.tool.execute` 请求宿主执行操作。因此：

- 由 sidecar 自行派生 MCP 服务进程会打破「Rust 拥有进程」这条边界，需要先写一条新的 ADR 说明理由与该进程的回收、日志和崩溃语义。
- 无论由谁派生，都必须有明确的单实例、超时、回收与退出诊断规则；孤儿进程是不可接受的。
- `http` 传输意味着 sidecar 需要主动对外发起网络请求。目前没有任何出站网络策略，接入前必须定义允许的目标范围、TLS 要求与失败语义。

### 8. 不要暗示已经可用

- 不要添加「兼容调用」的占位实现、假的工具列表或只读的界面入口，让用户以为 MCP 已经连上。
- 只要 `capabilities.mcp` 仍是 `false`，文档与界面就应该照实说。能力报告必须是事实，而不是意图。

### 9. 数据库变更走新迁移

`mcp_servers` 表在初始迁移里就已建立。已发布的迁移不得修改，新增字段必须追加一条新迁移（`crates/database/src/schema.rs` 顶部的规则），并同步更新 Rust 模型与 `McpServerConfig`。

### 10. 新增界面要补齐契约

如果将来要让用户配置 MCP 服务器，需要同时增加：`IPC_COMMANDS` 中的命令、`packages/shared/src/schemas/` 下的 Zod schema、`fixtures/ipc/` 下的契约样例（Rust 与 TypeScript 两侧都会解析它们），以及 Rust 侧的命令与仓库方法。只加一侧会让这个契约在运行时不成立。
