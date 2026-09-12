# ADR 0004 工具名称在 Provider 边界映射

Status: Accepted（成本一节「未来接入外部工具（例如 MCP）时，外来名称必须先被命名空间化并经过冲突检查才能注册」已经落地：[ADR 0014](0014-mcp-integration.md) 接入 MCP 后，`ToolRegistry.register()` 强制 `mcp.` 前缀与 `origin: { kind: "mcp" }` 互相匹配，外部来源无法冒充内置工具，命名冲突仍在注册期由 `assertUniqueProviderNames()` 发现。点号内部名、双下划线 Provider 名、单一映射实现与「不猜测未知工具名」四条不变）
Date: 2026-09-09

## Context

工具需要一个稳定的、能表达归属的名字。Yukinal 采用点号分层：`docker.ps`、`docker.logs`、`filesystem.read`、`system.echo`。这个名字要出现在审计表、活动记录、界面卡片、日志和用户看到的审批提示里——也就是说，它必须是人能读、能搜索、能长期稳定的标识。

但多数 function calling 网关对函数名的字符集与长度有更严格的限制，常见规则是 `^[a-zA-Z0-9_-]{1,64}$`：点号不被接受，或者被网关**静默改写**。静默改写的后果比拒绝更严重：模型回传的是一个我们没见过的名字，如果这个名字进了内部日志和审计，调用链就再也无法和真实工具对应上；如果它被当作未知工具丢弃，用户会看到一次无法解释的失败。

## Decision

- **内部一律使用点号名称。** ToolRegistry 注册、Permission Engine、审计表 `tool_executions.tool_name`、活动记录、IPC 事件和界面显示全部使用 `docker.ps` 这种拼写。
- **发给 Provider 的名称改为双下划线。** `docker.ps` 变成 `docker__ps`。
- **映射只有一个实现。** `packages/shared/src/naming/tool-name.ts` 定义规则，`packages/provider-sdk/src/name-index.ts` 的 `createProviderNameIndex()` 生成双向映射与工具声明列表。agent loop 用它把模型返回的 Provider 名称换回内部名称，再交给 ToolRegistry；任何工具实现、Provider 实现或界面代码都不允许手写这套转换。
- **名称规则是可判定的。** 内部名称必须是 `namespace.action` 形式：至少两段，每段匹配 `^[a-z][a-z0-9]*(-[a-z0-9]+)*$`，并且整体不得包含双下划线。`ToolRegistry.register()` 在注册时就拒绝不合规的名字，因此不会有非法名称进入后续流程。
- **映射是可逆的，并且冲突会被显式发现。** 因为内部名称的各段不允许下划线，`__` 与 `.` 的互换不会产生歧义。`createProviderNameIndex()` 在构建索引时检查两个内部名称是否映射到同一个 Provider 名称，命中即抛出错误；`assertUniqueProviderNames()` 用同样宽松的投影方式做注册期检查，专门用于发现外部来源（例如未来接入的外部工具）带来的外来拼写（`docker__get` 与 `docker.get` 会撞在一起）。
- **无法映射的名称不会被猜测。** 如果模型返回了一个我们从未声明过的工具名，loop 会返回一个「未知工具」的错误结果给模型，而不是模糊匹配到某个相似名称上。宿主在握手阶段也会检查 `system.describe` 返回的冲突列表：**只要 sidecar 报告存在名称冲突，Rust 就拒绝发布这个运行中的 sidecar**，而不是带着一个可能对不上号的工具表继续工作。

## Consequences

**收益**

- 审计、日志和界面使用同一套稳定名称，不受 Provider 命名限制影响；同一个工具在任何端点下都叫同一个名字。
- 冲突在注册期或握手期暴露，不会延迟到某次真实调用时才表现为「工具莫名失败」。
- 规则集中在一个文件里，新增工具不需要重新理解网关限制；新增 Provider 也不需要重新实现转换。

**成本**

- 多了一层间接：调试时必须记住模型看到的 `docker__ps` 与内部 `docker.ps` 是同一个东西。这一层刻意只存在于两个文件里，就是为了让这种心智负担有明确边界。
- 名称受到长度约束：64 字符的上限是 Provider 名称的约束，因此内部名称也不应无限加长（`toProviderToolName()` 会在超长时抛错）。
- 未来接入外部工具（例如 MCP）时，外来名称必须先被命名空间化并经过冲突检查才能注册；这一步不能省略，否则外部来源可以遮蔽内置工具。

## Alternatives considered

- **内部也使用双下划线。** 会让命名风格取决于上游网关的限制，且 `docker__ps` 在审计与界面里明显更难读。否决。
- **每个 Provider 各自做名称转换。** 会把同一套规则复制到每个 Provider 实现里，冲突检查也会随之分裂。否决。
- **在网关拒绝点号时自动降级为下划线。** 会让映射依赖运行时的失败路径，而映射一旦不确定，审计就不可信。否决。
- **用哈希作为 Provider 侧名称。** 完全消除长度与字符集问题，但模型无法从名字推断工具用途，且排障时可读性归零。否决。
