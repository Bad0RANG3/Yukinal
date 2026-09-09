# MCP 适配器边界

MCP 目前未实现。sidecar handshake 的 capability `mcp` 为 `false`，仓库没有启动 MCP 客户端、注册 MCP tool 或执行 MCP server 配置的代码。这里的文档用于固定未来实现的安全边界，不代表当前功能可用。

## 计划中的数据流

```text
MCP Server → MCP Client → Yukinal Adapter → ToolRegistry → Agent
```

适配器接入后必须把外部工具转换成 Yukinal 的 Tool declaration，并继续经过现有 Permission Engine 和 ExecutionTicket 流程：

| 关注点 | 规则 |
| --- | --- |
| 名称 | 使用 `mcp.<serverId>.<tool>`，再按 ADR 0004 映射到 Provider 名称 |
| 风险 | 外部工具默认至少为 `medium`，不能采信远端自报风险 |
| 输入和输出 | 本地 schema、超时、大小上限和取消由 Yukinal 强制 |
| 目标 | 由本地会话和当前工作区解析，不能由远端描述覆盖 |
| 权限 | 仍由 Permission Engine 唯一决定 |
| 描述文本 | 视为不可信数据，不能直接拼进系统提示词 |

在实现完成前，不要通过“兼容调用”或 UI 假装 MCP 已连接。新增 MCP 协议或配置时，需要补充信任模型、进程回收、网络访问控制和审计策略。
