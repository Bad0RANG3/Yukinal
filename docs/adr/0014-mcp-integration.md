# ADR 0014 MCP 接入：宿主独占进程与目录，agent 只注册与调用

Status: Accepted（Context 里「17 个集成测试」这个计数不准：`crates/core/tests/mcp_stdio.rs` 现在是 18 个 `#[tokio::test]`，跑在同一个已提交的 Node fixture 上 —— 这个数字在本记录写下时就已经错了，所以它仍然只证明「与 fixture 的行为一致」，不证明任何真实第三方服务器的兼容性。其余描述与今天的代码一致，原文不改写）
Date: 2026-09-12

## Context

`apps/agent/src/mcp/README.md` 之前的第一句话是「**MCP 尚未实现。** 本目录下没有代码」。这句话说对了一半：`crates/core/src/mcp/` 里其实**已经有**一个完整的 MCP stdio 客户端（进程、`initialize` 握手、`tools/list`、`tools/call`、每请求超时、退出记录、stderr 尾部，以及 `crates/core/tests/mcp_stdio.rs` 里针对一个已提交的 Node fixture 的 17 个集成测试），`mcp_servers` 表与 `McpServersRepository` 也在，但**没有任何东西把它们连起来**：

- 没有任何 Tauri 命令读写 `mcp_servers`，所以界面既不能配置也不能看到 MCP 服务器（`McpServersRepository` 当时只被一个数据库测试用）；
- 没有任何代码把 `McpToolDescriptor` 变成 `ToolRegistry` 里的工具；
- sidecar 报告 `capabilities.mcp: false`，而这个值必须保持真实；
- 2026 年新增的 `McpSupervisor` 是**唯一**派生 MCP 服务进程的地方，但它没有调用者。

同时，三条既有约束决定了接入只能有一个形状：

1. **ADR 0001 / 0008：进程归 Rust。** sidecar 只通过 `host.tool.execute` 请求宿主做事，自己不派生进程。让 Node 侧去 `spawn` 一个 MCP 服务器会直接打破这条边界。
2. **ADR 0005：Permission Engine 是唯一的执行授权决策者。** 外部工具必须先变成一个 `ToolDeclaration` 才可能被执行，而声明里的 `risk` 决定了它是自动批准还是要用户批准。
3. **ADR 0004：内部工具名用点号分段。** `mcp.<服务器>.<工具>` 需要两段都符合 `^[a-z][a-z0-9]*(-[a-z0-9]+)*$`，而远端工具的拼写不受我们控制。

还有一件必须在设计期就承认的事：**MCP 没有静态工具表。** 想知道一个服务器有哪些工具，唯一的办法是把它的进程起起来问它。这让「读取工具列表」成为一个有副作用的操作，而它恰好又是 sidecar 启动时最想要的东西。

## Decision

1. **进程、配置、目录、执行全部由宿主负责；agent 只做两件事：注册与调用。**
   - 新命令 `mcp_server_list` / `mcp_server_save` / `mcp_server_delete` / `mcp_server_start` / `mcp_server_stop`（`apps/desktop/src-tauri/src/commands/mcp.rs`）读写 `mcp_servers` 表并驱动 `McpSupervisor`。`mcp_server_list` **不启动任何进程**：看一眼列表不该派生第三方程序。
   - 目录通过既有的宿主 RPC 通道新增方法 `host.mcp.catalog`（`HOST_METHODS.mcpCatalog`）交给 sidecar，**没有第二条执行通道**。
   - `host.tool.execute` 里 `mcp.` 前缀的工具名由 `commands/host.rs` 分流到 `mcp::execute`，与 `docker.*` / `filesystem.*` 走同一条路、同一套取消令牌。

2. **目录是唯一的「读操作带副作用」，边界写在三个条件里。**
   它只碰 `enabled` 且传输为 stdio 的行；只启动 supervisor **从未管过** 的 id；总预算 `CATALOG_START_BUDGET = 4s`。超过预算的服务器这一次不出现，报告为 `timeout`。

3. **崩掉的 MCP 服务器永不自动重启。** 已跟踪但已死的句柄只被**报告**（`McpFailureCode::Exited` + 退出码 + `not restarted` + 「显式启动」的下一步），三个方向都不例外：目录不拉起它，`mcp_server_start` 是唯一的重启路径，而一次失败的调用给出的是 `transport` + `retryable: false` 与 `detail.restarted: false`。理由与 ADR 0010 对 sidecar 的取舍同源，但结论相反：sidecar 的自动恢复是**有界**的，MCP 服务器则一次都不自动恢复 —— 重启一个第三方工具服务器可能把上一次的副作用再执行一遍，而宿主无法判断那是否安全。

4. **`http` 传输在类型层面就不存在。** `McpStdioConfig::from_server_config` 拒绝它（`McpError::TransportNotImplemented`）；保存一个 `http` 行会被拒绝且**不写库**；表里已经存在的 `http` 行在列表、目录与启动路径上都会得到同一句完整理由（出站网络策略不存在），并且界面上「启动」是禁用的。**没有任何静默 no-op 的路径。** 界面的传输选项里也没有 `http` —— 摆一个保存时才失败的选项等于用一个下拉框骗人。

5. **MCP 工具一律声明 `risk: "critical"`，并且不采信服务器的自我描述。**
   `McpToolDescriptor` 里根本没有风险字段或注解，适配器也不读：MCP 的工具注解（`readOnlyHint` 之类）是**服务器对自己的一面之词**，让它降低自己的风险档位就是把授权决策交给被授权方。`critical` → 档位 `dangerous` → 权限引擎在**任何** `permissionMode` 下都要求用户逐项批准，`grantSession()` 拒绝记住它，`plan` / `readonly` 运行模式直接拒绝。

6. **远端声明是文档，不是契约。** 本地输入 schema 是 `z.record(z.string(), z.unknown())`（「一个对象，内容由服务器校验」）；服务器的 `inputSchema` 被追加到**描述**里（有长度上限），因为拿模型或服务器能影响的文档去校验模型输入是一个带额外步骤的验证漏洞。

7. **来源可以分辨，而且是双向钉住的。** `Tool` 新增 `origin?: ToolOrigin`，`ToolRegistry.register()` 用它填 `ToolDeclaration.origin`（缺省仍是 `{ kind: "builtin" }`），并强制两条不变量：`mcp.` 前缀的工具必须声明 `origin: mcp`，`origin: mcp` 的工具必须落在 `mcp.` 命名空间里。`agent.tool_call` / `agent.tool_result` 事件带上 `origin`（可选，老 sidecar 不发），所以审计能回答「这次调用是内置工具还是五分钟前某个第三方服务器声明的工具」。

8. **`capabilities.mcp` 只在真的有目录时为真。** agent 在 stdio RPC 起来**之后**异步取一次目录（宿主只有在握手完成后才会转发 sidecar 请求，见 `crates/core/src/supervisor.rs`），拿不到就记一行日志并继续，**不重试**；该标志的取值必须来自注册表的实际内容，而不是意图。

9. **`allowedTools` 与 `trustLevel` 仍然只被存储。** 保存它们的界面不存在，`mcp_server_save` 保留原值。注册一个工具**不是**一次授信：每个 MCP 调用都要用户逐项批准（第 5 条），所以「注册了」与「被信任」在这套模型里已经是两件事。

## Consequences

**收益**

- 根 README 的「MCP 没有接入」不再是事实：可以配置、启动、观察、调用一个 stdio MCP 服务器，工具出现在 `agent.list_tools` 与模型可用的工具列表里。
- 「Rust 拥有进程」在 MCP 上仍然成立，而且**只有一个** MCP 客户端实现（`crates/core/src/mcp/`），宿主与 agent 两侧没有第二套。
- 一次 MCP 调用的归属在审计里仍然能落到具体服务器，但落点是**工具名**：`tool_executions.tool_name` 是 `mcp.<服务器>.<工具>`（ADR 0004 的命名约定）。失败带着退出码与「不会自动重启」的说明，而不是一个挂住。
- `http` 被拒绝这件事只有一个来源（`McpStdioConfig`），界面、命令、目录都复述同一句话。

**成本与限制**

- **取消不撤回副作用。** `host.tool.execute` 的取消让宿主不再等待，但 MCP 的线上协议里没有「取消一次 `tools/call`」这个方法（`crates/core/src/mcp/wire.rs` 只有三个方法），所以服务进程那边的调用可能继续跑完。这是如实记录的缺陷，不是被忽略的细节。
- **目录请求会在启动时派生第三方进程。** MCP 没有静态工具表，这是无法绕开的：一个启用的、从未启动过的 stdio 服务器会在 agent 取目录时被起起来。因此 agent 启动多了一次最多 4 秒的等待，超时的服务器本次会话里就没有工具。
- **`capabilities.mcp` 在 `initialize` 时可能仍为 `false`。** 目录必须在 stdio RPC 之后取（否则宿主的握手会等一个永远不来的请求），所以 `initialize` 回答时目录通常还没到。它必须诚实地反映「此刻注册表里没有 MCP 工具」，而 `agent.list_tools` / `system.describe.toolCount` 是实时视图。要让它在 `initialize` 时为 `true`，需要把 `crates/core/src/supervisor.rs` 里事件泵的启动移到 `sidecar::handshake()` 之前 —— 那属于 ADR 0006/0008 的地盘，本次不做。
- **没有信任评审流程。** `trustLevel` 永远停在 `unreviewed`，`allowedTools` 永远是空表，因此每个 MCP 工具永远是 `critical`。这是当前唯一诚实的状态：任何「降低某个 MCP 工具风险」的能力都必须先有一个能让用户看过描述再决定的流程，而它还不存在。
- **孤儿进程在宿主退出路径上被显式处理，但 `kill_on_drop` 不是保证。** `RunEvent::ExitRequested` 里除了 `supervisor.stop()` 还会调用 `AppState.mcp.shutdown_all()`，因为 MCP 服务器是我们自己派生出来的第三方进程，和 sidecar 一样必须被回收；只靠 `kill_on_drop` 不够 —— 那是个 `Drop` 实现，而 Windows 上强杀宿主不会执行任何 `Drop`。任务管理器强杀仍然会留下子进程，这一点没有消除，只是不再依赖 `Drop` 一定会跑。
- **结构化归属没有落库。** `agent.tool_call` / `agent.tool_result` 事件上的 `origin` 是真的，但 `commands/mod.rs` 落库时没有读它，所以审计行里只有工具名这一条线索。把 `origin.kind` / `origin.serverId` 写进 `tool_executions` 需要改 `crates/core/src/ipc.rs` 的事件结构、`crates/database/src/models.rs` 和一条迁移，本次没有做。
- **没有真实的第三方 MCP 服务器被验证过。** 全部测试跑在 `crates/core/tests/fixtures/mcp-server.js` 这个已提交的 Node fixture 上；没有网络，也没有 `npx`。

## Alternatives considered

- **让 sidecar 直接派生 MCP 进程。** 被否决：违反 ADR 0001/0008，而且会把「谁回收孤儿进程、谁记录退出码、stderr 归谁」变成两个实现。sidecar 已经通过 `host.tool.execute` 请求宿主做事，MCP 没有理由例外。
- **给 MCP 工具单独一条执行方法（`host.mcp.call`）。** 被否决：那会造出第二条执行通道，取消、审计、目标校验、错误词汇都要写两遍。`mcp.` 前缀分流让 MCP 与内置工具共用同一条路径。
- **在 `initialize` 之前同步取目录，让 `capabilities.mcp` 一定为真。** 被否决：宿主在握手完成前不会转发 sidecar 请求（事件泵在 `sidecar::handshake` 之后才 spawn），这会死锁到握手超时。要改就得动 `crates/core/src/supervisor.rs` 的启动顺序，那需要单独一条 ADR。
- **采信 MCP 的工具注解来定风险档位。** 被否决：那是让服务器决定自己的授权级别。注解可以作为**展示**信息（未来），但不能作为档位来源。
- **崩溃后自动重启一次。** 被否决：与 ADR 0010 不同，MCP 服务器是用户自己引入的第三方工具，一次自动重启可能重复执行一个已经产生副作用的调用，而宿主没有任何办法知道上一次调用做到了哪一步。
- **支持 `http` 传输并在失败时提示「暂不可用」。** 被否决：出站网络策略还不存在，这不是「暂时不可用」而是一条尚未定义的安全边界。类型层面拒绝它，比运行期拒绝更早、更难绕过。
