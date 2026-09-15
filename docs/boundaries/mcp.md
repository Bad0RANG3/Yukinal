# 边界：外部工具（MCP）

**MCP 已接入，而执行边界与最初写下的约束一致。** `apps/agent/src/mcp/` 里有代码，而且只有两类：把宿主交来的目录变成一个 `Tool`（`tool.ts`），以及把它注册进 `ToolRegistry`（`catalog.ts`）。**这个目录里没有任何进程、网络客户端或 MCP 协议实现**——stdio 进程、Streamable HTTP 会话、`initialize` 握手、`tools/list`、`tools/call`、超时、退出记录、stderr 尾部都在 `crates/core/src/mcp/`，配置命令在 `apps/desktop/src-tauri/src/commands/mcp.rs`，交给 sidecar 的目录与工具名解析在 `crates/core/src/mcp/catalog.rs`。

## 接入形状（九条决定）

1. **进程、配置、目录、执行全部由宿主负责，agent 只做注册与调用。** 新命令 `mcp_server_list`/`mcp_server_save`/`mcp_server_delete`/`mcp_server_start`/`mcp_server_stop` 读写 `mcp_servers` 表并驱动 supervisor，其中 **`mcp_server_list` 不启动任何进程**——看一眼列表不该派生第三方程序。目录通过既有宿主 RPC 通道新增方法 `host.mcp.catalog` 交给 sidecar，**没有第二条执行通道**；`host.tool.execute` 里 `mcp.` 前缀的工具名由 `commands/host.rs` 分流，与 `docker.*`/`filesystem.*` 走同一条路、同一套取消令牌。
2. **目录是唯一的「读操作带副作用」。** MCP 没有静态工具表——想知道一个服务器有哪些工具，唯一的办法是启动 stdio 进程或建立 HTTP 会话问它。边界写在三个条件里：只碰 `enabled` 且通过传输配置校验的行；只启动 supervisor **从未管过**的 id；总预算 4 秒，超预算的服务器这一次不出现、报告为 `timeout`。
3. **崩掉的 stdio MCP 进程会按有界退避自动重启。** 每次退出都留下 `lastExit`，恢复尝试写入 `restart.attempt/maxAttempts/exhausted`，状态始终能回答「它为什么重启过、预算还剩多少」。恢复只重建进程并重新读取 `tools/list`，**绝不重放刚中断的调用**；预算耗尽后停止，`mcp_server_start` 可以显式重置。HTTP 会话失效不会假装存在一个待恢复的进程，用户显式启动时会建立新会话。
4. **HTTP 只连接显式 URL，凭据不进入 URL，也不重复发送。** 远程 endpoint 必须使用 HTTPS，明文 HTTP 仅限回环地址；拒绝 URL 内嵌用户名/密码、查询参数和 fragment，也关闭 redirect。HTTP transport 实现 MCP Streamable HTTP 的 `Mcp-Session-Id`、协议版本头、JSON/SSE POST 回包、可选 GET 事件流、`notifications/cancelled` 与 DELETE，并可配置最多 16 条有序静态认证头。另一种认证方式支持手填 issuer，或从 `WWW-Authenticate` / RFC 9728 protected-resource metadata 自动发现 issuer，再走 RFC 8414 discovery，并用两条流程之一拿到第一个 token：authorization code + PKCE S256（浏览器回调绑定随机 `127.0.0.1` 端口），或 RFC 8628 device code（不需要任何本地回调：界面显示服务器给的 `user_code`、优先打开 `verification_uri_complete`，客户端按 `interval` 轮询，`authorization_pending` 与 `slow_down` 都只是继续等，取消/到期/拒绝各说各的原因）。客户端认证三选一：`none`（公共客户端，只发送 `client_id`）、`client_secret_post`（`client_id` + `client_secret` 走请求体）、`client_secret_basic`（`client_id:client_secret` 走 `Authorization` 头，请求体里不再出现这两个参数，RFC 6749 §2.3 的一次一种方式）；code exchange、device authorization、每次轮询与 refresh **都走同一条认证路径**，所以不存在「换到 token 之后就不会续期」的写法。两条流程最后落在**同一个** token bundle 上：token 与 client secret 只存系统凭据库，过期前自动刷新，401 强制刷新一次；issuer、client id、scopes、流程与客户端认证方式都属于身份，改了就要重新授权。client id 留空时通过 RFC 7591 注册一个 `token_endpoint_auth_method: none` 的公共客户端，注册的 grant 与所选流程一致，且注册响应里带回 secret 时整次注册会被拒绝而不是把公共客户端变成保密的。header 名、OAuth 元数据与凭据引用留在 SQLite，secret/token 不进入设置响应；按请求动态签名仍未支持（[ADR 0016](../adr.md#adr-0016mcp-streamable-http-只在宿主内实现且凭据不进入-url)）。
5. **MCP 工具一律声明 `critical`，并且不采信服务器的自我描述。** 描述符里根本没有风险字段，适配器也不读：MCP 的工具注解（`readOnlyHint` 之类）是**服务器对自己的一面之词**，让它降低自己的风险档位就是把授权决策交给被授权方。`critical` → 档位 `dangerous` → 权限引擎在**任何**批准方式下都要求用户逐项批准，会话授权拒绝记住它，只读与计划模式直接拒绝（[权限档位：dangerous](../risk-tiers/dangerous.md)）。
6. **远端声明是文档，不是契约。** 本地输入 schema 是「一个对象，内容由服务器校验」；服务器的 `inputSchema` 被追加到**描述**里（有长度上限），因为拿模型或服务器能影响的文档去校验模型输入，是一个带额外步骤的验证漏洞。描述文本一律视为不可信数据：可以展示，不能执行，不能当作校验依据。
7. **来源可分辨且双向钉住。** `Tool` 新增可选的 `origin`，registry 用它填工具的来源声明，并强制两条不变量：`mcp.` 前缀的工具必须声明来源为 mcp，声明来源为 mcp 的工具必须落在 `mcp.` 命名空间里——外部来源无法冒充内置工具。`agent.tool_call`/`agent.tool_result` 事件因此能回答「这次调用是内置工具还是五分钟前某个第三方服务器声明的工具」。
8. **`capabilities.mcp` 只在真的有目录时为真。** agent 在 stdio RPC 起来**之后**异步取一次目录（宿主只有在握手完成后才会转发 sidecar 请求），拿不到就记一行日志并继续、**不重试**；该标志取值必须来自注册表实际内容而不是意图。
9. **工具注册需要一次显式审核。** 设置页在服务器运行后展示它实际声明的工具；用户勾选 `allowedTools` 并标记 `trustLevel: reviewed` 后，目录才把这些工具交给 Agent。审核只决定“是否暴露给模型”，**不会降低风险等级**：每个调用仍然是 `critical` 并逐项批准。

## 运行期需要知道的事实

- **取消是通知，不是回滚。** 取消会发送标准 `notifications/cancelled` 并让宿主停止等待；协议允许服务器忽略它，已经发生的副作用也不会被撤回。
- **设备码的等待没有后台轮询任务。** 轮询写在那个等待中的 `mcp_oauth_connect` 请求里，而不是一个游离的定时器：`mcp_oauth_cancel` 让它立刻返回并说明是用户取消，`expires_in` 到期与服务器回 `expired_token` 是两种不同的结束语，轮询次数另有硬上限 —— 即使等待它的窗口已经不在了，它也不会活得比 `expires_in` 更久。服务器没在元数据里声明 `device_authorization_endpoint` 时直接拒绝设备码流程，不悄悄改用浏览器回调；`slow_down` 按 RFC 加 5 秒，同时仍受 `expires_in` 上限约束。
- **宿主退出时显式关闭 MCP。** 退出路径与 sidecar 一样调用 `shutdown_all()`：stdio 先关 stdin、再按预算升级到强杀；HTTP 先取消 GET stream 与在途请求，再发送 DELETE。一台服务器关不掉不影响其余几台。`kill_on_drop` 是 Windows 强杀宿主时不会执行的 `Drop` 实现，因此任务管理器强杀仍可能留下 stdio 子进程，这一点没有消除。
- **结构化归属没有落库。** 事件上的 `origin` 是真的，但落库时没读它，审计行里只有工具名这一条线索。
- **服务器发起的请求不会被应答。** 我们声明零能力，所以 `notifications/tools/list_changed` 只被记成一条诊断，目录不会因此刷新；`sampling` / `roots` 这类请求不会被回答。

## 十条接入约束逐条对照

| 约束 | 满足方式 |
| --- | --- |
| 外部工具必须先变成 Yukinal 的工具声明 | 适配器只产出 `Tool`，此后与内置工具走同一条路径：权限、票据、超时竞赛、取消、trace、审计都无特殊待遇 |
| 命名空间与名称冲突 | 强制 `mcp.` 前缀与来源声明互相匹配；段在宿主侧规范化，两条 id 撞车时只让一个进目录；Provider 侧名称冲突在注册期拒绝 |
| 风险等级由本地决定 | 一律 `critical`，不读远端注解；档位不是「默认 `medium`」而是最严的那一档，见下面的取舍说明 |
| 输入、输出与超时由本地强制 | 本地 schema 只校验「是一个对象」；远端 `inputSchema` 只作文档；输出摘要 4000 字符上限；每次调用有超时（工具侧 45 秒比宿主侧 30 秒长，好让先放弃的是宿主），且 `retry.maxAttempts` 为 1 |
| 目标必须在本地解析 | `ToolTarget` 由调用侧给出，远端文本不参与解析。`mcp.` 分流发生在目标校验之前，因为一次 MCP 调用打给的是本机派生的 stdio 进程或本机配置的 HTTP endpoint，不是某台 SSH 服务器 |
| 描述文本一律视为不可信数据 | 只搬运与展示，不执行、不校验、不拼进系统提示词 |
| 进程与网络生命周期仍然归 Rust | stdio 只有 `crates/core/src/mcp/` 派生进程；HTTP 也只在同一模块持有 client、session、GET stream 与取消任务，agent 侧一行 `spawn`、一次 `fetch` 都没有；按 id 去重；stdio 崩溃后按有界退避重建，HTTP 失效后由用户显式新建会话 |
| 不要暗示已经可用 | 能力报告来自注册表实际内容，不是意图；没有目录就不显示工具数量 |
| 数据库变更走新迁移 | `allowed_tools` / `trust_level` 与 HTTP 认证列都由版本化迁移补齐；HTTP secret 本身只存在系统凭据库，SQLite 只保存有序 header 名与各自的 `keychain://` 引用 |
| 新增界面要补齐契约 | 六个命令、Zod schema、`IPC_COMMANDS` / `IPC_SCHEMAS`、fixture，以及 `apps/desktop/src/lib/mcp.ts` 与 `McpSettings.tsx` 都同步补齐 |

**风险等级始终由本地决定，且不因审核降低。** 约束只是“至少 `medium`”，实现选择了最严的 `critical`：审核决定工具能否进入注册表，权限引擎决定每次调用是否需要用户点头；后者永远需要。这样既不把 MCP 变成不可用，也不把服务器的自我描述变成授权依据。

**验证范围。** 确定性门禁使用自带的 Node fixture（`crates/core/tests/fixtures/mcp-server.js`）覆盖 stdio，并用本机临时 HTTP server（`crates/core/tests/mcp_http.rs`）在真实 socket 上覆盖会话头、JSON/SSE 回包、GET 事件流、取消与 DELETE。OAuth 的两条流程由一个本机授权服务器 fixture（`apps/desktop/src-tauri/src/commands/mcp/oauth.rs` 的测试模块）在真实 socket 上覆盖：授权码的 discovery/动态注册/回调/刷新，以及设备码的 `pending`、`slow_down`、拒绝、过期、取消、缺 `device_authorization_endpoint` 与轮询间隔；真实第三方授权服务器上的这些流程仍未被验证过。另有显式启用的网络矩阵：通过 `npx` 启动官方 `@modelcontextprotocol/server-everything`，用官方 TypeScript SDK 的 JSON 回包模式覆盖另一种 HTTP 形状，并接入独立 DuckDuckGo / Fetch server 的 stdio；握手、工具目录、工具调用、JSON/SSE 回包、GET stream 与 DELETE 的结果和上游错误记录在 [外部验证运行手册](../external-validation.md)。第三方 MCP 实现很多，这些 fixture 与抽样矩阵不能代表全部兼容性。
