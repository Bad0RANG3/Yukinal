# MCP 适配器边界

MCP 尚未实现。sidecar handshake 的 `mcp` capability 当前为 `false`，仓库中没有启动 MCP client、注册 MCP 工具或执行 MCP server 配置的代码。本文件规定未来接入时必须遵守的安全边界，不代表 MCP 已可使用。

## 目标数据流

```text
MCP Server → MCP Client → Yukinal Adapter → ToolRegistry → Agent
```

适配器必须将外部工具转换成 Yukinal 的工具声明，再让其照常经过 Permission Engine 和 `ExecutionTicket` 流程。远端 server 不能替代本地的名称、风险、schema、目标或授权决策。

| 关注点 | 本地约束 |
| --- | --- |
| 名称 | 使用 `mcp.<serverId>.<tool>`，再按 ADR 0004 映射为 Provider 名称。 |
| 风险 | 外部工具默认至少为 `medium`；不采信远端声明的风险等级。 |
| 输入与输出 | 由 Yukinal 强制本地 schema、超时、大小上限和取消语义。 |
| 目标 | 由当前会话与工作区在本地解析，不能由远端描述改写。 |
| 权限 | 仍由 Permission Engine 作出唯一的最终决策。 |
| 描述文本 | 一律视为不可信数据，不得直接拼接到系统提示词。 |

实现 MCP 前，必须补充信任模型、进程回收、网络访问控制和审计策略。不要以兼容调用或 UI 占位的方式暗示 MCP 已连接或可执行。
